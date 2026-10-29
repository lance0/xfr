//! End-to-end push coverage, also run by the normal all/no-default-features CI jobs.

use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use xfr::protocol::{Direction, Protocol};
use xfr::serve::{Server, ServerConfig};

static SERVER_PORT: AtomicU16 = AtomicU16::new(26000);

async fn gateway(statuses: Vec<u16>) -> (String, tokio::task::JoinHandle<Vec<(String, String)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/api/v1/import/prometheus/",
        listener.local_addr().unwrap()
    );
    let task = tokio::spawn(async move {
        let mut pushes = Vec::new();
        for status in statuses {
            let (stream, _) = timeout(Duration::from_secs(20), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            reader.read_line(&mut request).await.unwrap();
            assert!(request.starts_with("POST "));
            let mut length = None;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).await.unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
            let length = length.expect("push must have a Content-Length");
            assert!(length < 1_000_000);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).await.unwrap();
            reader
                .get_mut()
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            pushes.push((request, String::from_utf8(body).unwrap()));
        }
        pushes
    });
    (url, task)
}

async fn server(push_gateway_url: Option<String>) -> (u16, tokio::task::JoinHandle<()>) {
    // Keep server TCP+UDP ports outside the ephemeral range used by concurrent clients.
    let port = SERVER_PORT.fetch_add(1, Ordering::SeqCst);
    let task = tokio::spawn(async move {
        Server::new(ServerConfig {
            port,
            push_gateway_url,
            no_mdns: true,
            ..Default::default()
        })
        .run()
        .await
        .unwrap();
    });
    timeout(Duration::from_secs(5), async {
        loop {
            if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    (port, task)
}

fn sample(body: &str, name: &str) -> f64 {
    body.lines()
        .find_map(|line| line.strip_prefix(&format!("{name} ")))
        .unwrap_or_else(|| panic!("missing {name}: {body}"))
        .parse()
        .unwrap()
}

async fn client(port: u16, gateway: &str, host: &str, flags: &[&str]) -> std::process::Output {
    timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_xfr"))
            .args([
                host,
                "-p",
                &port.to_string(),
                "-t",
                "1",
                "-P",
                "2",
                "-b",
                "2M",
                "--json",
                "--no-update-check",
                "--push-gateway",
                gateway,
            ])
            .args(flags)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn finalized_snapshot_values_are_not_recomputed() {
    let (url, pushes) = gateway(vec![202]).await;
    let result = serde_json::from_value(serde_json::json!({
        "id": "snapshot", "bytes_total": 4242, "duration_ms": 2000,
        "throughput_mbps": 7.25,
        "streams": [{"id": 9, "bytes": 123, "throughput_mbps": 0.75, "retransmits": 4}],
        "tcp_info": {"rtt_us": 1250, "rtt_var_us": 100, "cwnd": 65536, "retransmits": 4},
        "udp_stats": {"packets_sent": 100, "packets_received": 97, "lost": 3,
                      "lost_percent": 3.0, "jitter_ms": 1.5, "out_of_order": 0}
    }))
    .unwrap();
    xfr::output::push_gateway::PushGatewayClient::new(url)
        .unwrap()
        .push_test_result(&result)
        .await
        .unwrap();
    let pushes = pushes.await.unwrap();
    let body = &pushes[0].1;
    assert_eq!(sample(body, "xfr_bytes_total"), 4242.0);
    assert_eq!(sample(body, "xfr_duration_seconds"), 2.0);
    assert_eq!(sample(body, "xfr_throughput_mbps"), 7.25);
    assert_eq!(sample(body, "xfr_tcp_rtt_microseconds"), 1250.0);
    assert_eq!(sample(body, "xfr_udp_packets_lost"), 3.0);
    assert_eq!(sample(body, "xfr_udp_lost_percent"), 3.0);
    assert_eq!(sample(body, "xfr_udp_jitter_ms"), 1.5);
    assert!(body.contains("xfr_stream_bytes_total{test_id=\"snapshot\",stream_id=\"9\"} 123\n"));
    assert!(
        body.contains("xfr_stream_throughput_mbps{test_id=\"snapshot\",stream_id=\"9\"} 0.75\n")
    );
}

#[tokio::test]
async fn client_and_server_pushes_are_distinct_and_match_final_results() {
    let (url, pushes) = gateway(vec![202; 12]).await;
    let (port, server) = server(Some(url.clone())).await;
    let mut results = Vec::new();
    for (protocol, direction) in [
        (Protocol::Tcp, Direction::Upload),
        (Protocol::Udp, Direction::Upload),
        (Protocol::Udp, Direction::Download),
        (Protocol::Udp, Direction::Bidir),
        (Protocol::Quic, Direction::Download),
        (Protocol::Quic, Direction::Bidir),
    ] {
        let mut flags = Vec::new();
        match protocol {
            Protocol::Udp => flags.push("-u"),
            Protocol::Quic => flags.push("-Q"),
            Protocol::Tcp => {}
        }
        match direction {
            Direction::Download => flags.push("-R"),
            Direction::Bidir => flags.push("--bidir"),
            Direction::Upload => {}
        }
        let output = client(port, &url, "127.0.0.1", &flags).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: xfr::protocol::TestResult = serde_json::from_slice(&output.stdout).unwrap();
        assert!(result.bytes_total > 0);
        results.push(result);
    }
    let pushes = timeout(Duration::from_secs(10), pushes)
        .await
        .unwrap()
        .unwrap();
    for result in results {
        let prefix = format!(
            "POST /api/v1/import/prometheus/metrics/job/xfr_test_{}/role/",
            result.id
        );
        let (_, body) = pushes
            .iter()
            .find(|(request, _)| {
                request.starts_with(&format!("{prefix}client/server/127.0.0.1:{port} "))
            })
            .expect("client push with server label");
        assert_eq!(sample(body, "xfr_bytes_total"), result.bytes_total as f64);
        assert_eq!(sample(body, "xfr_throughput_mbps"), result.throughput_mbps);
        assert_eq!(
            sample(body, "xfr_duration_seconds"),
            result.duration_ms as f64 / 1000.0
        );
        for stream in &result.streams {
            assert!(body.contains(&format!(
                "xfr_stream_bytes_total{{test_id=\"{}\",stream_id=\"{}\"}} {}\n",
                result.id, stream.id, stream.bytes as f64
            )));
        }
        if let Some(udp) = &result.udp_stats {
            assert_eq!(sample(body, "xfr_udp_packets_lost"), udp.lost as f64);
            assert_eq!(
                sample(body, "xfr_udp_packets_received"),
                udp.packets_received as f64
            );
        }
        assert!(
            pushes
                .iter()
                .any(|(request, _)| request
                    .starts_with(&format!("{prefix}server/client/127.0.0.1 "))),
            "server push with normalized client IP"
        );
    }
    server.abort();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn client_cli_environment_and_config_precedence() {
    let (url, pushes) = gateway(vec![202; 3]).await;
    let (port, server) = server(None).await;
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("xfr");
    std::fs::create_dir(&config_dir).unwrap();
    for source in ["config", "env", "cli"] {
        let config_url = if source == "config" {
            &url
        } else {
            "invalid-config-url"
        };
        std::fs::write(config_dir.join("config.toml"), format!("[client]\npush_gateway = {config_url:?}\n[server]\npush_gateway = \"invalid-server-url\"\n")).unwrap();
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_xfr"));
        cmd.env("XDG_CONFIG_HOME", dir.path())
            .env_remove("XFR_PUSH_GATEWAY")
            .args([
                "127.0.0.1",
                "-p",
                &port.to_string(),
                "-t",
                "1",
                "-b",
                "2M",
                "--json",
                "--no-update-check",
            ])
            .kill_on_drop(true);
        if source != "config" {
            cmd.env(
                "XFR_PUSH_GATEWAY",
                if source == "env" {
                    &url
                } else {
                    "invalid-env-url"
                },
            );
        }
        if source == "cli" {
            cmd.args(["--push-gateway", &url]);
        }
        let output = timeout(Duration::from_secs(15), cmd.output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: xfr::protocol::TestResult = serde_json::from_slice(&output.stdout).unwrap();
        assert!(result.bytes_total > 0);
    }
    assert_eq!(
        timeout(Duration::from_secs(5), pushes)
            .await
            .unwrap()
            .unwrap()
            .len(),
        3
    );
    server.abort();
}

#[tokio::test]
async fn failed_pushes_retry_without_failing_the_test() {
    let (url, pushes) = gateway(vec![500; 3]).await;
    let url =
        url.replacen("http://", "http://mesh-user:gateway-secret@", 1) + "?token=gateway-secret";
    let (port, server) = server(None).await;
    let output = client(port, &url, "127.0.0.1", &[]).await;
    assert!(output.status.success());
    let result: xfr::protocol::TestResult = serde_json::from_slice(&output.stdout).unwrap();
    assert!(result.bytes_total > 0);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("not confirmed"));
    assert!(
        !stderr.contains("mesh-user") && !stderr.contains("gateway-secret"),
        "{stderr}"
    );
    let pushes = pushes.await.unwrap();
    assert_eq!(pushes.len(), 3);
    assert!(pushes.windows(2).all(|pair| pair[0] == pair[1]));
    server.abort();
}

#[tokio::test]
async fn failed_test_does_not_push() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = unavailable.local_addr().unwrap().port();
    drop(unavailable);
    let output = client(
        port,
        &format!("http://{}", listener.local_addr().unwrap()),
        "127.0.0.1",
        &["--connect-timeout", "1s"],
    )
    .await;
    assert!(!output.status.success());
    assert!(
        timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn concurrent_ipv4_and_ipv6_clients_keep_their_own_labels() {
    let (url, pushes) = gateway(vec![202; 4]).await;
    let (port, server) = server(Some(url.clone())).await;
    let (v4, v6) = tokio::join!(
        client(port, &url, "127.0.0.1", &[]),
        client(port, &url, "[::1]", &[])
    );
    assert!(v4.status.success() && v6.status.success());
    let v4: xfr::protocol::TestResult = serde_json::from_slice(&v4.stdout).unwrap();
    let v6: xfr::protocol::TestResult = serde_json::from_slice(&v6.stdout).unwrap();
    assert_ne!(v4.id, v6.id);
    let pushes = pushes.await.unwrap();
    for (result, host, peer) in [(v4, "127.0.0.1", "127.0.0.1"), (v6, "[::1]", "::1")] {
        let prefix = format!(
            "POST /api/v1/import/prometheus/metrics/job/xfr_test_{}/role/",
            result.id
        );
        assert!(pushes.iter().any(|(request, _)| {
            request.starts_with(&format!("{prefix}client/server/{host}:{port} "))
        }));
        assert!(
            pushes
                .iter()
                .any(|(request, _)| request.starts_with(&format!("{prefix}server/client/{peer} ")))
        );
    }
    server.abort();
}

#[tokio::test]
async fn invalid_gateway_is_rejected_before_connecting() {
    for url in [
        "not-a-url",
        "file:///tmp/metrics",
        "ftp://localhost/metrics",
    ] {
        let output = client(1, url, "127.0.0.1", &[]).await;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Connecting to"));
    }
}

async fn stalled_gateway_preserves_result(interrupt: bool) {
    let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://mesh-user:gateway-secret@{}/?token=gateway-secret",
        gateway.local_addr().unwrap()
    );
    let (port, server) = server(None).await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("result.json");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_xfr"))
        .args([
            "127.0.0.1",
            "-p",
            &port.to_string(),
            "-t",
            "1",
            "-b",
            "2M",
            "--json",
            "--no-update-check",
            "--push-gateway",
            &url,
            "--output",
        ])
        .arg(&path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // Hold the HTTP connection open without a response, after the measurement completes.
    let (_connection, _) = timeout(Duration::from_secs(10), gateway.accept())
        .await
        .unwrap()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let result: xfr::protocol::TestResult = timeout(Duration::from_secs(2), async {
        let mut text = String::new();
        loop {
            assert!(stdout.read_line(&mut text).await.unwrap() > 0);
            if let Ok(result) = serde_json::from_str(&text) {
                break result;
            }
        }
    })
    .await
    .expect("final JSON must be flushed before gateway delivery finishes");
    assert!(result.bytes_total > 0);
    let saved: xfr::protocol::TestResult =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.id, result.id);
    assert_eq!(saved.bytes_total, result.bytes_total);
    #[cfg(unix)]
    if interrupt {
        // The PID belongs to this still-running child, which is waiting for the HTTP reply.
        assert_eq!(
            unsafe { libc::kill(child.id().unwrap() as libc::pid_t, libc::SIGINT) },
            0
        );
    }
    let limit = if interrupt { 3 } else { 8 };
    let output = timeout(Duration::from_secs(limit), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert!(
        !stderr.contains("mesh-user") && !stderr.contains("gateway-secret"),
        "{stderr}"
    );
    assert!(stderr.contains("not confirmed"), "{stderr}");
    assert!(
        !stderr.contains("partial results") && !stderr.contains("before any data"),
        "{stderr}"
    );
    if !interrupt {
        assert!(stderr.contains("timed out after 5 seconds"), "{stderr}");
    }
    assert_eq!(
        std::fs::read(&path).unwrap(),
        xfr::output::output_json(&result).unwrap().as_bytes()
    );
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_during_metrics_delivery_keeps_completed_json_and_file() {
    stalled_gateway_preserves_result(true).await;
}

#[tokio::test]
async fn stalled_metrics_delivery_has_a_total_deadline_and_keeps_result() {
    stalled_gateway_preserves_result(false).await;
}
