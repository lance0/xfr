//! Server mode implementation
//!
//! Listens for incoming connections and handles bandwidth tests.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::acl::{Acl, AclConfig};
use crate::audit::{AuditConfig, AuditEvent, AuditLogger};
use crate::auth::{self, AuthConfig};
use crate::net::{self, AddressFamily};
use crate::protocol::{
    ControlMessage, Direction, PROTOCOL_VERSION, Protocol, StreamInterval, TestResult,
    versions_compatible,
};
use crate::rate_limit::{RateLimitConfig, RateLimiter};
use crate::stats::TestStats;
use crate::tcp::{self, TcpConfig};
use crate::tls::{MaybeTlsServerStream, TlsServerConfig, accept_tls};
use crate::udp;
use tokio_rustls::TlsAcceptor;

/// Maximum control message line length to prevent memory DoS
const MAX_LINE_LENGTH: usize = 8192;
/// Maximum streams a client can request
const MAX_STREAMS: u8 = 128;
/// Maximum test duration a client can request (1 hour)
const MAX_TEST_DURATION: Duration = Duration::from_secs(3600);

pub struct ServerConfig {
    pub port: u16,
    pub one_off: bool,
    /// Maximum test duration (server-side limit)
    pub max_duration: Option<Duration>,
    #[cfg(feature = "prometheus")]
    pub prometheus_port: Option<u16>,
    /// Prometheus push gateway URL for pushing metrics at test completion
    pub push_gateway_url: Option<String>,
    /// Authentication configuration
    pub auth: AuthConfig,
    /// TLS configuration
    pub tls: TlsServerConfig,
    /// Access control list configuration
    pub acl: AclConfig,
    /// Rate limiting configuration
    pub rate_limit: RateLimitConfig,
    /// Audit logging configuration
    pub audit: AuditConfig,
    /// Address family (IPv4, IPv6, dual-stack)
    pub address_family: AddressFamily,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: crate::protocol::DEFAULT_PORT,
            one_off: false,
            max_duration: None,
            #[cfg(feature = "prometheus")]
            prometheus_port: None,
            push_gateway_url: None,
            auth: AuthConfig::default(),
            tls: TlsServerConfig::default(),
            acl: AclConfig::default(),
            rate_limit: RateLimitConfig::default(),
            audit: AuditConfig::default(),
            address_family: AddressFamily::default(),
        }
    }
}

/// Security context shared across client handlers
struct SecurityContext {
    psk: Option<String>,
    acl: Acl,
    rate_limiter: Option<Arc<RateLimiter>>,
    audit: Option<Arc<AuditLogger>>,
    tls_acceptor: Option<TlsAcceptor>,
    address_family: AddressFamily,
}

struct ActiveTest {
    #[allow(dead_code)]
    stats: Arc<TestStats>,
    #[allow(dead_code)]
    cancel_tx: watch::Sender<bool>,
    #[allow(dead_code)]
    data_ports: Vec<u16>,
}

pub struct Server {
    config: ServerConfig,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
}

impl Server {
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            active_tests: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let listener =
            net::create_tcp_listener(self.config.port, self.config.address_family).await?;

        // Initialize security context
        let acl = self.config.acl.build()?;
        let rate_limiter = self.config.rate_limit.build();
        let audit = self.config.audit.build()?;
        let tls_acceptor = self.config.tls.create_acceptor()?;

        if self.config.auth.psk.is_some() {
            info!("PSK authentication enabled");
        }
        if self.config.tls.enabled {
            info!("TLS enabled");
        }
        if acl.is_configured() {
            info!("ACL configured");
        }
        if rate_limiter.is_some() {
            info!(
                "Rate limiting enabled: {} per IP",
                self.config.rate_limit.max_per_ip.unwrap_or(0)
            );
        }
        if audit.is_some() {
            info!("Audit logging enabled");
        }

        let security = Arc::new(SecurityContext {
            psk: self.config.auth.psk.clone(),
            acl,
            rate_limiter: rate_limiter.clone(),
            audit,
            tls_acceptor,
            address_family: self.config.address_family,
        });

        // Start rate limiter cleanup task if enabled
        if let Some(limiter) = rate_limiter {
            limiter.start_cleanup_task();
        }

        // Register mDNS service for discovery
        #[cfg(feature = "discovery")]
        let _mdns = register_mdns_service(self.config.port);

        // Spawn Prometheus metrics server if enabled
        #[cfg(feature = "prometheus")]
        if let Some(prom_port) = self.config.prometheus_port {
            use crate::output::prometheus::{MetricsServer, register_metrics};
            register_metrics();
            let metrics_server = MetricsServer::new(prom_port);
            tokio::spawn(async move {
                if let Err(e) = metrics_server.run().await {
                    error!("Prometheus metrics server error: {}", e);
                }
            });
        }

        loop {
            let (stream, peer_addr) = listener.accept().await?;
            let peer_ip = peer_addr.ip();

            // Check ACL
            if !security.acl.is_allowed(peer_ip) {
                warn!("Connection rejected by ACL: {}", peer_addr);
                if let Some(audit) = &security.audit {
                    audit.log(AuditEvent::AclDenied {
                        ip: peer_ip,
                        rule: security.acl.matched_rule(peer_ip),
                    });
                }
                drop(stream);
                continue;
            }

            // Check rate limit
            if let Some(limiter) = &security.rate_limiter
                && let Err(e) = limiter.check(peer_ip)
            {
                warn!("Rate limit exceeded for {}: {}", peer_addr, e);
                if let Some(audit) = &security.audit {
                    audit.log(AuditEvent::RateLimitHit {
                        ip: peer_ip,
                        current: e.current,
                        max: e.max,
                    });
                }
                drop(stream);
                continue;
            }

            info!("Client connected: {}", peer_addr);

            let active_tests = self.active_tests.clone();
            let base_port = self.config.port;
            let max_duration = self.config.max_duration;
            let security = security.clone();

            let handle = tokio::spawn(async move {
                // Log connection
                if let Some(audit) = &security.audit {
                    audit.log(AuditEvent::ClientConnect {
                        ip: peer_ip,
                        tls: security.tls_acceptor.is_some(),
                    });
                }

                let result = handle_client_secure(
                    stream,
                    peer_addr,
                    active_tests,
                    base_port,
                    max_duration,
                    &security,
                )
                .await;

                // Release rate limit slot
                if let Some(limiter) = &security.rate_limiter {
                    limiter.release(peer_ip);
                }

                if let Err(e) = result {
                    error!("Client error {}: {}", peer_addr, e);
                }
            });

            if self.config.one_off {
                // Wait for the test to complete
                let _ = handle.await;
                break;
            }
        }

        Ok(())
    }
}

/// Handle client with security checks (TLS, auth)
async fn handle_client_secure(
    stream: TcpStream,
    peer_addr: SocketAddr,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    base_port: u16,
    server_max_duration: Option<Duration>,
    security: &SecurityContext,
) -> anyhow::Result<()> {
    // Accept TLS if configured
    let tls_stream = accept_tls(stream, &security.tls_acceptor).await?;

    // Split the stream based on TLS status
    match tls_stream {
        MaybeTlsServerStream::Plain(stream) => {
            handle_client_with_auth(
                stream,
                peer_addr,
                active_tests,
                base_port,
                server_max_duration,
                security,
            )
            .await
        }
        MaybeTlsServerStream::Tls(stream) => {
            let (reader, writer) = tokio::io::split(stream);
            handle_client_with_auth_split(
                reader,
                writer,
                peer_addr,
                active_tests,
                base_port,
                server_max_duration,
                security,
            )
            .await
        }
    }
}

/// Handle client with authentication (for plain TCP)
async fn handle_client_with_auth(
    stream: TcpStream,
    peer_addr: SocketAddr,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    _base_port: u16,
    server_max_duration: Option<Duration>,
    security: &SecurityContext,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    // Perform authentication handshake
    let auth_nonce = perform_auth_handshake(&mut reader, &mut writer, security).await?;

    // If auth was required, verify the response
    if let Some(nonce) = auth_nonce {
        read_bounded_line(&mut reader, &mut line).await?;
        let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

        match msg {
            ControlMessage::AuthResponse { response } => {
                let psk = security.psk.as_ref().unwrap();
                if !auth::verify_response(&nonce, psk, &response) {
                    if let Some(audit) = &security.audit {
                        audit.log(AuditEvent::AuthFailure {
                            ip: peer_addr.ip(),
                            method: "psk".to_string(),
                            reason: "Invalid response".to_string(),
                        });
                    }
                    let error = ControlMessage::error("Authentication failed");
                    writer
                        .write_all(format!("{}\n", error.serialize()?).as_bytes())
                        .await?;
                    return Err(anyhow::anyhow!("Authentication failed"));
                }

                if let Some(audit) = &security.audit {
                    audit.log(AuditEvent::AuthSuccess {
                        ip: peer_addr.ip(),
                        method: "psk".to_string(),
                    });
                }

                // Send auth success
                let success = ControlMessage::auth_success();
                writer
                    .write_all(format!("{}\n", success.serialize()?).as_bytes())
                    .await?;
            }
            _ => {
                let error = ControlMessage::error("Expected auth response");
                writer
                    .write_all(format!("{}\n", error.serialize()?).as_bytes())
                    .await?;
                return Err(anyhow::anyhow!("Expected auth response"));
            }
        }
    }

    // Continue with normal test handling
    handle_test_request(
        &mut reader,
        &mut writer,
        peer_addr,
        active_tests,
        server_max_duration,
        security,
    )
    .await
}

/// Handle client with TLS (split reader/writer)
async fn handle_client_with_auth_split<R, W>(
    reader: R,
    mut writer: W,
    peer_addr: SocketAddr,
    _active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    _base_port: u16,
    _server_max_duration: Option<Duration>,
    security: &SecurityContext,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    // Read client hello
    reader.read_line(&mut line).await?;
    let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

    match msg {
        ControlMessage::Hello { version, .. } => {
            if !versions_compatible(&version, PROTOCOL_VERSION) {
                let error = ControlMessage::error(format!(
                    "Incompatible protocol version: {} (server: {})",
                    version, PROTOCOL_VERSION
                ));
                writer
                    .write_all(format!("{}\n", error.serialize()?).as_bytes())
                    .await?;
                return Err(anyhow::anyhow!("Protocol version mismatch"));
            }

            // Send server hello (with auth challenge if required)
            let hello = if security.psk.is_some() {
                let nonce = auth::generate_nonce();
                ControlMessage::server_hello_with_auth(nonce)
            } else {
                ControlMessage::server_hello()
            };
            writer
                .write_all(format!("{}\n", hello.serialize()?).as_bytes())
                .await?;
        }
        _ => {
            let error = ControlMessage::error("Expected hello message");
            writer
                .write_all(format!("{}\n", error.serialize()?).as_bytes())
                .await?;
            return Err(anyhow::anyhow!("Expected hello message"));
        }
    }

    // For TLS streams, we can't easily split back for the existing handle_client
    // This is a simplified version - full implementation would need refactoring
    warn!(
        "TLS test handling not fully implemented yet for {}",
        peer_addr
    );
    Ok(())
}

/// Perform authentication handshake, returns nonce if auth was required
async fn perform_auth_handshake<W: tokio::io::AsyncWrite + Unpin>(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: &mut W,
    security: &SecurityContext,
) -> anyhow::Result<Option<String>> {
    let mut line = String::new();

    // Read client hello
    read_bounded_line(reader, &mut line).await?;
    let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

    match msg {
        ControlMessage::Hello { version, .. } => {
            if !versions_compatible(&version, PROTOCOL_VERSION) {
                let error = ControlMessage::error(format!(
                    "Incompatible protocol version: {} (server: {})",
                    version, PROTOCOL_VERSION
                ));
                writer
                    .write_all(format!("{}\n", error.serialize()?).as_bytes())
                    .await?;
                return Err(anyhow::anyhow!("Protocol version mismatch"));
            }

            // Send server hello (with auth challenge if required)
            if security.psk.is_some() {
                let nonce = auth::generate_nonce();
                let hello = ControlMessage::server_hello_with_auth(nonce.clone());
                writer
                    .write_all(format!("{}\n", hello.serialize()?).as_bytes())
                    .await?;
                Ok(Some(nonce))
            } else {
                let hello = ControlMessage::server_hello();
                writer
                    .write_all(format!("{}\n", hello.serialize()?).as_bytes())
                    .await?;
                Ok(None)
            }
        }
        _ => {
            let error = ControlMessage::error("Expected hello message");
            writer
                .write_all(format!("{}\n", error.serialize()?).as_bytes())
                .await?;
            Err(anyhow::anyhow!("Expected hello message"))
        }
    }
}

/// Handle test request after authentication
async fn handle_test_request(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    peer_addr: SocketAddr,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    server_max_duration: Option<Duration>,
    security: &SecurityContext,
) -> anyhow::Result<()> {
    let mut line = String::new();

    // Read test request
    read_bounded_line(reader, &mut line).await?;
    let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

    match msg {
        ControlMessage::TestStart {
            id,
            protocol,
            streams,
            duration_secs,
            direction,
            bitrate,
        } => {
            // Validate stream count
            if streams > MAX_STREAMS {
                let error = ControlMessage::error(format!(
                    "Requested {} streams exceeds maximum of {}",
                    streams, MAX_STREAMS
                ));
                writer
                    .write_all(format!("{}\n", error.serialize()?).as_bytes())
                    .await?;
                return Err(anyhow::anyhow!("Stream count exceeds maximum"));
            }

            // Calculate effective duration
            let mut duration = Duration::from_secs(duration_secs as u64);
            if duration > MAX_TEST_DURATION {
                duration = MAX_TEST_DURATION;
                warn!(
                    "Client requested {}s, capped to {}s",
                    duration_secs,
                    MAX_TEST_DURATION.as_secs()
                );
            }
            if let Some(max_dur) = server_max_duration
                && duration > max_dur
            {
                duration = max_dur;
                warn!(
                    "Client requested {}s, capped to server max {}s",
                    duration_secs,
                    max_dur.as_secs()
                );
            }

            // Log test start
            if let Some(audit) = &security.audit {
                audit.log(AuditEvent::TestStart {
                    ip: peer_addr.ip(),
                    test_id: id.clone(),
                    protocol: protocol.to_string(),
                    streams,
                    direction: direction.to_string(),
                    duration_secs: duration.as_secs() as u32,
                });
            }

            info!(
                "Test requested: {} {} streams, {} mode, {}s",
                protocol,
                streams,
                direction,
                duration.as_secs()
            );

            // Run the actual test
            let result = run_test(
                reader,
                writer,
                &id,
                protocol,
                streams,
                duration,
                direction,
                bitrate,
                active_tests.clone(),
                security.address_family,
            )
            .await;

            // Log test completion
            if let Some(audit) = &security.audit {
                match &result {
                    Ok((bytes, duration_ms, throughput)) => {
                        audit.log(AuditEvent::TestComplete {
                            ip: peer_addr.ip(),
                            test_id: id.clone(),
                            bytes: *bytes,
                            duration_ms: *duration_ms,
                            throughput_mbps: *throughput,
                        });
                    }
                    Err(e) => {
                        audit.log(AuditEvent::TestCancelled {
                            ip: peer_addr.ip(),
                            test_id: id.clone(),
                            reason: e.to_string(),
                        });
                    }
                }
            }

            result.map(|_| ())
        }
        _ => {
            let error = ControlMessage::error("Expected test_start message");
            writer
                .write_all(format!("{}\n", error.serialize()?).as_bytes())
                .await?;
            Err(anyhow::anyhow!("Expected test_start"))
        }
    }
}

/// Run the actual bandwidth test
#[allow(clippy::too_many_arguments)]
async fn run_test(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    id: &str,
    protocol: Protocol,
    streams: u8,
    duration: Duration,
    direction: Direction,
    bitrate: Option<u64>,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    address_family: AddressFamily,
) -> anyhow::Result<(u64, u64, f64)> {
    let mut line = String::new();

    // Dynamically allocate data ports (bind to 0 for OS-assigned ports)
    let mut data_ports = Vec::new();

    // Pre-bind listeners/sockets to get actual ports
    let (tcp_listeners, udp_sockets) = match protocol {
        Protocol::Tcp => {
            let mut listeners = Vec::new();
            for _ in 0..streams {
                let listener = net::create_tcp_listener(0, address_family).await?;
                data_ports.push(listener.local_addr()?.port());
                debug!("Data port {} allocated", data_ports.last().unwrap());
                listeners.push(listener);
            }
            (listeners, Vec::new())
        }
        Protocol::Udp => {
            let mut sockets = Vec::new();
            for _ in 0..streams {
                let socket = net::create_udp_socket(0, address_family).await?;
                data_ports.push(socket.local_addr()?.port());
                debug!("UDP port {} allocated", data_ports.last().unwrap());
                sockets.push(Arc::new(socket));
            }
            (Vec::new(), sockets)
        }
    };

    // Send test ack with allocated ports
    let ack = ControlMessage::TestAck {
        id: id.to_string(),
        data_ports: data_ports.clone(),
    };
    writer
        .write_all(format!("{}\n", ack.serialize()?).as_bytes())
        .await?;

    // Create test stats
    let stats = Arc::new(TestStats::new(id.to_string(), streams));
    let (cancel_tx, cancel_rx) = watch::channel(false);

    // Store active test
    {
        let mut tests = active_tests.lock().await;
        tests.insert(
            id.to_string(),
            ActiveTest {
                stats: stats.clone(),
                cancel_tx,
                data_ports: data_ports.clone(),
            },
        );
    }

    // Notify metrics that test started
    #[cfg(feature = "prometheus")]
    crate::output::prometheus::on_test_start();

    // Spawn data stream handlers
    let handles: Vec<JoinHandle<()>> = match protocol {
        Protocol::Tcp => {
            spawn_tcp_handlers(
                tcp_listeners,
                stats.clone(),
                direction,
                duration,
                cancel_rx.clone(),
            )
            .await
        }
        Protocol::Udp => {
            spawn_udp_handlers(
                udp_sockets,
                stats.clone(),
                direction,
                duration,
                bitrate.unwrap_or(1_000_000_000),
                cancel_rx.clone(),
            )
            .await
        }
    };

    // Send interval updates
    let mut interval_timer = tokio::time::interval(Duration::from_secs(1));
    let start = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = interval_timer.tick() => {
                if start.elapsed() >= duration {
                    break;
                }

                let intervals = stats.record_intervals();
                let stream_intervals: Vec<StreamInterval> = stats.streams.iter()
                    .zip(intervals.iter())
                    .map(|(s, i)| s.to_interval(i))
                    .collect();
                let aggregate = stats.to_aggregate(&intervals);

                let interval_msg = ControlMessage::Interval {
                    id: id.to_string(),
                    elapsed_ms: stats.elapsed_ms(),
                    streams: stream_intervals,
                    aggregate,
                };

                if writer.write_all(format!("{}\n", interval_msg.serialize()?).as_bytes()).await.is_err() {
                    warn!("Failed to send interval, client may have disconnected");
                    break;
                }
            }
        }

        // Check for cancel message
        line.clear();
        let read_result =
            tokio::time::timeout(Duration::from_millis(10), reader.read_line(&mut line)).await;

        if let Ok(Ok(n)) = read_result
            && n > 0
            && let Ok(ControlMessage::Cancel {
                id: cancel_id,
                reason,
            }) = ControlMessage::deserialize(line.trim())
            && cancel_id == id
        {
            info!("Test {} cancelled: {}", id, reason);
            if let Some(test) = active_tests.lock().await.get(id) {
                let _ = test.cancel_tx.send(true);
            }
            let cancelled = ControlMessage::Cancelled { id: id.to_string() };
            writer
                .write_all(format!("{}\n", cancelled.serialize()?).as_bytes())
                .await?;
            break;
        }
    }

    // Signal handlers to stop
    if let Some(test) = active_tests.lock().await.get(id) {
        let _ = test.cancel_tx.send(true);
    }

    // Wait for all data handlers to complete
    futures::future::join_all(handles).await;

    // Send final result
    let duration_ms = stats.elapsed_ms();
    let bytes_total = stats.total_bytes();
    let throughput_mbps = (bytes_total as f64 * 8.0) / (duration_ms as f64 / 1000.0) / 1_000_000.0;

    let stream_results: Vec<_> = stats
        .streams
        .iter()
        .map(|s| s.to_result(duration_ms))
        .collect();

    let result = ControlMessage::Result(TestResult {
        id: id.to_string(),
        bytes_total,
        duration_ms,
        throughput_mbps,
        streams: stream_results,
        tcp_info: stats.get_tcp_info(),
        udp_stats: stats.aggregate_udp_stats(),
    });

    writer
        .write_all(format!("{}\n", result.serialize()?).as_bytes())
        .await?;

    // Notify metrics that test completed
    #[cfg(feature = "prometheus")]
    crate::output::prometheus::on_test_complete(&stats);

    // Cleanup
    active_tests.lock().await.remove(id);
    info!(
        "Test {} complete: {:.2} Mbps, {} bytes",
        id, throughput_mbps, bytes_total
    );

    Ok((bytes_total, duration_ms, throughput_mbps))
}

/// Read a line with bounded length to prevent memory DoS
async fn read_bounded_line(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    buf: &mut String,
) -> anyhow::Result<usize> {
    buf.clear();
    let mut total = 0;
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            return Ok(total);
        }

        if let Some(newline_pos) = bytes.iter().position(|&b| b == b'\n') {
            let to_read = newline_pos + 1;
            if total + to_read > MAX_LINE_LENGTH {
                return Err(anyhow::anyhow!("Line exceeds maximum length"));
            }
            buf.push_str(std::str::from_utf8(&bytes[..to_read])?);
            reader.consume(to_read);
            return Ok(total + to_read);
        }

        let len = bytes.len();
        if total + len > MAX_LINE_LENGTH {
            return Err(anyhow::anyhow!("Line exceeds maximum length"));
        }
        buf.push_str(std::str::from_utf8(bytes)?);
        reader.consume(len);
        total += len;
    }
}

async fn spawn_tcp_handlers(
    listeners: Vec<TcpListener>,
    stats: Arc<TestStats>,
    direction: Direction,
    duration: Duration,
    cancel: watch::Receiver<bool>,
) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();

    for (i, listener) in listeners.into_iter().enumerate() {
        let cancel = cancel.clone();
        let stream_stats = stats.streams[i].clone();
        let test_stats = stats.clone();

        let handle = tokio::spawn(async move {
            // Timeout on accept to prevent blocking forever if client never connects
            let accept_result =
                tokio::time::timeout(Duration::from_secs(10), listener.accept()).await;

            let stream = match accept_result {
                Ok(Ok((stream, _))) => stream,
                Ok(Err(e)) => {
                    warn!("Accept error: {}", e);
                    return;
                }
                Err(_) => {
                    warn!("Accept timeout - client never connected to data port");
                    return;
                }
            };

            // Use high-speed config for server - we want maximum throughput
            let config = TcpConfig::high_speed();

            // Capture TCP_INFO before transfer starts
            if let Some(info) = tcp::get_stream_tcp_info(&stream) {
                test_stats.add_tcp_info(info);
            }

            match direction {
                Direction::Upload => {
                    // Server receives data
                    let _ = tcp::receive_data(stream, stream_stats.clone(), cancel, config).await;
                }
                Direction::Download => {
                    // Server sends data
                    let _ = tcp::send_data(stream, stream_stats.clone(), duration, config, cancel)
                        .await;
                }
                Direction::Bidir => {
                    // Split socket for concurrent send/receive
                    let (read_half, write_half) = stream.into_split();

                    let send_stats = stream_stats.clone();
                    let recv_stats = stream_stats.clone();
                    let send_cancel = cancel.clone();
                    let recv_cancel = cancel;

                    let send_config = TcpConfig {
                        buffer_size: config.buffer_size,
                        nodelay: config.nodelay,
                        window_size: config.window_size,
                    };
                    let recv_config = config;

                    let send_handle = tokio::spawn(async move {
                        let _ = tcp::send_data_half(
                            write_half,
                            send_stats,
                            duration,
                            send_config,
                            send_cancel,
                        )
                        .await;
                    });

                    let recv_handle = tokio::spawn(async move {
                        let _ =
                            tcp::receive_data_half(read_half, recv_stats, recv_cancel, recv_config)
                                .await;
                    });

                    let _ = tokio::join!(send_handle, recv_handle);
                }
            }
        });
        handles.push(handle);
    }

    handles
}

async fn spawn_udp_handlers(
    sockets: Vec<Arc<UdpSocket>>,
    stats: Arc<TestStats>,
    direction: Direction,
    duration: Duration,
    bitrate: u64,
    cancel: watch::Receiver<bool>,
) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();

    for (i, socket) in sockets.into_iter().enumerate() {
        let stream_stats = stats.streams[i].clone();
        let test_stats = stats.clone();
        let cancel = cancel.clone();

        let handle = tokio::spawn(async move {
            match direction {
                Direction::Upload => {
                    // Server receives UDP - capture stats
                    if let Ok((udp_stats, _bytes)) =
                        udp::receive_udp(socket, stream_stats, cancel).await
                    {
                        test_stats.add_udp_stats(udp_stats);
                    }
                }
                Direction::Download => {
                    // Server sends UDP
                    let _ =
                        udp::send_udp_paced(socket, bitrate, duration, stream_stats, cancel).await;
                }
                Direction::Bidir => {
                    // UDP can send/receive concurrently on same socket
                    let send_socket = socket.clone();
                    let recv_socket = socket;
                    let send_stats = stream_stats.clone();
                    let recv_stats = stream_stats;
                    let send_cancel = cancel.clone();
                    let recv_cancel = cancel;
                    let test_stats_copy = test_stats.clone();

                    let send_handle = tokio::spawn(async move {
                        let _ = udp::send_udp_paced(
                            send_socket,
                            bitrate,
                            duration,
                            send_stats,
                            send_cancel,
                        )
                        .await;
                    });

                    let recv_handle = tokio::spawn(async move {
                        if let Ok((udp_stats, _bytes)) =
                            udp::receive_udp(recv_socket, recv_stats, recv_cancel).await
                        {
                            test_stats_copy.add_udp_stats(udp_stats);
                        }
                    });

                    let _ = tokio::join!(send_handle, recv_handle);
                }
            }
        });
        handles.push(handle);
    }

    handles
}

/// Register mDNS service for server discovery
#[cfg(feature = "discovery")]
fn register_mdns_service(port: u16) -> Option<mdns_sd::ServiceDaemon> {
    // Use the shared registration from discover module
    match crate::discover::register_server(port) {
        Ok(mdns) => Some(mdns),
        Err(e) => {
            warn!("Failed to register mDNS service: {}", e);
            None
        }
    }
}
