//! Server mode implementation
//!
//! Listens for incoming connections and handles bandwidth tests.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Mutex, Semaphore, watch};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::acl::{Acl, AclConfig};
use crate::auth::{self, AuthConfig};
use crate::net::{self, AddressFamily};
use crate::protocol::{
    ControlMessage, Direction, PROTOCOL_VERSION, Protocol, StreamInterval, TestResult,
    versions_compatible,
};
use crate::quic;
use crate::rate_limit::{RateLimitConfig, RateLimitGuard, RateLimiter};
use crate::stats::TestStats;
use crate::tcp::{self, TcpConfig};
use crate::tui::server::{ActiveTestInfo, ServerEvent};
use crate::udp;
use tokio::sync::mpsc;

/// Maximum control message line length to prevent memory DoS
const MAX_LINE_LENGTH: usize = 8192;
/// Maximum streams a client can request
const MAX_STREAMS: u8 = 128;
/// Maximum test duration a client can request (1 hour)
const MAX_TEST_DURATION: Duration = Duration::from_secs(3600);
/// Timeout for control-plane handshake reads (prevents DoS from idle connections)
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Access control list configuration
    pub acl: AclConfig,
    /// Rate limiting configuration
    pub rate_limit: RateLimitConfig,
    /// Address family (IPv4, IPv6, dual-stack)
    pub address_family: AddressFamily,
    /// Channel to send events to TUI
    pub tui_tx: Option<mpsc::Sender<ServerEvent>>,
    /// Enable QUIC protocol support (binds additional UDP port)
    pub enable_quic: bool,
    /// Maximum concurrent client handlers (defense against connection floods)
    pub max_concurrent: u32,
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
            acl: AclConfig::default(),
            rate_limit: RateLimitConfig::default(),
            address_family: AddressFamily::default(),
            tui_tx: None,
            enable_quic: true,
            max_concurrent: 100,
        }
    }
}

/// Security context shared across client handlers
struct SecurityContext {
    psk: Option<String>,
    acl: Acl,
    rate_limiter: Option<Arc<RateLimiter>>,
    address_family: AddressFamily,
    tui_tx: Option<mpsc::Sender<ServerEvent>>,
    push_gateway_url: Option<String>,
}

struct ActiveTest {
    #[allow(dead_code)]
    stats: Arc<TestStats>,
    #[allow(dead_code)]
    cancel_tx: watch::Sender<bool>,
    #[allow(dead_code)]
    data_ports: Vec<u16>,
    /// Channel for receiving data connections in single-port TCP mode
    #[allow(dead_code)]
    data_stream_tx: Option<mpsc::Sender<(TcpStream, u16)>>, // (stream, stream_index)
    /// Control connection peer IP (for DataHello validation)
    control_peer_ip: std::net::IpAddr,
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

        // Create QUIC endpoint on the same port (UDP) - only if enabled
        let quic_endpoint = if self.config.enable_quic {
            let (cert, key) = quic::generate_self_signed_cert()?;
            let bind_addr: SocketAddr = match self.config.address_family {
                AddressFamily::V4Only => format!("0.0.0.0:{}", self.config.port).parse()?,
                AddressFamily::V6Only | AddressFamily::DualStack => {
                    format!("[::]:{}", self.config.port).parse()?
                }
            };
            let endpoint = quic::create_server_endpoint(bind_addr, cert, key)?;
            info!("QUIC endpoint ready on port {}", self.config.port);
            Some(endpoint)
        } else {
            None
        };

        // Initialize security context
        let acl = self.config.acl.build()?;
        let rate_limiter = self.config.rate_limit.build();

        if self.config.auth.psk.is_some() {
            info!("PSK authentication enabled");
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

        let security = Arc::new(SecurityContext {
            psk: self.config.auth.psk.clone(),
            acl,
            rate_limiter: rate_limiter.clone(),
            address_family: self.config.address_family,
            tui_tx: self.config.tui_tx.clone(),
            push_gateway_url: self.config.push_gateway_url.clone(),
        });

        // Start rate limiter cleanup task if enabled
        if let Some(limiter) = rate_limiter.clone() {
            limiter.start_cleanup_task();
        }

        // Semaphore to limit concurrent handlers (defense against connection floods)
        let handler_semaphore = Arc::new(Semaphore::new(self.config.max_concurrent as usize));

        // Spawn QUIC acceptor task (only if QUIC is enabled)
        if let Some(quic_endpoint) = quic_endpoint {
            let quic_security = security.clone();
            let quic_active_tests = self.active_tests.clone();
            let quic_max_duration = self.config.max_duration;
            let quic_rate_limiter = rate_limiter.clone();
            let quic_semaphore = handler_semaphore.clone();
            tokio::spawn(async move {
                while let Some(incoming) = quic_endpoint.accept().await {
                    let peer_addr = incoming.remote_address();
                    let peer_ip = peer_addr.ip();

                    // Check ACL
                    if !quic_security.acl.is_allowed(peer_ip) {
                        warn!("QUIC connection rejected by ACL: {}", peer_addr);
                        if let Some(tx) = &quic_security.tui_tx {
                            let _ = tx.try_send(ServerEvent::ConnectionBlocked);
                        }
                        continue;
                    }

                    // Check rate limit
                    if let Some(ref limiter) = quic_rate_limiter
                        && let Err(e) = limiter.check(peer_ip)
                    {
                        warn!("QUIC rate limit exceeded for {}: {}", peer_addr, e);
                        if let Some(tx) = &quic_security.tui_tx {
                            let _ = tx.try_send(ServerEvent::ConnectionBlocked);
                        }
                        continue;
                    }

                    // Acquire semaphore permit to limit concurrent handlers
                    let permit = match quic_semaphore.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            warn!(
                                "Max concurrent handlers reached, rejecting QUIC: {}",
                                peer_addr
                            );
                            continue;
                        }
                    };

                    info!("QUIC client connected: {}", peer_addr);

                    let security = quic_security.clone();
                    let active_tests = quic_active_tests.clone();
                    let rate_limiter = quic_rate_limiter.clone();

                    tokio::spawn(async move {
                        let _permit = permit; // Held until task completes

                        let result = handle_quic_client(
                            incoming,
                            peer_addr,
                            active_tests,
                            quic_max_duration,
                            &security,
                        )
                        .await;

                        // Release rate limit slot
                        if let Some(limiter) = &rate_limiter {
                            limiter.release(peer_ip);
                        }

                        if let Err(e) = result {
                            error!("QUIC client error {}: {}", peer_addr, e);
                        }
                    });
                }
            });
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
            let (mut stream, peer_addr) = listener.accept().await?;
            let peer_ip = peer_addr.ip();

            // Check ACL (cheap, no state change)
            if !security.acl.is_allowed(peer_ip) {
                warn!("Connection rejected by ACL: {}", peer_addr);
                if let Some(tx) = &security.tui_tx {
                    let _ = tx.try_send(ServerEvent::ConnectionBlocked);
                }
                drop(stream);
                continue;
            }

            // Read first line to determine connection type BEFORE acquiring permits
            // This allows DataHello (data connections) to bypass rate-limit and semaphore
            let line = match tokio::time::timeout(
                HANDSHAKE_TIMEOUT,
                read_first_line_unbuffered(&mut stream, MAX_LINE_LENGTH),
            )
            .await
            {
                Ok(Ok(line)) => line,
                Ok(Err(e)) => {
                    debug!("Failed to read first line from {}: {}", peer_addr, e);
                    continue;
                }
                Err(_) => {
                    debug!("Handshake timeout from {}", peer_addr);
                    continue;
                }
            };

            let msg: ControlMessage = match ControlMessage::deserialize(line.trim()) {
                Ok(msg) => msg,
                Err(e) => {
                    debug!("Invalid first message from {}: {}", peer_addr, e);
                    continue;
                }
            };

            match msg {
                ControlMessage::DataHello {
                    test_id,
                    stream_index,
                } => {
                    // Data connection - route directly without consuming rate-limit or semaphore
                    // (the control connection already acquired those resources)
                    let active_tests = self.active_tests.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            route_data_hello(stream, peer_addr, active_tests, test_id, stream_index)
                                .await
                        {
                            debug!("DataHello routing error from {}: {}", peer_addr, e);
                        }
                    });
                }
                ControlMessage::Hello { .. } => {
                    // Control connection - acquire rate limit and semaphore
                    let rate_limit_guard = if let Some(limiter) = &security.rate_limiter {
                        if let Err(e) = limiter.check(peer_ip) {
                            warn!("Rate limit exceeded for {}: {}", peer_addr, e);
                            if let Some(tx) = &security.tui_tx {
                                let _ = tx.try_send(ServerEvent::ConnectionBlocked);
                            }
                            continue;
                        }
                        Some(RateLimitGuard::new(limiter.clone(), peer_ip))
                    } else {
                        None
                    };

                    let permit = match handler_semaphore.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            warn!("Max concurrent handlers reached, rejecting: {}", peer_addr);
                            // rate_limit_guard drops here, releasing the slot
                            continue;
                        }
                    };

                    info!("Client connected: {}", peer_addr);

                    let active_tests = self.active_tests.clone();
                    let base_port = self.config.port;
                    let max_duration = self.config.max_duration;
                    let security = security.clone();

                    let handle = tokio::spawn(async move {
                        let _permit = permit; // Held until task completes
                        let _rate_guard = rate_limit_guard; // Released on drop (even on panic)

                        let result = handle_client_with_first_message(
                            stream,
                            peer_addr,
                            active_tests,
                            base_port,
                            max_duration,
                            &security,
                            msg,
                        )
                        .await;

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
                other => {
                    warn!("Unexpected first message from {}: {:?}", peer_addr, other);
                }
            }
        }

        Ok(())
    }
}

/// Read first line without buffering (to avoid losing data after DataHello)
async fn read_first_line_unbuffered(
    stream: &mut TcpStream,
    max_len: usize,
) -> anyhow::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut line = Vec::with_capacity(256);
    let mut buf = [0u8; 1];

    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Err(anyhow::anyhow!("Connection closed"));
        }
        if buf[0] == b'\n' {
            break;
        }
        line.push(buf[0]);

        // DoS guard: reject lines that exceed maximum length
        if line.len() >= max_len {
            return Err(anyhow::anyhow!(
                "Line exceeds maximum length of {} bytes",
                max_len
            ));
        }
    }

    Ok(String::from_utf8_lossy(&line).to_string())
}

/// Route DataHello connection to its test (no rate-limit/semaphore needed)
async fn route_data_hello(
    stream: TcpStream,
    peer_addr: SocketAddr,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    test_id: String,
    stream_index: u16,
) -> anyhow::Result<()> {
    debug!(
        "DataHello from {} for test {} stream {}",
        peer_addr, test_id, stream_index
    );

    // Look up the test and validate peer IP matches control connection
    let tx = {
        let tests = active_tests.lock().await;
        if let Some(test) = tests.get(&test_id) {
            // Security: Validate DataHello comes from same IP as control connection
            // Use normalize_ip to handle IPv4-mapped IPv6 addresses (::ffff:x.x.x.x vs x.x.x.x)
            let expected_ip = net::normalize_ip(test.control_peer_ip);
            let actual_ip = net::normalize_ip(peer_addr.ip());
            if expected_ip != actual_ip {
                warn!(
                    "DataHello IP mismatch for test {}: expected {}, got {}",
                    test_id,
                    test.control_peer_ip,
                    peer_addr.ip()
                );
                return Err(anyhow::anyhow!("DataHello from unauthorized IP"));
            }
            test.data_stream_tx.clone()
        } else {
            None
        }
    };

    if let Some(tx) = tx {
        // Stream is ready for data transfer
        if tx.send((stream, stream_index)).await.is_err() {
            warn!("Failed to route data stream - test may have ended");
        }
    } else {
        warn!(
            "DataHello for unknown/completed test {} from {}",
            test_id, peer_addr
        );
    }

    Ok(())
}

/// Route incoming connection: either DataHello (route to test) or Hello (control connection)
/// Note: Currently unused - kept for potential multi-port mode fallback
#[allow(dead_code)]
async fn route_connection(
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    base_port: u16,
    server_max_duration: Option<Duration>,
    security: &SecurityContext,
) -> anyhow::Result<()> {
    // Read first line without buffering to avoid losing data bytes after DataHello
    let line = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        read_first_line_unbuffered(&mut stream, MAX_LINE_LENGTH),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Handshake timeout"))??;

    let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

    match msg {
        ControlMessage::DataHello {
            test_id,
            stream_index,
        } => {
            // This is a data connection for an active test
            debug!(
                "DataHello from {} for test {} stream {}",
                peer_addr, test_id, stream_index
            );

            // Look up the test and validate peer IP matches control connection
            let tx = {
                let tests = active_tests.lock().await;
                if let Some(test) = tests.get(&test_id) {
                    // Security: Validate DataHello comes from same IP as control connection
                    // Use normalize_ip to handle IPv4-mapped IPv6 addresses (::ffff:x.x.x.x vs x.x.x.x)
                    let expected_ip = net::normalize_ip(test.control_peer_ip);
                    let actual_ip = net::normalize_ip(peer_addr.ip());
                    if expected_ip != actual_ip {
                        warn!(
                            "DataHello IP mismatch for test {}: expected {}, got {}",
                            test_id,
                            test.control_peer_ip,
                            peer_addr.ip()
                        );
                        return Err(anyhow::anyhow!("DataHello from unauthorized IP"));
                    }
                    test.data_stream_tx.clone()
                } else {
                    None
                }
            };

            if let Some(tx) = tx {
                // Stream is ready for data transfer (no buffered bytes lost)
                if tx.send((stream, stream_index)).await.is_err() {
                    warn!("Failed to route data stream - test may have ended");
                }
            } else {
                warn!(
                    "DataHello for unknown/completed test {} from {}",
                    test_id, peer_addr
                );
            }
            Ok(())
        }
        ControlMessage::Hello { .. } => {
            // This is a control connection - proceed with normal handling
            handle_client_with_first_message(
                stream,
                peer_addr,
                active_tests,
                base_port,
                server_max_duration,
                security,
                msg,
            )
            .await
        }
        other => {
            warn!("Unexpected first message from {}: {:?}", peer_addr, other);
            Err(anyhow::anyhow!("Expected Hello or DataHello"))
        }
    }
}

/// Handle client with security checks (auth)
/// Note: Currently unused - kept for potential multi-port mode fallback
#[allow(dead_code)]
async fn handle_client_secure(
    stream: TcpStream,
    peer_addr: SocketAddr,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    base_port: u16,
    server_max_duration: Option<Duration>,
    security: &SecurityContext,
) -> anyhow::Result<()> {
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

/// Handle QUIC client connection
async fn handle_quic_client(
    incoming: quinn::Incoming,
    peer_addr: SocketAddr,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    server_max_duration: Option<Duration>,
    security: &SecurityContext,
) -> anyhow::Result<()> {
    use tokio::io::BufReader;

    let connection = incoming.await?;
    debug!("QUIC connection established with {}", peer_addr);

    // Accept control stream (bidirectional)
    let (mut ctrl_send, ctrl_recv) = connection.accept_bi().await?;
    let mut ctrl_reader = BufReader::new(ctrl_recv);
    let mut line = String::new();

    // Read client hello (bounded to prevent DoS, with timeout)
    tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        read_bounded_line(&mut ctrl_reader, &mut line),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Handshake timeout waiting for hello"))??;
    let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

    let auth_nonce = match msg {
        ControlMessage::Hello { version, .. } => {
            if !versions_compatible(&version, PROTOCOL_VERSION) {
                let error = ControlMessage::error(format!(
                    "Incompatible protocol version: {} (server: {})",
                    version, PROTOCOL_VERSION
                ));
                ctrl_send
                    .write_all(format!("{}\n", error.serialize()?).as_bytes())
                    .await?;
                return Err(anyhow::anyhow!("Protocol version mismatch"));
            }

            // Send server hello (with auth challenge if required)
            if security.psk.is_some() {
                let nonce = auth::generate_nonce();
                let hello = ControlMessage::server_hello_with_auth(nonce.clone());
                ctrl_send
                    .write_all(format!("{}\n", hello.serialize()?).as_bytes())
                    .await?;
                Some(nonce)
            } else {
                let hello = ControlMessage::server_hello();
                ctrl_send
                    .write_all(format!("{}\n", hello.serialize()?).as_bytes())
                    .await?;
                None
            }
        }
        _ => {
            let error = ControlMessage::error("Expected hello message");
            ctrl_send
                .write_all(format!("{}\n", error.serialize()?).as_bytes())
                .await?;
            return Err(anyhow::anyhow!("Expected hello message"));
        }
    };

    // Handle authentication if required
    if let Some(nonce) = auth_nonce {
        tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            read_bounded_line(&mut ctrl_reader, &mut line),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Handshake timeout waiting for auth"))??;
        let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

        match msg {
            ControlMessage::AuthResponse { response } => {
                let psk = security.psk.as_ref().unwrap();
                if !auth::verify_response(&nonce, psk, &response) {
                    if let Some(tx) = &security.tui_tx {
                        let _ = tx.try_send(ServerEvent::AuthFailure);
                    }
                    let error = ControlMessage::error("Authentication failed");
                    ctrl_send
                        .write_all(format!("{}\n", error.serialize()?).as_bytes())
                        .await?;
                    return Err(anyhow::anyhow!("Authentication failed"));
                }

                let success = ControlMessage::auth_success();
                ctrl_send
                    .write_all(format!("{}\n", success.serialize()?).as_bytes())
                    .await?;
            }
            _ => {
                let error = ControlMessage::error("Expected auth response");
                ctrl_send
                    .write_all(format!("{}\n", error.serialize()?).as_bytes())
                    .await?;
                return Err(anyhow::anyhow!("Expected auth response"));
            }
        }
    }

    // Read test start (with timeout)
    tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        read_bounded_line(&mut ctrl_reader, &mut line),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Handshake timeout waiting for test start"))??;
    let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

    match msg {
        ControlMessage::TestStart {
            id,
            protocol,
            streams,
            duration_secs,
            direction,
            bitrate: _,
        } => {
            if protocol != Protocol::Quic {
                let error = ControlMessage::error("Expected QUIC protocol for QUIC connection");
                ctrl_send
                    .write_all(format!("{}\n", error.serialize()?).as_bytes())
                    .await?;
                return Err(anyhow::anyhow!("Protocol mismatch"));
            }

            if streams == 0 || streams > MAX_STREAMS {
                let error = ControlMessage::error(format!(
                    "Invalid stream count {} (must be 1-{})",
                    streams, MAX_STREAMS
                ));
                ctrl_send
                    .write_all(format!("{}\n", error.serialize()?).as_bytes())
                    .await?;
                return Err(anyhow::anyhow!("Invalid stream count"));
            }

            let mut duration = Duration::from_secs(duration_secs as u64);
            // Handle infinite duration (0) - apply server max if set
            if duration == Duration::ZERO {
                if let Some(max_dur) = server_max_duration {
                    duration = max_dur;
                    warn!(
                        "Infinite duration requested, capped to server max {}s",
                        max_dur.as_secs()
                    );
                }
                // else: allow infinite if no server max
            } else {
                if duration > MAX_TEST_DURATION {
                    duration = MAX_TEST_DURATION;
                }
                if let Some(max_dur) = server_max_duration
                    && duration > max_dur
                {
                    duration = max_dur;
                }
            }

            let duration_display = if duration == Duration::ZERO {
                "∞".to_string()
            } else {
                format!("{}s", duration.as_secs())
            };
            info!(
                "QUIC test requested: {} streams, {} mode, {}",
                streams, direction, duration_display
            );

            // Send test ack (no data ports for QUIC - streams are multiplexed)
            let ack = ControlMessage::TestAck {
                id: id.clone(),
                data_ports: vec![], // Empty for QUIC
            };
            ctrl_send
                .write_all(format!("{}\n", ack.serialize()?).as_bytes())
                .await?;

            // Run QUIC test
            run_quic_test(
                &connection,
                ctrl_reader,
                ctrl_send,
                &id,
                streams,
                duration,
                direction,
                active_tests,
                peer_addr,
                security.tui_tx.clone(),
                &security.push_gateway_url,
            )
            .await?;
        }
        _ => {
            let error = ControlMessage::error("Expected test_start message");
            ctrl_send
                .write_all(format!("{}\n", error.serialize()?).as_bytes())
                .await?;
            return Err(anyhow::anyhow!("Expected test_start"));
        }
    }

    Ok(())
}

/// Handle client with pre-read first message (Hello already parsed in route_connection)
async fn handle_client_with_first_message(
    stream: TcpStream,
    peer_addr: SocketAddr,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    _base_port: u16,
    server_max_duration: Option<Duration>,
    security: &SecurityContext,
    first_msg: ControlMessage,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    // Process pre-read Hello message
    let auth_nonce = match first_msg {
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
                Some(nonce)
            } else {
                let hello = ControlMessage::server_hello();
                writer
                    .write_all(format!("{}\n", hello.serialize()?).as_bytes())
                    .await?;
                None
            }
        }
        _ => {
            return Err(anyhow::anyhow!("Expected Hello message"));
        }
    };

    // Handle auth response if needed
    if let Some(nonce) = auth_nonce {
        tokio::time::timeout(HANDSHAKE_TIMEOUT, read_bounded_line(&mut reader, &mut line))
            .await
            .map_err(|_| anyhow::anyhow!("Handshake timeout waiting for auth"))??;
        let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

        match msg {
            ControlMessage::AuthResponse { response } => {
                let psk = security.psk.as_ref().unwrap();
                if !auth::verify_response(&nonce, psk, &response) {
                    if let Some(tx) = &security.tui_tx {
                        let _ = tx.try_send(ServerEvent::AuthFailure);
                    }
                    let error = ControlMessage::error("Authentication failed");
                    writer
                        .write_all(format!("{}\n", error.serialize()?).as_bytes())
                        .await?;
                    return Err(anyhow::anyhow!("Authentication failed"));
                }

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

    // Continue with test handling
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

/// Handle client with authentication (for plain TCP)
#[allow(dead_code)]
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
        tokio::time::timeout(HANDSHAKE_TIMEOUT, read_bounded_line(&mut reader, &mut line))
            .await
            .map_err(|_| anyhow::anyhow!("Handshake timeout waiting for auth"))??;
        let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

        match msg {
            ControlMessage::AuthResponse { response } => {
                let psk = security.psk.as_ref().unwrap();
                if !auth::verify_response(&nonce, psk, &response) {
                    if let Some(tx) = &security.tui_tx {
                        let _ = tx.try_send(ServerEvent::AuthFailure);
                    }
                    let error = ControlMessage::error("Authentication failed");
                    writer
                        .write_all(format!("{}\n", error.serialize()?).as_bytes())
                        .await?;
                    return Err(anyhow::anyhow!("Authentication failed"));
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

/// Perform authentication handshake, returns nonce if auth was required
#[allow(dead_code)]
async fn perform_auth_handshake<W: tokio::io::AsyncWrite + Unpin>(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: &mut W,
    security: &SecurityContext,
) -> anyhow::Result<Option<String>> {
    let mut line = String::new();

    // Read client hello (with timeout)
    tokio::time::timeout(HANDSHAKE_TIMEOUT, read_bounded_line(reader, &mut line))
        .await
        .map_err(|_| anyhow::anyhow!("Handshake timeout waiting for hello"))??;
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

    // Read test request (with timeout)
    tokio::time::timeout(HANDSHAKE_TIMEOUT, read_bounded_line(reader, &mut line))
        .await
        .map_err(|_| anyhow::anyhow!("Handshake timeout waiting for test start"))??;
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
            if streams == 0 || streams > MAX_STREAMS {
                let error = ControlMessage::error(format!(
                    "Invalid stream count {} (must be 1-{})",
                    streams, MAX_STREAMS
                ));
                writer
                    .write_all(format!("{}\n", error.serialize()?).as_bytes())
                    .await?;
                return Err(anyhow::anyhow!("Invalid stream count"));
            }

            // Calculate effective duration
            let mut duration = Duration::from_secs(duration_secs as u64);
            // Handle infinite duration (0) - apply server max if set
            if duration == Duration::ZERO {
                if let Some(max_dur) = server_max_duration {
                    duration = max_dur;
                    warn!(
                        "Infinite duration requested, capped to server max {}s",
                        max_dur.as_secs()
                    );
                }
                // else: allow infinite if no server max
            } else {
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
            }

            let duration_display = if duration == Duration::ZERO {
                "∞".to_string()
            } else {
                format!("{}s", duration.as_secs())
            };
            info!(
                "Test requested: {} {} streams, {} mode, {}",
                protocol, streams, direction, duration_display
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
                peer_addr,
                security.tui_tx.clone(),
                &security.push_gateway_url,
            )
            .await;

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

/// Run a QUIC bandwidth test
#[allow(clippy::too_many_arguments)]
async fn run_quic_test(
    connection: &quinn::Connection,
    mut ctrl_reader: BufReader<quinn::RecvStream>,
    mut ctrl_send: quinn::SendStream,
    id: &str,
    streams: u8,
    duration: Duration,
    direction: Direction,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    peer_addr: SocketAddr,
    tui_tx: Option<mpsc::Sender<ServerEvent>>,
    push_gateway_url: &Option<String>,
) -> anyhow::Result<()> {
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
                data_ports: vec![],
                data_stream_tx: None,
                control_peer_ip: peer_addr.ip(),
            },
        );
    }

    // Notify TUI
    if let Some(tx) = &tui_tx {
        let _ = tx.try_send(ServerEvent::TestStarted(ActiveTestInfo {
            id: id.to_string(),
            client_ip: peer_addr.ip(),
            protocol: "QUIC".to_string(),
            direction: direction.to_string(),
            streams,
            started: std::time::Instant::now(),
            duration_secs: duration.as_secs() as u32,
            bytes: 0,
            throughput_mbps: 0.0,
        }));
    }

    #[cfg(feature = "prometheus")]
    crate::output::prometheus::on_test_start();

    // Spawn data stream handlers
    let mut handles = Vec::new();
    for i in 0..streams {
        let stream_stats = stats.streams[i as usize].clone();
        let cancel = cancel_rx.clone();
        let conn = connection.clone();

        let handle = tokio::spawn(async move {
            match direction {
                Direction::Upload => {
                    // Server receives - accept uni stream from client with timeout
                    let mut cancel_rx = cancel.clone();
                    let accept_result = tokio::select! {
                        result = conn.accept_uni() => result.ok(),
                        _ = tokio::time::sleep(HANDSHAKE_TIMEOUT) => {
                            debug!("Timeout waiting for client to open uni stream");
                            None
                        }
                        _ = cancel_rx.changed() => {
                            debug!("Cancelled while waiting for uni stream");
                            None
                        }
                    };
                    if let Some(recv) = accept_result
                        && let Err(e) = quic::receive_quic_data(recv, stream_stats, cancel).await
                    {
                        debug!("QUIC receive ended: {}", e);
                    }
                }
                Direction::Download => {
                    // Server sends - open uni stream to client
                    match conn.open_uni().await {
                        Ok(send) => {
                            if let Err(e) =
                                quic::send_quic_data(send, stream_stats, duration, cancel).await
                            {
                                debug!("QUIC send ended: {}", e);
                            }
                        }
                        Err(e) => debug!("Failed to open uni stream: {}", e),
                    }
                }
                Direction::Bidir => {
                    // Accept bidir stream from client with timeout
                    let mut cancel_rx = cancel.clone();
                    let accept_result = tokio::select! {
                        result = conn.accept_bi() => result.ok(),
                        _ = tokio::time::sleep(HANDSHAKE_TIMEOUT) => {
                            debug!("Timeout waiting for client to open bi stream");
                            None
                        }
                        _ = cancel_rx.changed() => {
                            debug!("Cancelled while waiting for bi stream");
                            None
                        }
                    };
                    if let Some((send, recv)) = accept_result {
                        let send_stats = stream_stats.clone();
                        let recv_stats = stream_stats;
                        let send_cancel = cancel.clone();
                        let recv_cancel = cancel;

                        let send_handle = tokio::spawn(async move {
                            let _ =
                                quic::send_quic_data(send, send_stats, duration, send_cancel).await;
                        });

                        let recv_handle = tokio::spawn(async move {
                            let _ = quic::receive_quic_data(recv, recv_stats, recv_cancel).await;
                        });

                        let _ = tokio::join!(send_handle, recv_handle);
                    }
                }
            }
        });
        handles.push(handle);
    }

    // Send interval updates
    let mut interval_timer = tokio::time::interval(Duration::from_secs(1));
    let start = std::time::Instant::now();
    let mut line = String::new();

    loop {
        tokio::select! {
            _ = interval_timer.tick() => {
                // Duration::ZERO means infinite - only break if duration is set
                if duration != Duration::ZERO && start.elapsed() >= duration {
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
                    aggregate: aggregate.clone(),
                };

                if let Some(tx) = &tui_tx {
                    let _ = tx.try_send(ServerEvent::TestUpdated {
                        id: id.to_string(),
                        bytes: aggregate.bytes,
                        throughput_mbps: aggregate.throughput_mbps,
                    });
                }

                if ctrl_send.write_all(format!("{}\n", interval_msg.serialize()?).as_bytes()).await.is_err() {
                    warn!("Failed to send interval");
                    break;
                }
            }
        }

        // Check for cancel message (non-blocking, bounded read)
        let read_result = tokio::time::timeout(
            Duration::from_millis(10),
            read_bounded_line(&mut ctrl_reader, &mut line),
        )
        .await;

        if let Ok(Ok(n)) = read_result
            && n > 0
            && let Ok(ControlMessage::Cancel {
                id: cancel_id,
                reason,
            }) = ControlMessage::deserialize(line.trim())
            && cancel_id == id
        {
            info!("QUIC test {} cancelled: {}", id, reason);
            if let Some(test) = active_tests.lock().await.get(id) {
                let _ = test.cancel_tx.send(true);
            }
            let cancelled = ControlMessage::Cancelled { id: id.to_string() };
            ctrl_send
                .write_all(format!("{}\n", cancelled.serialize()?).as_bytes())
                .await?;
            break;
        }
    }

    // Signal handlers to stop
    if let Some(test) = active_tests.lock().await.get(id) {
        let _ = test.cancel_tx.send(true);
    }

    // Wait for handlers to complete
    futures::future::join_all(handles).await;

    // Send final result
    let duration_ms = stats.elapsed_ms();
    let bytes_total = stats.total_bytes();
    let throughput_mbps = if duration_ms > 0 {
        (bytes_total as f64 * 8.0) / (duration_ms as f64 / 1000.0) / 1_000_000.0
    } else {
        0.0
    };

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
        tcp_info: None,
        udp_stats: None,
    });

    ctrl_send
        .write_all(format!("{}\n", result.serialize()?).as_bytes())
        .await?;

    // Finish the control stream to ensure result is sent
    ctrl_send.finish()?;

    // Give client time to receive the result before connection closes
    tokio::time::sleep(Duration::from_millis(100)).await;

    #[cfg(feature = "prometheus")]
    crate::output::prometheus::on_test_complete(&stats);

    // Push metrics to gateway if configured
    crate::output::push_gateway::maybe_push_metrics(push_gateway_url, &stats).await;

    if let Some(tx) = &tui_tx {
        let _ = tx.try_send(ServerEvent::TestCompleted {
            id: id.to_string(),
            bytes: bytes_total,
        });
    }

    active_tests.lock().await.remove(id);
    info!(
        "QUIC test {} complete: {:.2} Mbps, {} bytes",
        id, throughput_mbps, bytes_total
    );

    Ok(())
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
    peer_addr: SocketAddr,
    tui_tx: Option<mpsc::Sender<ServerEvent>>,
    push_gateway_url: &Option<String>,
) -> anyhow::Result<(u64, u64, f64)> {
    let mut line = String::new();

    // For TCP: single-port mode (data connections come on control port)
    // For UDP: allocate per-stream sockets
    let mut data_ports = Vec::new();
    let mut udp_sockets: Vec<Arc<UdpSocket>> = Vec::new();

    let (data_stream_tx, data_stream_rx) = match protocol {
        Protocol::Tcp => {
            // Single-port mode: data connections will be routed via channel
            let (tx, rx) = mpsc::channel::<(TcpStream, u16)>(streams as usize);
            (Some(tx), Some(rx))
        }
        Protocol::Udp => {
            for _ in 0..streams {
                let socket = net::create_udp_socket(0, address_family).await?;
                data_ports.push(socket.local_addr()?.port());
                debug!("UDP port {} allocated", data_ports.last().unwrap());
                udp_sockets.push(Arc::new(socket));
            }
            (None, None)
        }
        Protocol::Quic => {
            // QUIC uses its own connection model with multiplexed streams
            return Err(anyhow::anyhow!(
                "QUIC protocol requires QUIC endpoint, not TCP control"
            ));
        }
    };

    // Create test stats
    let stats = Arc::new(TestStats::new(id.to_string(), streams));
    let (cancel_tx, cancel_rx) = watch::channel(false);

    // Store active test BEFORE sending TestAck to avoid race condition
    // (client may send DataHello immediately after receiving TestAck)
    {
        let mut tests = active_tests.lock().await;
        tests.insert(
            id.to_string(),
            ActiveTest {
                stats: stats.clone(),
                cancel_tx,
                data_ports: data_ports.clone(),
                data_stream_tx,
                control_peer_ip: peer_addr.ip(),
            },
        );
    }

    // Send test ack with allocated ports
    let ack = ControlMessage::TestAck {
        id: id.to_string(),
        data_ports: data_ports.clone(),
    };
    writer
        .write_all(format!("{}\n", ack.serialize()?).as_bytes())
        .await?;

    // Notify TUI that test started
    if let Some(tx) = &tui_tx {
        let _ = tx.try_send(ServerEvent::TestStarted(ActiveTestInfo {
            id: id.to_string(),
            client_ip: peer_addr.ip(),
            protocol: protocol.to_string(),
            direction: direction.to_string(),
            streams,
            started: std::time::Instant::now(),
            duration_secs: duration.as_secs() as u32,
            bytes: 0,
            throughput_mbps: 0.0,
        }));
    }

    // Notify metrics that test started
    #[cfg(feature = "prometheus")]
    crate::output::prometheus::on_test_start();

    // Spawn data stream handlers
    // For TCP: spawn stream collection in background to not block interval loop
    // For UDP: handlers are spawned immediately
    let (tcp_collection_handle, udp_handles) = match protocol {
        Protocol::Tcp => {
            // Single-port mode: spawn stream collection in background
            // This allows interval loop and cancel handling to run while waiting for streams
            if let Some(rx) = data_stream_rx {
                let stats_clone = stats.clone();
                let cancel_clone = cancel_rx.clone();
                let handle = tokio::spawn(async move {
                    spawn_tcp_stream_handlers(
                        rx,
                        streams as usize,
                        stats_clone,
                        direction,
                        duration,
                        cancel_clone,
                    )
                    .await
                });
                (Some(handle), Vec::new())
            } else {
                (None, Vec::new())
            }
        }
        Protocol::Udp => {
            let handles = spawn_udp_handlers(
                udp_sockets,
                stats.clone(),
                direction,
                duration,
                bitrate.unwrap_or(1_000_000_000),
                cancel_rx.clone(),
            )
            .await;
            (None, handles)
        }
        Protocol::Quic => {
            // Unreachable - QUIC returns early above
            (None, Vec::new())
        }
    };

    // Start interval loop IMMEDIATELY (don't wait for TCP stream collection)
    let mut interval_timer = tokio::time::interval(Duration::from_secs(1));
    let start = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = interval_timer.tick() => {
                // Duration::ZERO means infinite - only break if duration is set
                if duration != Duration::ZERO && start.elapsed() >= duration {
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
                    aggregate: aggregate.clone(),
                };

                // Notify TUI of progress
                if let Some(tx) = &tui_tx {
                    let _ = tx.try_send(ServerEvent::TestUpdated {
                        id: id.to_string(),
                        bytes: aggregate.bytes,
                        throughput_mbps: aggregate.throughput_mbps,
                    });
                }

                if writer.write_all(format!("{}\n", interval_msg.serialize()?).as_bytes()).await.is_err() {
                    warn!("Failed to send interval, client may have disconnected");
                    break;
                }
            }
        }

        // Check for cancel message (bounded read)
        let read_result = tokio::time::timeout(
            Duration::from_millis(10),
            read_bounded_line(reader, &mut line),
        )
        .await;

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
    match (tcp_collection_handle, udp_handles) {
        (Some(handle), _) => {
            // TCP: wait for stream collection task, then wait for individual handlers
            match handle.await {
                Ok(tcp_handles) => {
                    futures::future::join_all(tcp_handles).await;
                }
                Err(e) => {
                    if e.is_panic() {
                        error!("TCP stream collection task panicked: {:?}", e);
                    } else {
                        warn!("TCP stream collection task was cancelled");
                    }
                }
            }
        }
        (None, handles) if !handles.is_empty() => {
            // UDP: wait for handlers directly
            futures::future::join_all(handles).await;
        }
        _ => {}
    }

    // Send final result
    let duration_ms = stats.elapsed_ms();
    let bytes_total = stats.total_bytes();
    let throughput_mbps = if duration_ms > 0 {
        (bytes_total as f64 * 8.0) / (duration_ms as f64 / 1000.0) / 1_000_000.0
    } else {
        0.0
    };

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

    // Cleanup active test entry BEFORE sending result
    // This ensures cleanup happens even if the write fails
    active_tests.lock().await.remove(id);

    // Notify metrics that test completed
    #[cfg(feature = "prometheus")]
    crate::output::prometheus::on_test_complete(&stats);

    // Push metrics to gateway if configured
    crate::output::push_gateway::maybe_push_metrics(push_gateway_url, &stats).await;

    // Notify TUI that test completed
    if let Some(tx) = &tui_tx {
        let _ = tx.try_send(ServerEvent::TestCompleted {
            id: id.to_string(),
            bytes: bytes_total,
        });
    }

    // Send result (after cleanup so stale entries don't persist on failure)
    writer
        .write_all(format!("{}\n", result.serialize()?).as_bytes())
        .await?;

    info!(
        "Test {} complete: {:.2} Mbps, {} bytes",
        id, throughput_mbps, bytes_total
    );

    Ok((bytes_total, duration_ms, throughput_mbps))
}

/// Read a line with bounded length to prevent memory DoS
async fn read_bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
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
            // Use lossy conversion to handle partial UTF-8 sequences at buffer boundaries
            buf.push_str(&String::from_utf8_lossy(&bytes[..to_read]));
            reader.consume(to_read);
            return Ok(total + to_read);
        }

        let len = bytes.len();
        if total + len > MAX_LINE_LENGTH {
            return Err(anyhow::anyhow!("Line exceeds maximum length"));
        }
        // Use lossy conversion to handle partial UTF-8 sequences at buffer boundaries
        buf.push_str(&String::from_utf8_lossy(bytes));
        reader.consume(len);
        total += len;
    }
}

/// Legacy multi-port TCP handler (kept for potential fallback)
#[allow(dead_code)]
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
                    if let Ok(Some(info)) =
                        tcp::receive_data(stream, stream_stats.clone(), cancel, config).await
                    {
                        test_stats.add_tcp_info(info);
                    }
                }
                Direction::Download => {
                    // Server sends data - capture final TCP_INFO for RTT/retransmits
                    if let Ok(Some(info)) =
                        tcp::send_data(stream, stream_stats.clone(), duration, config, cancel).await
                    {
                        test_stats.add_tcp_info(info);
                    }
                }
                Direction::Bidir => {
                    // Configure socket BEFORE splitting (nodelay, window, buffers)
                    if let Err(e) = tcp::configure_stream(&stream, &config) {
                        tracing::error!("Failed to configure TCP socket: {}", e);
                    }

                    // Split socket for concurrent send/receive
                    let (read_half, write_half) = stream.into_split();

                    let send_stats = stream_stats.clone();
                    let recv_stats = stream_stats.clone();
                    let final_stats = stream_stats.clone();
                    let send_cancel = cancel.clone();
                    let recv_cancel = cancel;

                    let send_config = TcpConfig {
                        buffer_size: config.buffer_size,
                        nodelay: config.nodelay,
                        window_size: config.window_size,
                    };
                    let recv_config = config;

                    let send_handle = tokio::spawn(async move {
                        tcp::send_data_half(
                            write_half,
                            send_stats,
                            duration,
                            send_config,
                            send_cancel,
                        )
                        .await
                    });

                    let recv_handle = tokio::spawn(async move {
                        tcp::receive_data_half(read_half, recv_stats, recv_cancel, recv_config)
                            .await
                    });

                    // Wait for both to complete and reunite halves to get TCP_INFO
                    let (send_result, recv_result) = tokio::join!(send_handle, recv_handle);
                    if let (Ok(Ok(write_half)), Ok(Ok(read_half))) = (send_result, recv_result)
                        && let Ok(stream) = read_half.reunite(write_half)
                        && let Some(info) = tcp::get_stream_tcp_info(&stream)
                    {
                        final_stats.add_retransmits(info.retransmits);
                        test_stats.add_tcp_info(info);
                    }
                }
            }
        });
        handles.push(handle);
    }

    handles
}

/// Single-port TCP mode: receive streams from channel and spawn handlers
async fn spawn_tcp_stream_handlers(
    mut rx: mpsc::Receiver<(TcpStream, u16)>,
    num_streams: usize,
    stats: Arc<TestStats>,
    direction: Direction,
    duration: Duration,
    cancel: watch::Receiver<bool>,
) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();
    let mut received = vec![false; num_streams];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut cancel = cancel;

    // Receive all expected streams from channel
    while received.iter().any(|&r| !r) {
        // Check cancel signal first
        if *cancel.borrow() {
            warn!("Test cancelled during stream collection");
            break;
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            warn!("Timeout waiting for all data streams");
            break;
        }

        // Use select! to check both stream arrival and cancel signal
        let stream_result = tokio::select! {
            biased;
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    warn!("Test cancelled during stream collection");
                    break;
                }
                continue;
            }
            result = tokio::time::timeout(remaining, rx.recv()) => result,
        };

        match stream_result {
            Ok(Some((stream, stream_index))) => {
                let i = stream_index as usize;
                if i >= num_streams {
                    warn!("Invalid stream index: {}", stream_index);
                    continue;
                }
                if received[i] {
                    warn!("Duplicate stream index: {}", stream_index);
                    continue;
                }
                received[i] = true;

                let cancel = cancel.clone();
                let stream_stats = stats.streams[i].clone();
                let test_stats = stats.clone();

                let handle = tokio::spawn(async move {
                    let config = TcpConfig::high_speed();

                    // Capture TCP_INFO before transfer starts
                    if let Some(info) = tcp::get_stream_tcp_info(&stream) {
                        test_stats.add_tcp_info(info);
                    }

                    match direction {
                        Direction::Upload => {
                            if let Ok(Some(info)) =
                                tcp::receive_data(stream, stream_stats.clone(), cancel, config)
                                    .await
                            {
                                test_stats.add_tcp_info(info);
                            }
                        }
                        Direction::Download => {
                            if let Ok(Some(info)) = tcp::send_data(
                                stream,
                                stream_stats.clone(),
                                duration,
                                config,
                                cancel,
                            )
                            .await
                            {
                                test_stats.add_tcp_info(info);
                            }
                        }
                        Direction::Bidir => {
                            if let Err(e) = tcp::configure_stream(&stream, &config) {
                                tracing::error!("Failed to configure TCP socket: {}", e);
                            }
                            let (read_half, write_half) = stream.into_split();

                            let send_stats = stream_stats.clone();
                            let recv_stats = stream_stats.clone();
                            let final_stats = stream_stats.clone();
                            let send_cancel = cancel.clone();
                            let recv_cancel = cancel;
                            let send_config = config.clone();
                            let recv_config = config;

                            let send_handle = tokio::spawn(async move {
                                tcp::send_data_half(
                                    write_half,
                                    send_stats,
                                    duration,
                                    send_config,
                                    send_cancel,
                                )
                                .await
                            });

                            let recv_handle = tokio::spawn(async move {
                                tcp::receive_data_half(
                                    read_half,
                                    recv_stats,
                                    recv_cancel,
                                    recv_config,
                                )
                                .await
                            });

                            let (send_result, recv_result) = tokio::join!(send_handle, recv_handle);
                            if let (Ok(Ok(write_half)), Ok(Ok(read_half))) =
                                (send_result, recv_result)
                                && let Ok(stream) = read_half.reunite(write_half)
                                && let Some(info) = tcp::get_stream_tcp_info(&stream)
                            {
                                final_stats.add_retransmits(info.retransmits);
                                test_stats.add_tcp_info(info);
                            }
                        }
                    }
                });
                handles.push(handle);
            }
            Ok(None) => {
                warn!("Data stream channel closed");
                break;
            }
            Err(_) => {
                warn!("Timeout waiting for data stream");
                break;
            }
        }
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

    // Divide bitrate evenly across streams (matching client behavior)
    // Clamp to at least 1 bps to prevent integer division underflow
    // Only bitrate=0 means unlimited (explicit -b 0)
    let num_streams = sockets.len().max(1) as u64;
    let per_stream_bitrate = if bitrate == 0 {
        0 // Unlimited mode (explicit -b 0)
    } else {
        (bitrate / num_streams).max(1)
    };

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
                    // Wait for client's hello packet to learn their address
                    match udp::wait_for_client(&socket, Duration::from_secs(10)).await {
                        Ok(client_addr) => {
                            // Server sends UDP at per-stream rate to client
                            let _ = udp::send_udp_paced(
                                socket,
                                Some(client_addr),
                                per_stream_bitrate,
                                duration,
                                stream_stats,
                                cancel,
                            )
                            .await;
                        }
                        Err(e) => {
                            warn!("UDP reverse: failed to get client address: {}", e);
                        }
                    }
                }
                Direction::Bidir => {
                    // Wait for client's first packet to learn their address
                    match udp::wait_for_client(&socket, Duration::from_secs(10)).await {
                        Ok(client_addr) => {
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
                                    Some(client_addr),
                                    per_stream_bitrate,
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
                        Err(e) => {
                            warn!("UDP bidir: failed to get client address: {}", e);
                        }
                    }
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
