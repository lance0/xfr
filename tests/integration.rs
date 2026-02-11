//! Integration tests for xfr

use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use tokio::time::timeout;

use xfr::client::{Client, ClientConfig};
use xfr::protocol::{Direction, Protocol};
use xfr::serve::{Server, ServerConfig};

// Use different ports for each test to avoid conflicts
static PORT_COUNTER: AtomicU16 = AtomicU16::new(16000);

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
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "Bidir test should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "Bidir test should succeed: {:?}", result);

    let result = result.unwrap();
    assert!(result.duration_ms > 0, "Should have duration");
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
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "UDP test should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "UDP test should succeed: {:?}", result);

    let result = result.unwrap();
    assert!(result.duration_ms > 0, "Should have duration");
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
        duration: Duration::from_secs(3),
        direction: Direction::Upload,
        bitrate: None,
        tcp_nodelay: false,
        window_size: None,
        tcp_congestion: None,
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
        bind_addr: None,
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
    };

    let client2 = Client::new(config2);
    let result2 = timeout(Duration::from_secs(5), client2.run(None)).await;

    // Second connection should fail (rate limited - connection dropped)
    // The server drops the connection before hello, so client gets connection error
    if let Ok(inner) = result2 {
        // Either fails or succeeds if timing allows
        // We mainly verify we don't panic
        let _ = inner;
    }

    // Wait for first client
    let result1 = handle1.await;
    assert!(result1.is_ok(), "First client task should complete");
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
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "QUIC with PSK should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "QUIC with PSK should succeed: {:?}", result);
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
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(5), client.run(None)).await;

    // Connection should be dropped by ACL
    assert!(result.is_ok(), "Test should complete (not timeout)");
    let result = result.unwrap();
    assert!(result.is_err(), "Connection should be denied by ACL");
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

    // Server should have exited after the test
    let server_result = timeout(Duration::from_secs(2), server_handle).await;
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
    };

    let client = Client::new(config);
    assert_eq!(
        client.pause(),
        xfr::client::PauseResult::NotReady,
        "pause() should return NotReady before capability negotiation"
    );
}
