//! End-to-end push coverage, also run by the normal all/no-default-features CI jobs.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use xfr::client::{Client, ClientConfig};
use xfr::protocol::{Direction, Protocol};
use xfr::serve::{Server, ServerConfig};

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
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
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
        .await;
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
        let result = timeout(
            Duration::from_secs(15),
            Client::new(ClientConfig {
                port,
                protocol,
                direction,
                duration: Duration::from_secs(1),
                streams: 2,
                bitrate: Some(2_000_000),
                push_gateway_url: Some(url.clone()),
                ..Default::default()
            })
            .run(None),
        )
        .await
        .unwrap()
        .unwrap();
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
    let (port, server) = server(None).await;
    let result = timeout(
        Duration::from_secs(15),
        Client::new(ClientConfig {
            port,
            duration: Duration::from_secs(1),
            bitrate: Some(2_000_000),
            push_gateway_url: Some(url),
            ..Default::default()
        })
        .run(None),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.bytes_total > 0);
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
    assert!(
        Client::new(ClientConfig {
            port,
            connect_timeout: Some(Duration::from_secs(1)),
            push_gateway_url: Some(format!("http://{}", listener.local_addr().unwrap())),
            ..Default::default()
        })
        .run(None)
        .await
        .is_err()
    );
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
    let config = ClientConfig {
        port,
        duration: Duration::from_secs(1),
        bitrate: Some(2_000_000),
        push_gateway_url: Some(url),
        ..Default::default()
    };
    let ipv4 = Client::new(config.clone());
    let ipv6 = Client::new(ClientConfig {
        host: "[::1]".into(),
        ..config
    });
    let (v4, v6) = timeout(Duration::from_secs(15), async {
        tokio::join!(ipv4.run(None), ipv6.run(None))
    })
    .await
    .unwrap();
    let v4 = v4.unwrap();
    let v6 = v6.unwrap();
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
