//! Integration tests for xfr

use std::process::Command;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use tokio::time::timeout;

use xfr::client::{Client, ClientConfig, ZerocopyMode};
use xfr::protocol::{Direction, Protocol};
use xfr::serve::{Server, ServerConfig};

// Use different ports for each test to avoid conflicts
static PORT_COUNTER: AtomicU16 = AtomicU16::new(16000);

#[test]
fn test_cli_diff_invalid_threshold_cannot_report_ok() {
    let dir = tempfile::tempdir().unwrap();
    let baseline_path = dir.path().join("baseline.json");
    let current_path = dir.path().join("current.json");
    for (path, throughput) in [(&baseline_path, 100.0), (&current_path, 1.0)] {
        let result = serde_json::json!({
            "id": "threshold-test", "bytes_total": 1000, "duration_ms": 1000,
            "throughput_mbps": throughput, "streams": [],
        });
        std::fs::write(path, result.to_string()).unwrap();
    }
    for threshold in ["NaN", "inf", "-1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
            .arg("diff")
            .arg(&baseline_path)
            .arg(&current_path)
            .arg(format!("--threshold={threshold}"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("threshold"));
        assert!(output.stdout.is_empty());
    }
    for threshold in ["0", "5", "12.5"] {
        let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
            .arg("diff")
            .arg(&baseline_path)
            .arg(&current_path)
            .arg(format!("--threshold={threshold}"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stdout).contains("REGRESSION DETECTED"));
    }
}

fn get_test_port() -> u16 {
    PORT_COUNTER.fetch_add(10, Ordering::SeqCst)
}

async fn start_test_server(port: u16) -> tokio::task::JoinHandle<()> {
    let config = ServerConfig {
        port,
        one_off: false, // Keep running for tests
        max_duration: None,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        ..Default::default()
    };

    tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    })
}

#[tokio::test]
async fn test_cli_json_output_is_parseable_without_quiet() {
    let port = get_test_port();
    let server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    for mode in ["--json", "--json-stream"] {
        let output = timeout(
            Duration::from_secs(15),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_xfr"))
                .args([
                    "127.0.0.1",
                    "-p",
                    &port.to_string(),
                    "-u",
                    "-b",
                    "2M",
                    "-t",
                    "2",
                    "--no-tui",
                    mode,
                ])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if mode == "--json" {
            let result: xfr::protocol::TestResult = serde_json::from_slice(&output.stdout)
                .expect("summary stdout must contain exactly one JSON document");
            assert!(result.bytes_total > 0);
        } else {
            let stdout = String::from_utf8(output.stdout).unwrap();
            let records: Vec<serde_json::Value> = stdout
                .lines()
                .map(|line| serde_json::from_str(line).expect("every stream line is a JSON object"))
                .collect();
            assert!(
                records.len() >= 2,
                "stream must retain live interval records"
            );
            assert!(
                records
                    .iter()
                    .any(|record| record.get("elapsed_secs").is_some())
            );
            assert!(records.last().unwrap()["bytes_total"].as_u64().unwrap() > 0);
        }
    }
    server.abort();
}

#[tokio::test]
async fn test_tcp_single_stream() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        // The default mode: exercises sendfile on Linux and the silent
        // regular-write fallback elsewhere (macOS CI).
        zerocopy: ZerocopyMode::Auto,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "Test should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "Test should succeed: {:?}", result);

    let result = result.unwrap();
    // Server reports bytes based on what it tracked, which may be 0 if stats aren't linked
    // The test passes if we got a valid result structure back
    assert!(result.duration_ms > 0, "Should have duration");

    // Unidirectional tests should NOT populate the per-direction split — those
    // fields are reserved for bidir where the combined total would be misleading.
    assert!(
        result.bytes_sent.is_none(),
        "unidir result should not populate bytes_sent"
    );
    assert!(
        result.bytes_received.is_none(),
        "unidir result should not populate bytes_received"
    );
}

#[tokio::test]
async fn test_tcp_multi_stream() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 4,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "Test should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "Test should succeed");

    let result = result.unwrap();
    assert_eq!(result.streams.len(), 4, "Should have 4 streams");
}

#[tokio::test]
async fn test_connection_refused() {
    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port: 19999, // Unlikely to be in use
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(1),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = client.run(None).await;

    assert!(result.is_err(), "Should fail to connect");
}

#[tokio::test]
async fn test_tcp_download() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Download,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "Test should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "Download test should succeed: {:?}", result);

    let result = result.unwrap();
    assert!(result.duration_ms > 0, "Should have duration");
}

#[tokio::test]
async fn test_tcp_bidir() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Bidir,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "Bidir test should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "Bidir test should succeed: {:?}", result);

    let result = result.unwrap();
    assert!(result.duration_ms > 0, "Should have duration");

    // Bidir tests should populate the per-direction fields (issue #56).
    assert!(
        result.bytes_sent.is_some(),
        "bidir result should have bytes_sent populated"
    );
    assert!(
        result.bytes_received.is_some(),
        "bidir result should have bytes_received populated"
    );
    assert!(
        result.throughput_send_mbps.is_some(),
        "bidir result should have throughput_send_mbps populated"
    );
    assert!(
        result.throughput_recv_mbps.is_some(),
        "bidir result should have throughput_recv_mbps populated"
    );
}

#[tokio::test]
async fn test_udp_upload() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: Some(100_000_000), // 100 Mbps
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "UDP test should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "UDP test should succeed: {:?}", result);

    let result = result.unwrap();
    assert!(result.duration_ms > 0, "Should have duration");
}

/// Issue #213: both peers must use the requested size, including reverse
/// and multistream bidir. Jumbo payloads catch silent receive truncation.
#[tokio::test]
async fn test_udp_custom_payload_sizes() {
    let port = get_test_port();
    let server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    for size in [1368, 8192] {
        for direction in [Direction::Upload, Direction::Download, Direction::Bidir] {
            let config = ClientConfig {
                port,
                protocol: Protocol::Udp,
                streams: 2,
                duration: Duration::from_secs(1),
                direction,
                bitrate: Some(2_000_000),
                udp_payload_size: Some(size),
                ..Default::default()
            };
            let result = timeout(Duration::from_secs(10), Client::new(config).run(None))
                .await
                .unwrap()
                .unwrap();
            assert!(result.bytes_total > 0, "size={size}, direction={direction}");
            let stats = result.udp_stats.expect("receiver UDP stats");
            assert!(
                stats.packets_received > 0,
                "size={size}, direction={direction}"
            );
            assert_eq!(stats.packet_size, Some(u32::from(size)));
            if direction == Direction::Bidir {
                assert!(result.bytes_sent.unwrap() > 0);
                assert!(result.bytes_received.unwrap() > 0);
            }
        }
    }
    server.abort();
}

/// A late jumbo packet belongs to the stray lane on the wildcard socket;
/// it must be drained without truncation or starving QUIC's receive task.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shared_udp_socket_drains_jumbo_strays() {
    let port = get_test_port();
    let server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    for _ in 0..4 {
        socket
            .send_to(&vec![0; 8192], ("127.0.0.1", port))
            .await
            .unwrap();
    }
    let client = Client::new(ClientConfig {
        port,
        protocol: Protocol::Quic,
        duration: Duration::from_secs(1),
        bitrate: Some(1_000_000),
        ..Default::default()
    });
    let result = timeout(Duration::from_secs(10), client.run(None))
        .await
        .unwrap()
        .unwrap();
    assert!(result.bytes_total > 0);
    server.abort();
}

#[tokio::test]
async fn test_udp_payload_size_rejects_older_server() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use xfr::protocol::ControlMessage;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(read);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let hello = ControlMessage::server_hello_with_capabilities(vec!["udp".into()]);
        write
            .write_all(format!("{}\n", hello.serialize().unwrap()).as_bytes())
            .await
            .unwrap();
        line.clear();
        assert_eq!(
            reader.read_line(&mut line).await.unwrap(),
            0,
            "client must refuse before TestStart"
        );
    });
    let config = ClientConfig {
        port,
        protocol: Protocol::Udp,
        direction: Direction::Download,
        udp_payload_size: Some(1368),
        ..Default::default()
    };
    let error = timeout(Duration::from_secs(5), Client::new(config).run(None))
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("udp_payload_size_v1"), "{error}");
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

/// The TUI settings modal can switch a `-u -l` run to TCP or QUIC. The
/// size must be ignored there, not sent to a server that rejects it.
#[tokio::test]
async fn test_udp_payload_size_ignored_for_tcp_and_quic() {
    let port = get_test_port();
    let server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    for protocol in [Protocol::Tcp, Protocol::Quic] {
        let config = ClientConfig {
            port,
            protocol,
            duration: Duration::from_secs(1),
            bitrate: Some(10_000_000),
            udp_payload_size: Some(1368),
            ..Default::default()
        };
        let result = timeout(Duration::from_secs(10), Client::new(config).run(None))
            .await
            .unwrap();
        assert!(result.is_ok(), "{protocol}: {result:?}");
    }
    server.abort();
}

#[test]
fn test_udp_payload_size_validates_library_config() {
    for size in [0, 15, 65508, u16::MAX] {
        assert!(
            ClientConfig {
                protocol: Protocol::Udp,
                udp_payload_size: Some(size),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    for size in [16, 1368, 65507] {
        assert!(
            ClientConfig {
                protocol: Protocol::Udp,
                udp_payload_size: Some(size),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
    }
    assert!(
        ClientConfig {
            protocol: Protocol::Udp,
            mtu_probe: true,
            udp_payload_size: Some(1368),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}

/// `--probe-mtu` end to end (issue #64): full control-channel handshake,
/// probe exchange against the server's responder, cancel, and the report
/// riding home on the result. Loopback passes every size, so both
/// directions must converge to the probe ceiling.
#[tokio::test]
async fn test_mtu_probe() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 1,
        duration: Duration::from_secs(60), // server-side safety deadline only
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: true,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(30), client.run(None)).await;

    assert!(result.is_ok(), "probe should complete well under deadline");
    let result = result.unwrap().expect("probe should succeed");

    let report = result.mtu_probe.expect("result must carry a probe report");
    assert_eq!(
        report.forward_max_payload,
        Some(xfr::probe::MAX_PROBE_PAYLOAD),
        "loopback passes every size forward"
    );
    assert_eq!(
        report.reverse_max_payload,
        Some(xfr::probe::MAX_PROBE_PAYLOAD),
        "loopback passes every size in reverse"
    );
    assert!(!report.sizes.is_empty());
}

#[tokio::test]
async fn test_udp_download() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    // Give server more time to start on slower CI runners
    tokio::time::sleep(Duration::from_millis(500)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Download,
        bitrate: Some(100_000_000), // 100 Mbps
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(20), client.run(None)).await;

    assert!(result.is_ok(), "UDP download should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "UDP download should succeed: {:?}", result);
}

#[tokio::test]
async fn test_udp_bidir() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    // Give server more time to start on slower CI runners
    tokio::time::sleep(Duration::from_millis(500)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Bidir,
        bitrate: Some(100_000_000), // 100 Mbps
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(20), client.run(None)).await;

    assert!(result.is_ok(), "UDP bidir should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "UDP bidir should succeed: {:?}", result);

    let result = result.unwrap();
    assert!(result.duration_ms > 0, "Should have duration");
}

#[tokio::test]
async fn test_udp_multi_stream() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 4,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: Some(100_000_000), // 100 Mbps total
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "UDP multi-stream should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "UDP multi-stream should succeed: {:?}",
        result
    );

    let result = result.unwrap();
    assert_eq!(result.streams.len(), 4, "Should have 4 streams");
}

#[tokio::test]
async fn test_multi_client_concurrent() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Spawn two clients concurrently
    let config1 = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let config2 = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client1 = Client::new(config1);
    let client2 = Client::new(config2);

    let (result1, result2) = tokio::join!(
        timeout(Duration::from_secs(10), client1.run(None)),
        timeout(Duration::from_secs(10), client2.run(None))
    );

    assert!(result1.is_ok(), "Client 1 should complete");
    assert!(result2.is_ok(), "Client 2 should complete");

    let result1 = result1.unwrap();
    let result2 = result2.unwrap();

    assert!(result1.is_ok(), "Client 1 should succeed: {:?}", result1);
    assert!(result2.is_ok(), "Client 2 should succeed: {:?}", result2);
}

// ============================================================================
// Security Integration Tests
// ============================================================================

use xfr::acl::AclConfig;
use xfr::auth::AuthConfig;
use xfr::rate_limit::RateLimitConfig;

async fn start_secure_server(
    port: u16,
    psk: Option<String>,
    rate_limit: Option<u32>,
    allow: Vec<String>,
    deny: Vec<String>,
) -> tokio::task::JoinHandle<()> {
    let config = ServerConfig {
        port,
        one_off: false,
        max_duration: None,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        auth: AuthConfig { psk },
        acl: AclConfig {
            allow,
            deny,
            file: None,
        },
        rate_limit: RateLimitConfig {
            max_per_ip: rate_limit,
            window_secs: 60,
        },
        ..Default::default()
    };

    tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    })
}

async fn start_preset_server(
    port: u16,
    preset_allowed_clients: Vec<String>,
) -> tokio::task::JoinHandle<()> {
    let config = ServerConfig {
        port,
        one_off: false,
        max_duration: None,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        auth: AuthConfig { psk: None },
        acl: AclConfig {
            allow: vec![],
            deny: vec![],
            file: None,
        },
        rate_limit: RateLimitConfig {
            max_per_ip: None,
            window_secs: 60,
        },
        preset_allowed_clients: Some(preset_allowed_clients),
        ..Default::default()
    };

    tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    })
}

#[tokio::test]
async fn test_psk_auth_success() {
    let port = get_test_port();
    let psk = "test-secret-key".to_string();
    let _server = start_secure_server(port, Some(psk.clone()), None, vec![], vec![]).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: Some(psk),
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "Test should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "PSK auth should succeed: {:?}", result);
}

#[tokio::test]
async fn test_psk_auth_failure() {
    let port = get_test_port();
    let _server = start_secure_server(
        port,
        Some("server-secret".to_string()),
        None,
        vec![],
        vec![],
    )
    .await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: Some("wrong-secret".to_string()),
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "Test should complete (not timeout)");
    let result = result.unwrap();
    assert!(result.is_err(), "PSK auth should fail with wrong key");
}

#[tokio::test]
async fn test_psk_auth_missing_client_key() {
    let port = get_test_port();
    let _server = start_secure_server(
        port,
        Some("server-secret".to_string()),
        None,
        vec![],
        vec![],
    )
    .await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None, // No PSK provided
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "Test should complete (not timeout)");
    let result = result.unwrap();
    assert!(
        result.is_err(),
        "Should fail when server requires auth but client has no PSK"
    );
}

#[tokio::test]
async fn test_acl_allow() {
    let port = get_test_port();
    // Allow localhost only
    let _server =
        start_secure_server(port, None, None, vec!["127.0.0.1/32".to_string()], vec![]).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "Test should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "Localhost should be allowed: {:?}", result);
}

#[tokio::test]
async fn test_rate_limit() {
    let port = get_test_port();
    // Allow only 1 concurrent test per IP
    let _server = start_secure_server(port, None, Some(1), vec![], vec![]).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Start first client (should succeed)
    let config1 = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(60),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client1 = Client::new(config1.clone());
    let handle1 = tokio::spawn(async move { client1.run(None).await });

    // Give first client time to connect
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Second client should be rate limited
    let config2 = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client2 = Client::new(config2);
    let result2 = timeout(Duration::from_secs(5), client2.run(None)).await;

    // Second connection should be rejected (rate limited - connection dropped).
    // The server drops the connection before hello, so the client should get
    // an error result back promptly.
    assert!(
        matches!(result2, Ok(Err(_))),
        "second concurrent connection should be rejected: {result2:?}"
    );

    // First client is still running; abort it rather than waiting 60s.
    handle1.abort();
    let result1 = handle1.await;
    assert!(result1.is_err(), "First client task should be aborted");
}

// ============================================================================
// QUIC Integration Tests
// ============================================================================

#[tokio::test]
async fn test_quic_upload() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    // Give server time to start (including QUIC endpoint)
    tokio::time::sleep(Duration::from_millis(300)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC test should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "QUIC upload should succeed: {:?}", result);

    let result = result.unwrap();
    assert!(result.duration_ms > 0, "Should have duration");
    assert!(result.bytes_total > 0, "Should have transferred bytes");
}

#[tokio::test]
async fn test_quic_download() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Download,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC download should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "QUIC download should succeed: {:?}", result);

    let result = result.unwrap();
    assert!(result.duration_ms > 0, "Should have duration");
}

#[tokio::test]
async fn test_quic_upload_bitrate_pacing() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Request 20 Mbps pacing on a 2-second test (expected ~5 MB total).
    // Without pacing, loopback QUIC easily achieves > 1000 Mbps (> 250 MB).
    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: Some(20_000_000),
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC paced upload should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "QUIC paced upload should succeed: {:?}",
        result
    );

    let result = result.unwrap();
    assert!(
        result.throughput_mbps > 5.0 && result.throughput_mbps < 50.0,
        "Expected throughput near 20 Mbps, got {:.1} Mbps",
        result.throughput_mbps
    );
}

#[tokio::test]
async fn test_quic_download_bitrate_pacing() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Request 20 Mbps pacing on a 2-second download test.
    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Download,
        bitrate: Some(20_000_000),
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC paced download should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "QUIC paced download should succeed: {:?}",
        result
    );

    let result = result.unwrap();
    assert!(
        result.throughput_mbps > 5.0 && result.throughput_mbps < 50.0,
        "Expected throughput near 20 Mbps, got {:.1} Mbps",
        result.throughput_mbps
    );
}

#[tokio::test]
async fn test_quic_download_bitrate_pacing_multi_stream() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Request 20 Mbps total pacing shared by 2 streams (10 Mbps each) on a
    // 2-second download test. Per-stream pacing divides the configured
    // bitrate evenly (serve.rs / client.rs), so the measured total should
    // still sit near the requested 20 Mbps, not 2x or 4x it.
    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 2,
        duration: Duration::from_secs(2),
        direction: Direction::Download,
        bitrate: Some(20_000_000),
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC paced download should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "QUIC paced download should succeed: {:?}",
        result
    );

    let result = result.unwrap();
    assert_eq!(result.streams.len(), 2, "Should have 2 streams");
    // Upper bound is tighter than the single-stream tests' 50.0 on purpose: if
    // the per-stream division were dropped, two streams would each pace at the
    // full 20 Mbps for a ~41 Mbps aggregate, which 50.0 would happily accept.
    // Anything at or above 2x the budget has to fail for this test to mean
    // anything. Measured aggregate is a flat 20.0 Mbps -- pacing is a hard
    // cumulative cap, so 30.0 is 50% of headroom, not a tight fit.
    assert!(
        result.throughput_mbps > 5.0 && result.throughput_mbps < 30.0,
        "Expected total throughput near 20 Mbps across 2 streams, got {:.1} Mbps",
        result.throughput_mbps
    );
}

#[tokio::test]
async fn test_quic_upload_bitrate_pacing_multi_stream() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Request 20 Mbps total pacing shared by 4 streams (5 Mbps each) on a
    // 2-second test. With per-stream pacing, the client's stream bitrate is
    // bitrate / num_streams, so the aggregate should still track the
    // requested budget rather than summing n full-rate streams.
    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 4,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: Some(20_000_000),
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC paced upload should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "QUIC paced upload should succeed: {:?}",
        result
    );

    let result = result.unwrap();
    assert_eq!(result.streams.len(), 4, "Should have 4 streams");
    assert!(
        result.throughput_mbps > 5.0 && result.throughput_mbps < 50.0,
        "Expected total throughput near 20 Mbps across 4 streams, got {:.1} Mbps",
        result.throughput_mbps
    );
}

#[tokio::test]
async fn test_quic_download_bitrate_pacing_4_streams() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Request 20 Mbps total pacing shared by 4 streams (5 Mbps each) on a
    // 2-second download test.
    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 4,
        duration: Duration::from_secs(2),
        direction: Direction::Download,
        bitrate: Some(20_000_000),
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC paced download should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "QUIC paced download should succeed: {:?}",
        result
    );

    let result = result.unwrap();
    assert_eq!(result.streams.len(), 4, "Should have 4 streams");
    assert!(
        result.throughput_mbps > 5.0 && result.throughput_mbps < 50.0,
        "Expected total throughput near 20 Mbps across 4 streams, got {:.1} Mbps",
        result.throughput_mbps
    );
}

#[tokio::test]
async fn test_quic_multi_stream() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 4,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC multi-stream should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "QUIC multi-stream should succeed: {:?}",
        result
    );

    let result = result.unwrap();
    assert_eq!(result.streams.len(), 4, "Should have 4 streams");
}

#[tokio::test]
async fn test_quic_bidir() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Bidir,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC bidir should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "QUIC bidir should succeed: {:?}", result);

    let result = result.unwrap();
    assert!(result.duration_ms > 0, "Should have duration");
}

#[tokio::test]
async fn test_quic_with_psk() {
    let port = get_test_port();
    let psk = "quic-test-secret".to_string();
    let _server = start_secure_server(port, Some(psk.clone()), None, vec![], vec![]).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: Some(psk),
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC with PSK should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "QUIC with PSK should succeed: {:?}", result);
}

/// A new PSK-configured QUIC server must reject a peer that supports protected
/// control framing but not TLS-exporter channel binding. This exercises the
/// transport-specific fail-closed check over a real QUIC connection.
#[tokio::test]
async fn test_quic_psk_rejects_client_without_channel_binding() {
    use tokio::io::AsyncBufReadExt;
    use xfr::protocol::ControlMessage;

    let port = get_test_port();
    let _server = start_secure_server(
        port,
        Some("quic-downgrade-secret".to_string()),
        None,
        vec![],
        vec![],
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    let endpoint = xfr::quic::create_client_endpoint(addr, None).unwrap();
    let connection = xfr::quic::connect(&endpoint, addr).await.unwrap();
    let (mut send, recv) = connection.open_bi().await.unwrap();
    let mut reader = tokio::io::BufReader::new(recv);

    let hello = ControlMessage::Hello {
        version: "1.1".to_string(),
        client: Some("pre-channel-binding-client".to_string()),
        server: None,
        capabilities: Some(vec!["protected_control_v1".to_string()]),
        auth: None,
        client_nonce: Some("client-nonce".to_string()),
    };
    send.write_all(format!("{}\n", hello.serialize().unwrap()).as_bytes())
        .await
        .unwrap();

    let mut line = String::new();
    match timeout(Duration::from_secs(5), reader.read_line(&mut line)).await {
        Ok(Ok(n)) => {
            if n == 0 {
                return;
            }
            match ControlMessage::deserialize(line.trim()).expect("parse response") {
                ControlMessage::Error { message } => assert!(
                    message.contains("quic_channel_binding_v1"),
                    "expected channel-binding refusal, got: {message}"
                ),
                other => panic!("expected Error refusing unbound QUIC PSK, got: {other:?}"),
            }
        }
        // Quinn may surface the handler's immediate connection close before
        // its best-effort Error frame. Either result is a fail-closed reject;
        // a normal server Hello below would still fail this test.
        Ok(Err(_)) => {}
        Err(_) => panic!("server neither rejected nor responded within 5s"),
    }
}

/// A new PSK-configured QUIC client must likewise refuse an older server that
/// offers protected control but not TLS-exporter channel binding.
#[tokio::test]
async fn test_quic_psk_rejects_server_without_channel_binding() {
    use tokio::io::AsyncBufReadExt;
    use xfr::protocol::ControlMessage;

    let port = get_test_port();
    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    let (cert, key) = xfr::quic::generate_self_signed_cert().unwrap();
    let endpoint =
        xfr::quic::create_server_endpoint(addr, xfr::net::AddressFamily::V4Only, cert, key, None)
            .unwrap();

    let _legacy_server = tokio::spawn(async move {
        let incoming = endpoint.accept().await.expect("accept QUIC connection");
        let connection = incoming.await.expect("complete QUIC handshake");
        let (mut send, recv) = connection.accept_bi().await.expect("accept control stream");
        let mut reader = tokio::io::BufReader::new(recv);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("read client hello");

        let hello = ControlMessage::server_hello_with_auth_and_capabilities(
            "legacy-challenge".to_string(),
            vec!["protected_control_v1".to_string()],
        );
        send.write_all(format!("{}\n", hello.serialize().unwrap()).as_bytes())
            .await
            .expect("send legacy server hello");
        // Keep the connection alive long enough for the client to consume
        // the Hello and make its capability decision.
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let client = Client::new(ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 1,
        duration: Duration::from_secs(1),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: Some("quic-downgrade-secret".to_string()),
        address_family: xfr::net::AddressFamily::V4Only,
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    });
    let error = timeout(Duration::from_secs(5), client.run(None))
        .await
        .expect("client should reject promptly")
        .expect_err("client must refuse an unbound QUIC PSK server");
    assert!(
        error.to_string().contains("quic_channel_binding_v1"),
        "unexpected refusal: {error:#}"
    );
}

/// Positive TCP PSK round-trip. Unlike QUIC (which is TLS-encrypted regardless),
/// the TCP control channel is plaintext without LAN-159, so this exercises the
/// security-critical path end to end: capability negotiation, the server
/// proof-of-PSK, and the AEAD-framed control channel. If the transcript proof or
/// the AEAD framing regresses, this test fails.
#[tokio::test]
async fn test_tcp_with_psk() {
    let port = get_test_port();
    let psk = "tcp-test-secret".to_string();
    let _server = start_secure_server(port, Some(psk.clone()), None, vec![], vec![]).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: Some(psk),
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "TCP with PSK should complete");
    assert!(
        result.unwrap().is_ok(),
        "TCP with PSK should succeed (server proof + AEAD control channel)"
    );
}

/// Fail-closed downgrade resistance: a PSK-configured server must refuse a
/// well-formed client that does not advertise `protected_control_v1` (i.e. a
/// pre-LAN-159 client, or a MITM that stripped the capability), and must say so
/// with a clear error rather than silently proceeding in plaintext.
#[tokio::test]
async fn test_psk_rejects_client_without_protected_control() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use xfr::protocol::ControlMessage;

    let port = get_test_port();
    let psk = "downgrade-test-secret".to_string();
    let _server = start_secure_server(port, Some(psk), None, vec![], vec![]).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    // A valid, newline-terminated Hello that omits protected_control_v1.
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let (reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader);

    let hello = ControlMessage::Hello {
        version: "1.1".to_string(),
        client: Some("legacy-client".to_string()),
        server: None,
        capabilities: Some(vec![]), // no protected_control_v1
        auth: None,
        client_nonce: None,
    };
    writer
        .write_all(format!("{}\n", hello.serialize().unwrap()).as_bytes())
        .await
        .expect("send hello");

    // The server must respond with an error (not hang, not close silently).
    let mut line = String::new();
    let n = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .expect("server should respond, not hang")
        .expect("read response");
    assert!(n > 0, "server closed without an error response");

    match ControlMessage::deserialize(line.trim()).expect("parse response") {
        ControlMessage::Error { message } => assert!(
            message.contains("protected_control_v1"),
            "expected a downgrade-refusal error, got: {message}"
        ),
        other => panic!("expected Error refusing the downgrade, got: {other:?}"),
    }
}

// ============================================================================
// ACL Deny Test
// ============================================================================

#[tokio::test]
async fn test_acl_deny() {
    let port = get_test_port();
    // Deny localhost - should block connection
    let _server =
        start_secure_server(port, None, None, vec![], vec!["127.0.0.0/8".to_string()]).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(5), client.run(None)).await;

    // Connection should be dropped by ACL
    assert!(result.is_ok(), "Test should complete (not timeout)");
    let result = result.unwrap();
    assert!(result.is_err(), "Connection should be denied by ACL");
}

// ============================================================================
// Server preset allowed_clients Tests
// ============================================================================

#[tokio::test]
async fn test_preset_allowed_clients_allows_matching_ip() {
    let port = get_test_port();
    // Allow only localhost through the preset allowlist
    let _server = start_preset_server(port, vec!["127.0.0.1/32".to_string()]).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "Test should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "Localhost should be allowed by preset: {:?}",
        result
    );
}

#[tokio::test]
async fn test_preset_allowed_clients_denies_non_matching_ip() {
    let port = get_test_port();
    // Preset allowlist excludes localhost
    let _server = start_preset_server(port, vec!["192.168.1.0/24".to_string()]).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(5), client.run(None)).await;

    // Connection should be dropped by preset allowlist without hanging
    assert!(result.is_ok(), "Test should complete (not timeout)");
    let result = result.unwrap();
    assert!(
        result.is_err(),
        "Localhost should be denied by preset allowlist"
    );
}

// ============================================================================
// IPv6 Tests
// ============================================================================

#[tokio::test]
async fn test_ipv6_localhost() {
    let port = get_test_port();

    // Start server with dual-stack (default)
    let config = ServerConfig {
        port,
        one_off: false,
        max_duration: None,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        address_family: xfr::net::AddressFamily::DualStack,
        ..Default::default()
    };

    tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Connect via IPv6 localhost
    let config = ClientConfig {
        host: "::1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::V6Only,
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "IPv6 test should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "IPv6 localhost should succeed: {:?}",
        result
    );
}

// ============================================================================
// Infinite Duration Tests (Duration::ZERO with cancel)
// ============================================================================

#[tokio::test]
async fn test_tcp_infinite_duration_with_cancel() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::ZERO, // Infinite
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);

    // Run for 1 second then cancel
    let run_future = client.run(None);
    let cancel_future = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let _ = client.cancel();
    };

    // Race the test against cancellation
    let result = tokio::select! {
        r = run_future => r,
        _ = cancel_future => {
            // Give a moment for cancel to propagate
            tokio::time::sleep(Duration::from_millis(100)).await;
            Err(anyhow::anyhow!("Cancelled"))
        }
    };

    // Test should either complete with result or be cancelled gracefully
    // The important thing is it doesn't hang forever
    let _ = result;
}

#[tokio::test]
async fn test_udp_infinite_duration_with_cancel() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 1,
        duration: Duration::ZERO, // Infinite
        direction: Direction::Upload,
        bitrate: Some(100_000_000), // 100 Mbps to avoid flooding
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);

    // Run for 1 second then cancel
    let run_future = client.run(None);
    let cancel_future = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let _ = client.cancel();
    };

    let result = tokio::select! {
        r = run_future => r,
        _ = cancel_future => {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Err(anyhow::anyhow!("Cancelled"))
        }
    };

    let _ = result;
}

#[tokio::test]
async fn test_dropping_udp_run_closes_progress_channel() {
    let port = get_test_port();
    let server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let client = Client::new(ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        duration: Duration::ZERO,
        bitrate: Some(1_000_000),
        ..Default::default()
    });
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(16);
    let mut run = Box::pin(client.run(Some(progress_tx)));

    // Wait for a full interval so the local data/feedback tasks are running.
    // Feedback-only updates may all be suppressed when fresher TCP interval
    // counters arrive first, but that consumer still owns a progress sender.
    // Retain Client after dropping its run future, as library callers do
    // when wrapping a test in a timeout or selecting a different operation.
    timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                result = &mut run => panic!("infinite run ended before feedback: {result:?}"),
                progress = progress_rx.recv() => {
                    if !progress.expect("progress channel should remain open").udp_feedback_only {
                        break;
                    }
                }
            }
        }
    })
    .await
    .expect("UDP upload should emit an interval");
    drop(run);

    timeout(Duration::from_secs(2), async {
        while progress_rx.recv().await.is_some() {}
    })
    .await
    .expect("dropping a run should stop background progress producers");
    drop(client);
    server.abort();
}

async fn assert_cancel_with_full_progress_queue(protocol: Protocol, direction: Direction) {
    let port = get_test_port();
    let server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let client = Client::new(ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol,
        direction,
        duration: Duration::ZERO,
        bitrate: Some(1_000_000),
        ..Default::default()
    });
    let (progress_tx, progress_rx) = tokio::sync::mpsc::channel(1);
    let mut run = Box::pin(client.run(Some(progress_tx)));

    // Keep polling the client while withholding reads. The first interval
    // fills the queue; the second blocks its producer outside select!.
    // Wait for the fill rather than a fixed sleep: under load the first
    // interval can land well after 1 s.
    let fill = async {
        timeout(Duration::from_secs(15), async {
            while progress_rx.is_empty() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("test should fill the interval queue");
        // One more 1 s interval period so the next send parks on the full queue.
        tokio::time::sleep(Duration::from_millis(1500)).await;
    };
    tokio::select! {
        result = &mut run => panic!("infinite run ended before cancel: {result:?}"),
        () = fill => {}
    }
    assert_eq!(progress_rx.len(), 1, "test should fill the interval queue");
    client.cancel().expect("test should be running");
    let result = timeout(Duration::from_secs(2), &mut run)
        .await
        .expect("cancellation should interrupt a blocked progress send")
        .expect("server should return the cancelled test summary");
    assert!(result.bytes_total > 0);
    // Keep the stalled receiver alive throughout cancellation; dropping it
    // would independently unblock send() and mask the regression.
    drop(progress_rx);
    server.abort();
}

#[tokio::test]
async fn test_tcp_cancel_with_full_progress_queue() {
    assert_cancel_with_full_progress_queue(Protocol::Tcp, Direction::Upload).await;
}

#[tokio::test]
async fn test_quic_cancel_with_full_progress_queue() {
    assert_cancel_with_full_progress_queue(Protocol::Quic, Direction::Upload).await;
}

#[tokio::test]
async fn test_udp_download_cancel_with_full_progress_queue() {
    assert_cancel_with_full_progress_queue(Protocol::Udp, Direction::Download).await;
}

#[tokio::test]
async fn test_quic_infinite_duration_with_cancel() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 1,
        duration: Duration::ZERO, // Infinite
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);

    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(16);
    let run_future = client.run(Some(progress_tx));
    let cancel_future = async {
        // Wait until the test is running so cancellation cannot race the
        // handshake. Keep polling run_future while the result is awaited.
        timeout(Duration::from_secs(5), progress_rx.recv())
            .await
            .expect("QUIC test should emit progress")
            .expect("QUIC test should still be running");
        client
            .cancel()
            .expect("QUIC cancellation should be accepted");
    };

    let (result, ()) = timeout(Duration::from_secs(10), async {
        tokio::join!(run_future, cancel_future)
    })
    .await
    .expect("cancelled QUIC test should finish promptly");
    let result = result.expect("server should return the cancelled QUIC summary");
    assert!(
        result.bytes_total > 0,
        "summary should preserve transferred data"
    );
    assert!(result.duration_ms > 0, "summary should record elapsed time");
}

// ============================================================================
// UDP Address Family Matching Tests (Issue #10 fix)
// ============================================================================

#[tokio::test]
async fn test_udp_ipv4_explicit() {
    // Test that UDP works with explicit IPv4 address
    // This validates the fix for issue #10 (macOS dual-stack compatibility)
    let port = get_test_port();

    // Start server bound to IPv4 only
    let config = ServerConfig {
        port,
        one_off: false,
        max_duration: None,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        address_family: xfr::net::AddressFamily::V4Only,
        ..Default::default()
    };

    tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Client connects to IPv4 address - socket should match server's family
    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: Some(100_000_000),
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::V4Only,
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "UDP IPv4 test should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "UDP with explicit IPv4 should succeed: {:?}",
        result
    );
}

#[tokio::test]
async fn test_udp_ipv6_explicit() {
    // Test that UDP works with explicit IPv6 address
    let port = get_test_port();

    // Start server bound to IPv6 only
    let config = ServerConfig {
        port,
        one_off: false,
        max_duration: None,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        address_family: xfr::net::AddressFamily::V6Only,
        ..Default::default()
    };

    tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Client connects to IPv6 address - socket should match server's family
    let config = ClientConfig {
        host: "::1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: Some(100_000_000),
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::V6Only,
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: Some(1368),
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "UDP IPv6 test should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "UDP with explicit IPv6 should succeed: {:?}",
        result
    );
}

#[tokio::test]
async fn test_udp_cport_dualstack_ipv6_target() {
    // Regression test: --cport in dual-stack mode should work with IPv6 targets.
    // Control TCP bind starts as 0.0.0.0:PORT and must be re-familied to [::]:PORT.
    let port = get_test_port();
    let cport = get_test_port();

    let server_cfg = ServerConfig {
        port,
        one_off: false,
        max_duration: None,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        address_family: xfr::net::AddressFamily::V6Only,
        ..Default::default()
    };

    tokio::spawn(async move {
        let server = Server::new(server_cfg);
        let _ = server.run().await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "::1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: Some(100_000_000),
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::DualStack,
        bind_addr: Some(format!("0.0.0.0:{cport}").parse().unwrap()),
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "UDP --cport IPv6 test should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "UDP dual-stack --cport should succeed with IPv6 target: {:?}",
        result
    );
}

#[tokio::test]
async fn test_tcp_cport_single_stream() {
    let port = get_test_port();
    let cport = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: Some(format!("0.0.0.0:{cport}").parse().unwrap()),
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(
        result.is_ok(),
        "TCP --cport single-stream test should complete"
    );
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "TCP --cport single-stream should succeed: {:?}",
        result
    );
}

#[tokio::test]
async fn test_tcp_cport_multi_stream() {
    let port = get_test_port();
    let cport = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 4,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: Some(format!("0.0.0.0:{cport}").parse().unwrap()),
        sequential_ports: true,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(
        result.is_ok(),
        "TCP --cport multi-stream test should complete"
    );
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "TCP --cport multi-stream should succeed: {:?}",
        result
    );
}

#[tokio::test]
async fn test_tcp_cport_dualstack_ipv6_target() {
    // Regression test: TCP with --cport in dual-stack mode should work with IPv6 targets.
    // Control and data binds start as 0.0.0.0:PORT and must be re-familied to [::]:PORT.
    let port = get_test_port();
    let cport = get_test_port();

    let server_cfg = ServerConfig {
        port,
        one_off: false,
        max_duration: None,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        address_family: xfr::net::AddressFamily::V6Only,
        ..Default::default()
    };

    tokio::spawn(async move {
        let server = Server::new(server_cfg);
        let _ = server.run().await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "::1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::DualStack,
        bind_addr: Some(format!("0.0.0.0:{cport}").parse().unwrap()),
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "TCP --cport IPv6 test should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "TCP dual-stack --cport should succeed with IPv6 target: {:?}",
        result
    );
}

#[tokio::test]
async fn test_quic_cport_dualstack_ipv6_target() {
    // Regression test: QUIC with --cport in dual-stack mode should work with IPv6 targets.
    let port = get_test_port();
    let cport = get_test_port();

    let server_cfg = ServerConfig {
        port,
        one_off: false,
        max_duration: None,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        address_family: xfr::net::AddressFamily::V6Only,
        ..Default::default()
    };

    tokio::spawn(async move {
        let server = Server::new(server_cfg);
        let _ = server.run().await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "::1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 4,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::DualStack,
        bind_addr: Some(format!("0.0.0.0:{cport}").parse().unwrap()),
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC --cport IPv6 test should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "QUIC dual-stack --cport should succeed with IPv6 target: {:?}",
        result
    );
}

#[tokio::test]
async fn test_udp_invalid_sequential_ports_config_fails() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    // This config can only be built through library usage (CLI prevents it).
    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 4,
        duration: Duration::from_secs(1),
        direction: Direction::Upload,
        bitrate: Some(1_000_000),
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: true,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "Invalid config test should complete");
    let result = result.unwrap();
    assert!(result.is_err(), "Invalid sequential config should fail");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("sequential_ports"),
        "Error should mention sequential_ports, got: {}",
        err
    );
}

/// Regression test for UDP bitrate underflow (issue: bitrate/streams=0 became unlimited)
/// With very low bitrate and multiple streams, test should still complete within timeout
/// and transfer a bounded amount of data (not unlimited).
#[tokio::test]
async fn test_udp_bitrate_underflow_regression() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Very low bitrate (1000 bps) with 8 streams
    // Old bug: 1000/8 = 125, but with integer underflow could become 0 (unlimited)
    // Fixed: per-stream bitrate clamped to at least 1 bps
    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 8,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: Some(1000), // 1000 bps total = 125 bps per stream
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    // If bitrate became unlimited, this would timeout or transfer huge amounts
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "UDP low bitrate test should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "UDP with low bitrate should succeed: {:?}",
        result
    );

    // Verify bounded data transfer (at 1000 bps for 2 sec = ~250 bytes max)
    // Allow overhead for UDP headers and minimum packet sizes, but must be well under 1MB
    // (if unlimited, would transfer megabytes at wire speed)
    if let Ok(test_result) = result {
        assert!(
            test_result.bytes_total < 100_000,
            "Low bitrate should transfer bounded data, got {} bytes (unlimited would be MB+)",
            test_result.bytes_total
        );
    }
}

/// Regression test for QUIC IPv6 support (issue #17)
/// Verifies that QUIC clients can connect to IPv6 addresses without needing -6 flag
#[tokio::test]
async fn test_quic_ipv6() {
    let port = get_test_port();

    // Start server bound to IPv6 only
    let config = ServerConfig {
        port,
        one_off: false,
        max_duration: None,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        address_family: xfr::net::AddressFamily::V6Only,
        ..Default::default()
    };

    tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Client connects with DualStack (default) - the bug was that DualStack
    // bound to 0.0.0.0 which couldn't connect to IPv6
    let config = ClientConfig {
        host: "::1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::DualStack, // Default, was broken
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC IPv6 test should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "QUIC with IPv6 and DualStack should succeed: {:?}",
        result
    );
}

/// Test one-off mode with TCP multi-stream
/// Regression test for single-port TCP deadlock in one-off mode
#[tokio::test]
async fn test_tcp_one_off_multi_stream() {
    let port = get_test_port();

    // Start server in one-off mode
    let config = ServerConfig {
        port,
        one_off: true, // This is the key difference
        max_duration: None,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        ..Default::default()
    };

    let server_handle = tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Run client with multiple streams
    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 4, // Multiple streams to test DataHello routing
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(
        result.is_ok(),
        "TCP one-off test should complete (not hang)"
    );
    let result = result.unwrap();
    assert!(result.is_ok(), "TCP one-off should succeed: {:?}", result);

    let result = result.unwrap();
    assert!(result.bytes_total > 0, "Should have transferred bytes");
    assert!(result.duration_ms > 0, "Should have duration");
    assert_eq!(result.streams.len(), 4, "Should have 4 stream results");

    // Server should have exited after the test
    let server_result = timeout(Duration::from_secs(2), server_handle).await;
    assert!(
        server_result.is_ok(),
        "Server should exit after one-off test"
    );
}

/// Test one-off mode with QUIC
#[tokio::test]
async fn test_quic_one_off() {
    let port = get_test_port();

    // Start server in one-off mode
    let config = ServerConfig {
        port,
        one_off: true,
        max_duration: None,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        ..Default::default()
    };

    let server_handle = tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 2,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC one-off test should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "QUIC one-off should succeed: {:?}", result);

    let result = result.unwrap();
    assert!(result.bytes_total > 0, "Should have transferred bytes");
    assert!(result.duration_ms > 0, "Should have duration");
    assert_eq!(result.streams.len(), 2, "Should have 2 stream results");

    // Server should have exited after the test. QUIC teardown can lag the
    // client's result under CPU load; 5 s still catches a server that stays up.
    let server_result = timeout(Duration::from_secs(5), server_handle).await;
    assert!(
        server_result.is_ok(),
        "Server should exit after QUIC one-off test"
    );
}

#[test]
fn test_pause_not_ready_before_test_run() {
    // Client::pause() should return NotReady when no test is running
    // (server_supports_pause defaults to None — not yet negotiated)
    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(1),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);
    assert_eq!(
        client.pause(),
        xfr::client::PauseResult::NotReady,
        "pause() should return NotReady before capability negotiation"
    );
}

/// Check if MPTCP is available on this system
fn mptcp_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        use socket2::{Domain, Protocol, Socket, Type};
        Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::MPTCP)).is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[tokio::test]
async fn test_mptcp_single_stream() {
    if !mptcp_available() {
        eprintln!("MPTCP not available, skipping test");
        return;
    }
    let port = get_test_port();
    let _server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        mptcp: true,
        ..Default::default()
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;
    assert!(result.is_ok(), "MPTCP test should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "MPTCP test should succeed: {:?}", result);
    let result = result.unwrap();
    assert!(result.duration_ms > 0, "Should have duration");
}

#[tokio::test]
async fn test_mptcp_multi_stream() {
    if !mptcp_available() {
        return;
    }
    let port = get_test_port();
    let _server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 4,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        mptcp: true,
        ..Default::default()
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;
    assert!(result.is_ok());
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "MPTCP multi-stream should succeed: {:?}",
        result
    );
    assert_eq!(result.unwrap().streams.len(), 4);
}

#[tokio::test]
async fn test_mptcp_download() {
    if !mptcp_available() {
        return;
    }
    let port = get_test_port();
    let _server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Download,
        mptcp: true,
        ..Default::default()
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_ok());
}

// MPTCP client connects to server (server auto-uses MPTCP if available)
#[tokio::test]
async fn test_mptcp_client_to_tcp_server() {
    if !mptcp_available() {
        return;
    }
    let port = get_test_port();
    let _server = start_test_server(port).await; // regular TCP server
    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        mptcp: true,
        ..Default::default()
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;
    assert!(result.is_ok(), "MPTCP client to TCP server should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "MPTCP client to TCP server should succeed (kernel falls back): {:?}",
        result
    );
}

// Regular TCP client connects to server (server auto-uses MPTCP, kernel falls back to TCP)
#[tokio::test]
async fn test_tcp_client_to_mptcp_server() {
    if !mptcp_available() {
        return;
    }
    let port = get_test_port();
    let _server = start_test_server(port).await; // server auto-uses MPTCP
    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        mptcp: false, // regular TCP client
        ..Default::default()
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;
    assert!(result.is_ok(), "TCP client to MPTCP server should complete");
    let result = result.unwrap();
    assert!(
        result.is_ok(),
        "TCP client to MPTCP server should succeed (kernel falls back): {:?}",
        result
    );
}

#[tokio::test]
async fn test_cli_csv_output_file_is_csv() {
    let port = get_test_port();
    let server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.csv");
    let output = timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_xfr"))
            .args(["127.0.0.1", "-p", &port.to_string(), "-t", "1", "--no-tui"])
            .arg("--csv")
            .arg("-o")
            .arg(&path)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = saved.lines().collect();
    assert_eq!(lines.len(), 2, "header plus one summary row: {saved}");
    assert!(lines[0].starts_with("test_id,duration_secs,"), "{saved}");
    assert_eq!(lines[0].split(',').count(), lines[1].split(',').count());
    server.abort();
}

#[test]
fn test_cli_client_options_rejected_with_subcommands() {
    let dir = tempfile::tempdir().unwrap();
    let result_path = dir.path().join("result.json");
    let result = serde_json::json!({
        "id": "subcommand-test", "bytes_total": 1000, "duration_ms": 1000,
        "throughput_mbps": 1.0, "streams": [],
    });
    std::fs::write(&result_path, result.to_string()).unwrap();

    for (args, subcommand) in [
        (
            &["--json", "-t", "5", "-P", "4", "diff", "a", "b"][..],
            "diff",
        ),
        (&["--csv", "-q", "-R", "discover"][..], "discover"),
        (&["-t", "5", "serve"][..], "serve"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("cannot be used with '{subcommand}'")),
            "{args:?}: {stderr}"
        );
    }

    // Options subcommands use, and env-backed client options, still work.
    let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
        .args(["--theme", "nord", "--log-level", "warn", "diff"])
        .arg(&result_path)
        .arg(&result_path)
        .env("XFR_DURATION", "5")
        .env("XFR_PORT", "9000")
        .env("XFR_PSK", "secret")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_cli_json_stream_conflicts_with_quiet() {
    let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
        .args(["127.0.0.1", "--json-stream", "-q", "--no-tui"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--json-stream") && stderr.contains("--quiet"));
}

#[test]
fn test_cli_mptcp_conflicts() {
    // --mptcp conflicts with --udp
    let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
        .args(["127.0.0.1", "--mptcp", "--udp", "--no-tui"])
        .output()
        .expect("failed to run xfr binary");
    assert!(!output.status.success());

    // --mptcp conflicts with --quic
    let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
        .args(["127.0.0.1", "--mptcp", "--quic", "--no-tui"])
        .output()
        .expect("failed to run xfr binary");
    assert!(!output.status.success());
}

#[test]
fn test_cli_cport_overflow_error_message() {
    let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
        .args(["127.0.0.1", "-u", "--cport", "65535", "-P", "2", "--no-tui"])
        .output()
        .expect("failed to run xfr binary");

    assert!(
        !output.status.success(),
        "command should fail when --cport range overflows"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requires ports 65535-65536, which exceeds 65535"),
        "unexpected stderr: {}",
        stderr
    );
}

#[test]
fn test_cli_tcp_cport_overflow_error_message() {
    let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
        .args(["127.0.0.1", "--cport", "65535", "-P", "2", "--no-tui"])
        .output()
        .expect("failed to run xfr binary");

    assert!(
        !output.status.success(),
        "command should fail when TCP --cport range overflows"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requires ports 65535-65536, which exceeds 65535"),
        "unexpected stderr: {}",
        stderr
    );
}

// --- Server --bind tests ---

/// Server bound to 127.0.0.1 should accept TCP connections on that address
#[tokio::test]
async fn test_serve_bind_ipv4_loopback() {
    let port = get_test_port();
    let config = ServerConfig {
        port,
        bind_addr: Some("127.0.0.1".parse().unwrap()),
        address_family: xfr::net::AddressFamily::V4Only,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        ..Default::default()
    };
    let _server = tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client_cfg = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(1),
        direction: Direction::Upload,
        address_family: xfr::net::AddressFamily::V4Only,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        ..Default::default()
    };
    let result = timeout(Duration::from_secs(10), Client::new(client_cfg).run(None)).await;
    assert!(result.is_ok(), "Test should complete");
    assert!(
        result.unwrap().is_ok(),
        "TCP to bound 127.0.0.1 should succeed"
    );
}

/// Server bound to ::1 should accept TCP connections on that address
#[tokio::test]
async fn test_serve_bind_ipv6_loopback() {
    // Skip if IPv6 is not available
    if std::net::TcpListener::bind("[::1]:0").is_err() {
        return;
    }

    let port = get_test_port();
    let config = ServerConfig {
        port,
        bind_addr: Some("::1".parse().unwrap()),
        address_family: xfr::net::AddressFamily::V6Only,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        ..Default::default()
    };
    let _server = tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client_cfg = ClientConfig {
        host: "::1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::from_secs(1),
        direction: Direction::Upload,
        address_family: xfr::net::AddressFamily::V6Only,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        ..Default::default()
    };
    let result = timeout(Duration::from_secs(10), Client::new(client_cfg).run(None)).await;
    assert!(result.is_ok(), "Test should complete");
    assert!(result.unwrap().is_ok(), "TCP to bound [::1] should succeed");
}

/// Server bound to 127.0.0.1 with QUIC should accept QUIC connections
#[tokio::test]
async fn test_serve_bind_ipv4_quic() {
    let port = get_test_port();
    let config = ServerConfig {
        port,
        bind_addr: Some("127.0.0.1".parse().unwrap()),
        address_family: xfr::net::AddressFamily::V4Only,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        ..Default::default()
    };
    let _server = tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client_cfg = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 1,
        duration: Duration::from_secs(1),
        direction: Direction::Upload,
        address_family: xfr::net::AddressFamily::V4Only,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        ..Default::default()
    };
    let result = timeout(Duration::from_secs(10), Client::new(client_cfg).run(None)).await;
    assert!(result.is_ok(), "Test should complete");
    assert!(
        result.unwrap().is_ok(),
        "QUIC to bound 127.0.0.1 should succeed"
    );
}

/// Server bound to ::1 with QUIC should accept QUIC connections
#[tokio::test]
async fn test_serve_bind_ipv6_quic() {
    if std::net::TcpListener::bind("[::1]:0").is_err() {
        return;
    }

    let port = get_test_port();
    let config = ServerConfig {
        port,
        bind_addr: Some("::1".parse().unwrap()),
        address_family: xfr::net::AddressFamily::V6Only,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        ..Default::default()
    };
    let _server = tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client_cfg = ClientConfig {
        host: "::1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 1,
        duration: Duration::from_secs(1),
        direction: Direction::Upload,
        address_family: xfr::net::AddressFamily::V6Only,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        ..Default::default()
    };
    let result = timeout(Duration::from_secs(10), Client::new(client_cfg).run(None)).await;
    assert!(result.is_ok(), "Test should complete");
    assert!(
        result.unwrap().is_ok(),
        "QUIC to bound [::1] should succeed"
    );
}

/// --bind with mismatched -4/-6 should error at the CLI level
#[test]
fn test_serve_bind_family_mismatch_ipv4_with_ipv6_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
        .args(["serve", "--bind", "127.0.0.1", "-6"])
        .output()
        .expect("failed to run xfr binary");

    assert!(
        !output.status.success(),
        "should fail: --bind 127.0.0.1 contradicts -6"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("IPv4") && stderr.contains("IPv6"),
        "error should mention the family mismatch: {}",
        stderr
    );
}

/// --bind with mismatched -4/-6 should error at the CLI level (reverse case)
#[test]
fn test_serve_bind_family_mismatch_ipv6_with_ipv4_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
        .args(["serve", "--bind", "::1", "-4"])
        .output()
        .expect("failed to run xfr binary");

    assert!(
        !output.status.success(),
        "should fail: --bind ::1 contradicts -4"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("IPv6") && stderr.contains("IPv4"),
        "error should mention the family mismatch: {}",
        stderr
    );
}

/// --bind :: (unspecified) should be rejected
#[test]
fn test_serve_bind_rejects_unspecified_ipv6() {
    let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
        .args(["serve", "--bind", "::"])
        .output()
        .expect("failed to run xfr binary");

    assert!(
        !output.status.success(),
        "should fail: --bind :: is unspecified"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unspecified"),
        "error should mention unspecified address: {}",
        stderr
    );
}

/// --bind 0.0.0.0 (unspecified) should be rejected
#[test]
fn test_serve_bind_rejects_unspecified_ipv4() {
    let output = Command::new(env!("CARGO_BIN_EXE_xfr"))
        .args(["serve", "--bind", "0.0.0.0"])
        .output()
        .expect("failed to run xfr binary");

    assert!(
        !output.status.success(),
        "should fail: --bind 0.0.0.0 is unspecified"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unspecified"),
        "error should mention unspecified address: {}",
        stderr
    );
}

/// Server bound to 127.0.0.1 should accept UDP connections on that address
#[tokio::test]
async fn test_serve_bind_ipv4_udp() {
    let port = get_test_port();
    let config = ServerConfig {
        port,
        bind_addr: Some("127.0.0.1".parse().unwrap()),
        address_family: xfr::net::AddressFamily::V4Only,
        #[cfg(feature = "prometheus")]
        prometheus_port: None,
        ..Default::default()
    };
    let _server = tokio::spawn(async move {
        let server = Server::new(config);
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client_cfg = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 1,
        duration: Duration::from_secs(1),
        direction: Direction::Upload,
        address_family: xfr::net::AddressFamily::V4Only,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        ..Default::default()
    };
    let result = timeout(Duration::from_secs(10), Client::new(client_cfg).run(None)).await;
    assert!(result.is_ok(), "Test should complete");
    assert!(
        result.unwrap().is_ok(),
        "UDP to bound 127.0.0.1 should succeed"
    );
}

// ============================================================================
// Single-port UDP (issue #63)
// ============================================================================

/// Raw control-channel driver for protocol-shape assertions that the
/// `Client` API hides (capability negotiation, TestAck contents).
struct RawControl {
    reader: tokio::io::BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    server_capabilities: Vec<String>,
}

impl RawControl {
    async fn connect(port: u16, capabilities: Vec<String>) -> Self {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        use xfr::protocol::ControlMessage;

        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("control connect");
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        let hello = ControlMessage::Hello {
            version: "1.1".to_string(),
            client: Some("xfr-test".to_string()),
            server: None,
            capabilities: Some(capabilities),
            auth: None,
            client_nonce: None,
        };
        writer
            .write_all(format!("{}\n", hello.serialize().unwrap()).as_bytes())
            .await
            .expect("send hello");

        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("read server hello");
        let server_capabilities = match ControlMessage::deserialize(line.trim()).unwrap() {
            ControlMessage::Hello { capabilities, .. } => capabilities.unwrap_or_default(),
            other => panic!("expected server hello, got {:?}", other),
        };

        Self {
            reader,
            writer,
            server_capabilities,
        }
    }

    /// Send a UDP TestStart and return the TestAck's (data_ports, udp_token).
    async fn start_udp_test(&mut self, duration_secs: u32) -> (Vec<u16>, Option<String>) {
        self.start_udp_test_with_payload(duration_secs, Direction::Upload, None)
            .await
    }

    async fn start_udp_test_with_payload(
        &mut self,
        duration_secs: u32,
        direction: Direction,
        udp_payload_size: Option<u16>,
    ) -> (Vec<u16>, Option<String>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        use xfr::protocol::ControlMessage;

        let start = ControlMessage::TestStart {
            id: "raw-test".to_string(),
            protocol: Protocol::Udp,
            streams: 1,
            duration_secs,
            direction,
            bitrate: Some(10_000_000),
            congestion: None,
            mptcp: false,
            dscp: None,
            window_size: None,
            zerocopy: false,
            mtu_probe: false,
            tcp_nodelay: false,
            byte_budget: None,
            udp_payload_size,
        };
        self.writer
            .write_all(format!("{}\n", start.serialize().unwrap()).as_bytes())
            .await
            .expect("send test_start");

        let mut line = String::new();
        self.reader.read_line(&mut line).await.expect("read ack");
        match ControlMessage::deserialize(line.trim()).unwrap() {
            ControlMessage::TestAck {
                data_ports,
                udp_token,
                ..
            } => (data_ports, udp_token),
            other => panic!("expected test_ack, got {:?}", other),
        }
    }

    /// Drain Interval messages until the final Result arrives.
    async fn wait_for_result(&mut self) -> xfr::protocol::TestResult {
        use tokio::io::AsyncBufReadExt;
        use xfr::protocol::ControlMessage;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let mut line = String::new();
            let n = tokio::time::timeout_at(deadline, self.reader.read_line(&mut line))
                .await
                .expect("result before deadline")
                .expect("control read");
            assert!(n > 0, "connection closed before Result");
            match ControlMessage::deserialize(line.trim()).unwrap() {
                ControlMessage::Result(result) => return result,
                _ => continue,
            }
        }
    }
}

#[tokio::test]
async fn test_udp_custom_payload_legacy_ports() {
    let port = get_test_port();
    let server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    for size in [1368, 8192] {
        for direction in [Direction::Upload, Direction::Download, Direction::Bidir] {
            let capabilities = xfr::protocol::supported_capabilities_without(
                xfr::protocol::SINGLE_PORT_UDP_CAPABILITY,
            );
            let mut control = RawControl::connect(port, capabilities).await;
            let (ports, token) = control
                .start_udp_test_with_payload(1, direction, Some(size))
                .await;
            assert!(token.is_none());
            assert_eq!(ports.len(), 1);
            let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            socket.connect(("127.0.0.1", ports[0])).await.unwrap();
            if direction == Direction::Download {
                socket.send(&[0]).await.unwrap();
            } else {
                let mut packet = vec![0; usize::from(size)];
                for sequence in 0..10 {
                    xfr::udp::UdpPacketHeader {
                        sequence,
                        timestamp_us: 1,
                    }
                    .encode(&mut packet);
                    socket.send(&packet).await.unwrap();
                }
            }
            if direction != Direction::Upload {
                let mut packet = vec![0; xfr::udp::MAX_UDP_PAYLOAD_SIZE];
                let received = timeout(Duration::from_secs(5), socket.recv(&mut packet))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(received, usize::from(size), "legacy server sender size");
            }
            let result = control.wait_for_result().await;
            if direction != Direction::Download {
                let stats = result.udp_stats.unwrap();
                assert!(stats.packets_received > 0);
                assert_eq!(stats.packet_size, Some(u32::from(size)));
            }
        }
    }
    server.abort();
}

#[tokio::test]
async fn test_udp_payload_size_rejects_invalid_wire_requests() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use xfr::protocol::ControlMessage;
    let port = get_test_port();
    let server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    for (protocol, size, probe) in [
        ("udp", 0, false),
        ("udp", 15, false),
        ("udp", 65508, false),
        ("tcp", 1368, false),
        ("udp", 1368, true),
    ] {
        let mut control = RawControl::connect(port, xfr::protocol::supported_capabilities()).await;
        let request = serde_json::json!({
            "type": "test_start", "id": "invalid-payload", "protocol": protocol,
            "streams": 1, "duration_secs": 1, "direction": "upload",
            "udp_payload_size": size, "mtu_probe": probe,
        });
        control
            .writer
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        timeout(Duration::from_secs(5), control.reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        match ControlMessage::deserialize(line.trim()).unwrap() {
            ControlMessage::Error { message } => assert!(message.contains("UDP payload size")),
            other => panic!("expected invalid payload rejection, got {other:?}"),
        }
    }
    server.abort();
}

async fn assert_legacy_udp_cancel_before_hello(direction: Direction) {
    use tokio::io::AsyncWriteExt;
    use xfr::protocol::ControlMessage;

    let port = get_test_port();
    let server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // No single_port_udp_v1 capability: use the legacy per-stream port and
    // cancel before any data-plane hello reaches the discovery handler.
    let mut control = RawControl::connect(port, vec![]).await;
    let (data_ports, udp_token) = control
        .start_udp_test_with_payload(0, direction, None)
        .await;
    assert_eq!(data_ports.len(), 1);
    assert!(udp_token.is_none());
    let cancel = ControlMessage::Cancel {
        id: "raw-test".to_string(),
        reason: "test cancellation before UDP hello".to_string(),
    };
    control
        .writer
        .write_all(format!("{}\n", cancel.serialize().unwrap()).as_bytes())
        .await
        .unwrap();

    let result = timeout(Duration::from_secs(2), control.wait_for_result())
        .await
        .expect("cancel before UDP discovery should return a summary promptly");
    assert_eq!(result.bytes_total, 0);
    server.abort();
}

#[tokio::test]
async fn test_legacy_udp_download_cancel_before_hello() {
    assert_legacy_udp_cancel_before_hello(Direction::Download).await;
}

#[tokio::test]
async fn test_legacy_udp_bidir_cancel_before_hello() {
    assert_legacy_udp_cancel_before_hello(Direction::Bidir).await;
}

/// The TestAck shape must match what was negotiated: a server that
/// advertises single_port_udp_v1 (self-test passed) answers a capable
/// client with no data ports plus a routing token; a server that
/// doesn't (platforms where the kernel routing check fails) must keep
/// allocating per-stream ports. Self-validating on either platform.
#[tokio::test]
async fn test_single_port_udp_testack_shape() {
    let port = get_test_port();
    let _server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut control = RawControl::connect(port, xfr::protocol::supported_capabilities()).await;
    let server_single_port = control
        .server_capabilities
        .iter()
        .any(|c| c == xfr::protocol::SINGLE_PORT_UDP_CAPABILITY);
    let (data_ports, udp_token) = control.start_udp_test(1).await;

    if server_single_port {
        assert!(
            data_ports.is_empty(),
            "single-port UDP must not allocate per-stream ports, got {:?}",
            data_ports
        );
        let token = udp_token.expect("single-port TestAck must carry a routing token");
        assert!(
            xfr::udp::parse_hello_token(&token).is_some(),
            "token must be 32 hex digits, got {:?}",
            token
        );
    } else {
        assert!(
            !data_ports.is_empty(),
            "without the capability the server must allocate legacy ports"
        );
        assert!(udp_token.is_none());
    }

    // Let the 1s test run out so the server tears down cleanly.
    let _ = control.wait_for_result().await;
}

/// Legacy fallback: a client that doesn't advertise single_port_udp_v1
/// must get per-stream ephemeral ports and a fully working legacy data
/// plane, even on a server with single-port enabled.
#[tokio::test]
async fn test_legacy_udp_fallback_allocates_ports() {
    let port = get_test_port();
    let _server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let legacy_caps: Vec<String> = ["tcp", "udp", "multistream"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut control = RawControl::connect(port, legacy_caps).await;
    let (data_ports, udp_token) = control.start_udp_test(2).await;

    assert_eq!(data_ports.len(), 1, "legacy client must get a data port");
    assert!(
        udp_token.is_none(),
        "legacy negotiation must not carry a token"
    );

    // Drive the legacy data plane for real: send sequenced packets to the
    // allocated ephemeral port and verify the server counted them.
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.connect(("127.0.0.1", data_ports[0])).await.unwrap();
    let mut packet = vec![0u8; 1400];
    for seq in 0..200u64 {
        let header = xfr::udp::UdpPacketHeader {
            sequence: seq,
            timestamp_us: seq * 1000,
        };
        assert!(header.encode(&mut packet));
        socket.send(&packet).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let result = control.wait_for_result().await;
    assert!(
        result.bytes_total > 0,
        "server must have counted legacy UDP data, got {} bytes",
        result.bytes_total
    );
}

/// QUIC and single-port UDP share the server's main UDP port (the tee on
/// quinn's socket). Run both protocols against the same server instance
/// at the same time to prove neither lane disturbs the other.
#[tokio::test]
async fn test_quic_and_single_port_udp_coexist() {
    let port = get_test_port();
    let _server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let udp_config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 2,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        bitrate: Some(50_000_000),
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        ..Default::default()
    };
    let quic_config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Quic,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Upload,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        ..Default::default()
    };

    let udp_client = Client::new(udp_config);
    let quic_client = Client::new(quic_config);
    let (udp_result, quic_result) = tokio::join!(
        timeout(Duration::from_secs(20), udp_client.run(None)),
        timeout(Duration::from_secs(20), quic_client.run(None)),
    );

    let udp_result = udp_result
        .expect("UDP test should complete")
        .expect("UDP test should succeed alongside QUIC");
    assert!(udp_result.duration_ms > 0);
    assert_eq!(udp_result.streams.len(), 2);

    let quic_result = quic_result
        .expect("QUIC test should complete")
        .expect("QUIC test should succeed with the tee in place");
    assert!(
        quic_result.bytes_total > 0,
        "QUIC must still move data through the shared socket"
    );
}

/// Single-port UDP download: server-sent data must flow over the
/// connected same-port socket and the client's receiver-truth overlay
/// (issue #81) must still populate loss/jitter stats.
#[tokio::test]
async fn test_single_port_udp_download_stats() {
    let port = get_test_port();
    let _server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 1,
        duration: Duration::from_secs(2),
        direction: Direction::Download,
        bitrate: Some(50_000_000),
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        ..Default::default()
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(20), client.run(None))
        .await
        .expect("download should complete")
        .expect("download should succeed");
    assert!(result.duration_ms > 0);
    assert!(
        result.bytes_total > 0,
        "client must have received data over the connected socket"
    );
    assert!(
        result.udp_stats.is_some(),
        "download summary must carry receiver-side loss/jitter stats"
    );
}

/// Cancel latency: cancelling an infinite-duration test must return promptly
/// even when the stats loop is between 1s interval ticks.  Before LAN-160 the
/// cancel read was serialized behind the interval tick, so a cancel issued
/// 200ms into the test would not be observed until the 1s tick — up to 1s of
/// latency.  With the dedicated control-reader task the cancel is observed
/// within tens of milliseconds.
#[tokio::test]
async fn test_tcp_cancel_latency_before_first_tick() {
    let port = get_test_port();
    let _server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::ZERO, // infinite — only cancel ends it
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);

    // Poll the run future so it connects and installs the cancel channel,
    // then cancel at 200ms — well before the first 1s stats tick.  The run
    // must resolve within 600ms of the cancel, proving the server's
    // control-reader task observed it promptly.  The old serialized-behind-
    // tick behavior would not observe the cancel until the ~1s tick, so
    // the run would resolve at ~1s+RTT — well outside this 600ms budget.
    let mut run_future = std::pin::pin!(client.run(None));
    tokio::select! {
        res = &mut run_future => panic!("run ended before cancel: {res:?}"),
        _ = tokio::time::sleep(Duration::from_millis(200)) => {}
    }
    assert!(client.cancel().is_ok(), "cancel should be accepted");

    let result = tokio::time::timeout(Duration::from_millis(600), &mut run_future).await;
    assert!(
        result.is_ok(),
        "run should resolve within 600ms of cancel — old serialized behavior would take ~1s"
    );
}

/// PSK cancel: the dedicated control-reader task must decrypt AEAD-protected
/// cancel messages.  This test would have caught the original LAN-160/LAN-159
/// conflict where `spawn_control_reader` used raw `read_bounded_line` instead
/// of `ControlReader::read_message`, breaking cancel/pause for PSK sessions.
#[tokio::test]
async fn test_tcp_psk_cancel_infinite_duration() {
    let port = get_test_port();
    let psk = "psk-cancel-secret".to_string();
    let _server = start_secure_server(port, Some(psk.clone()), None, vec![], vec![]).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Tcp,
        streams: 1,
        duration: Duration::ZERO, // infinite — only cancel ends it
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: Some(psk),
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);

    // Poll the run future so it connects and installs the cancel channel,
    // then cancel after 500ms.  The run must resolve within 2s of the cancel
    // — if the server's AEAD control reader is broken, the cancel is never
    // observed and the infinite test hangs until the timeout fails the test.
    let mut run_future = std::pin::pin!(client.run(None));
    tokio::select! {
        res = &mut run_future => panic!("run ended before cancel: {res:?}"),
        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
    }
    let _ = client.cancel();

    let result = tokio::time::timeout(Duration::from_secs(2), &mut run_future).await;
    assert!(
        result.is_ok(),
        "PSK cancel should resolve within 2s — if it times out the AEAD control reader is broken"
    );
}

/// UDP pause/resume smoke test: pause and resume an infinite-duration UDP
/// test and confirm it doesn't hang or crash.  The unit test in udp.rs
/// (`test_udp_paced_no_burst_after_resume`) covers the no-burst invariant.
#[tokio::test]
async fn test_udp_pause_resume_no_burst() {
    let port = get_test_port();
    let _server = start_test_server(port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let config = ClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        protocol: Protocol::Udp,
        streams: 1,
        duration: Duration::ZERO, // infinite
        direction: Direction::Upload,
        bitrate: Some(80_000), // 80kbps — slow enough to observe pacing
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
        sequential_ports: false,
        mptcp: false,
        random_payload: false,
        zerocopy: ZerocopyMode::Off,
        dscp: None,
        mtu_probe: false,
        connect_timeout: None,
        byte_budget: None,
        udp_payload_size: None,
    };

    let client = Client::new(config);

    // Poll the run future so it connects and installs the pause/cancel
    // channels, then exercise pause/resume/cancel in sequence.  Each sleep
    // is inside a select! against the run future so it stays polled.
    let mut run_future = std::pin::pin!(client.run(None));
    tokio::select! {
        res = &mut run_future => panic!("run ended before pause: {res:?}"),
        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
    }
    let _ = client.pause();
    tokio::select! {
        res = &mut run_future => panic!("run ended during pause: {res:?}"),
        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
    }
    let _ = client.pause(); // toggle: pause→resume
    tokio::select! {
        res = &mut run_future => panic!("run ended after resume: {res:?}"),
        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
    }
    let _ = client.cancel();

    let result = tokio::time::timeout(Duration::from_secs(3), &mut run_future).await;
    assert!(
        result.is_ok(),
        "UDP pause/resume/cancel should resolve within 3s"
    );
}

/// `-n`: the transfer must be exactly the requested size, and end when the
/// bytes are delivered rather than when a clock runs out. Runs each direction
/// and protocol because the stopping condition is negotiated differently in
/// each: upload ends on the client's `Finish` plus a drain, download ends when
/// the server's own send loops spend the budget, bidir needs both.
mod byte_budget {
    use super::*;

    const BUDGET: u64 = 4 * 1024 * 1024;

    fn budget_config(
        port: u16,
        protocol: Protocol,
        direction: Direction,
        streams: u8,
    ) -> ClientConfig {
        ClientConfig {
            host: "127.0.0.1".to_string(),
            port,
            protocol,
            streams,
            // Duration::ZERO is what `-n` without `-t` resolves to: no clock,
            // the byte count is the only stopping condition.
            duration: Duration::ZERO,
            direction,
            byte_budget: Some(BUDGET),
            random_payload: false,
            zerocopy: ZerocopyMode::Auto,
            ..Default::default()
        }
    }

    async fn run_budget_test(config: ClientConfig) -> xfr::protocol::TestResult {
        let client = Client::new(config);
        timeout(Duration::from_secs(30), client.run(None))
            .await
            .expect("byte-budget test should finish on its own, not hang")
            .expect("test should succeed")
    }

    #[tokio::test]
    async fn tcp_upload_transfers_exactly_the_budget() {
        let port = get_test_port();
        let _server = start_test_server(port).await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        let result =
            run_budget_test(budget_config(port, Protocol::Tcp, Direction::Upload, 1)).await;

        // bytes_total is the receiver's count in upload mode: this asserts the
        // bytes actually arrived, not merely that we wrote them.
        assert_eq!(
            result.bytes_total, BUDGET,
            "upload should deliver exactly the budget"
        );
    }

    #[tokio::test]
    async fn tcp_download_transfers_exactly_the_budget() {
        let port = get_test_port();
        let _server = start_test_server(port).await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        let result =
            run_budget_test(budget_config(port, Protocol::Tcp, Direction::Download, 1)).await;

        assert_eq!(
            result.bytes_total, BUDGET,
            "download should deliver exactly the budget"
        );
    }

    #[tokio::test]
    async fn multi_stream_splits_one_shared_budget() {
        let port = get_test_port();
        let _server = start_test_server(port).await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        let result =
            run_budget_test(budget_config(port, Protocol::Tcp, Direction::Upload, 4)).await;

        // The budget is shared, not per-stream: 4 streams still total N, and
        // whichever stream gets more socket time simply claims more of it.
        assert_eq!(
            result.bytes_total, BUDGET,
            "4 streams should still total exactly the budget, not 4x it"
        );
    }

    #[tokio::test]
    async fn udp_upload_transfers_the_budget_in_whole_datagrams() {
        let port = get_test_port();
        let _server = start_test_server(port).await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut config = budget_config(port, Protocol::Udp, Direction::Upload, 1);
        // Paced: unlimited UDP on loopback overruns the receive buffer and
        // drops several percent, which would make the delivered count a test
        // of kernel buffering rather than of the budget.
        config.bitrate = Some(50_000_000);
        let client = Client::new(config);
        let result = timeout(Duration::from_secs(30), client.run(None))
            .await
            .expect("UDP byte-budget test should finish on its own")
            .expect("test should succeed");

        // UDP promises "send exactly N", not "deliver exactly N" — loss is
        // real and separately reported. What must hold: the sender stopped at
        // the budget (never over), the test ended on its own rather than
        // running forever on a ZERO duration, and the bulk of it arrived.
        assert!(
            result.bytes_total <= BUDGET,
            "UDP must never exceed the budget: got {}",
            result.bytes_total
        );
        assert!(
            result.bytes_total >= BUDGET / 2,
            "UDP should deliver most of the budget on loopback: got {}",
            result.bytes_total
        );
    }

    #[tokio::test]
    async fn explicit_duration_caps_a_budget_that_cannot_finish() {
        let port = get_test_port();
        let _server = start_test_server(port).await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        // 64 MiB at 10 Mbps needs ~50s; -t 2s must cut it short rather than
        // the budget holding the test open.
        let mut config = budget_config(port, Protocol::Tcp, Direction::Upload, 1);
        config.byte_budget = Some(64 * 1024 * 1024);
        config.duration = Duration::from_secs(2);
        config.bitrate = Some(10_000_000);

        let result = run_budget_test(config).await;

        assert!(
            result.bytes_total < 64 * 1024 * 1024,
            "the duration cap should stop the transfer before the budget is met"
        );
        assert!(
            result.duration_ms < 10_000,
            "test should end near its 2s cap, took {}ms",
            result.duration_ms
        );
    }
}
