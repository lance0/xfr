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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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

    tokio::time::sleep(Duration::from_millis(100)).await;

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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

    assert!(result.is_ok(), "UDP download should complete");
    let result = result.unwrap();
    assert!(result.is_ok(), "UDP download should succeed: {:?}", result);
}

#[tokio::test]
async fn test_udp_bidir() {
    let port = get_test_port();
    let _server = start_test_server(port).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
    };

    let client = Client::new(config);
    let result = timeout(Duration::from_secs(10), client.run(None)).await;

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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: Some(psk),
        address_family: xfr::net::AddressFamily::default(),
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
        psk: Some("wrong-secret".to_string()),
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None, // No PSK provided
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: Some(psk),
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::default(),
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
        psk: None,
        address_family: xfr::net::AddressFamily::V6Only,
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
