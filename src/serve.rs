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
use crate::rate_limit::{RateLimitConfig, RateLimiter};
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
            max_concurrent: 1000,
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
                            warn!("Max concurrent handlers reached, rejecting QUIC: {}", peer_addr);
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
            let (stream, peer_addr) = listener.accept().await?;
            let peer_ip = peer_addr.ip();

            // Check ACL
            if !security.acl.is_allowed(peer_ip) {
                warn!("Connection rejected by ACL: {}", peer_addr);
                if let Some(tx) = &security.tui_tx {
                    let _ = tx.try_send(ServerEvent::ConnectionBlocked);
                }
                drop(stream);
                continue;
            }

            // Check rate limit
            if let Some(limiter) = &security.rate_limiter
                && let Err(e) = limiter.check(peer_ip)
            {
                warn!("Rate limit exceeded for {}: {}", peer_addr, e);
                if let Some(tx) = &security.tui_tx {
                    let _ = tx.try_send(ServerEvent::ConnectionBlocked);
                }
                drop(stream);
                continue;
            }

            // Acquire semaphore permit to limit concurrent handlers
            let permit = match handler_semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    warn!("Max concurrent handlers reached, rejecting: {}", peer_addr);
                    drop(stream);
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

/// Handle client with security checks (auth)
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

    // Read client hello (bounded to prevent DoS)
    read_bounded_line(&mut ctrl_reader, &mut line).await?;
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
        read_bounded_line(&mut ctrl_reader, &mut line).await?;
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

    // Read test start
    read_bounded_line(&mut ctrl_reader, &mut line).await?;
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

            if streams > MAX_STREAMS {
                let error = ControlMessage::error(format!(
                    "Requested {} streams exceeds maximum of {}",
                    streams, MAX_STREAMS
                ));
                ctrl_send
                    .write_all(format!("{}\n", error.serialize()?).as_bytes())
                    .await?;
                return Err(anyhow::anyhow!("Stream count exceeds maximum"));
            }

            let mut duration = Duration::from_secs(duration_secs as u64);
            if duration > MAX_TEST_DURATION {
                duration = MAX_TEST_DURATION;
            }
            if let Some(max_dur) = server_max_duration
                && duration > max_dur
            {
                duration = max_dur;
            }

            info!(
                "QUIC test requested: {} streams, {} mode, {}s",
                streams,
                direction,
                duration.as_secs()
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
                    // Server receives - accept uni stream from client
                    match conn.accept_uni().await {
                        Ok(recv) => {
                            if let Err(e) =
                                quic::receive_quic_data(recv, stream_stats, cancel).await
                            {
                                debug!("QUIC receive ended: {}", e);
                            }
                        }
                        Err(e) => debug!("Failed to accept uni stream: {}", e),
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
                    // Accept bidir stream from client
                    match conn.accept_bi().await {
                        Ok((send, recv)) => {
                            let send_stats = stream_stats.clone();
                            let recv_stats = stream_stats;
                            let send_cancel = cancel.clone();
                            let recv_cancel = cancel;

                            let send_handle = tokio::spawn(async move {
                                let _ =
                                    quic::send_quic_data(send, send_stats, duration, send_cancel)
                                        .await;
                            });

                            let recv_handle = tokio::spawn(async move {
                                let _ =
                                    quic::receive_quic_data(recv, recv_stats, recv_cancel).await;
                            });

                            let _ = tokio::join!(send_handle, recv_handle);
                        }
                        Err(e) => debug!("Failed to accept bidir stream: {}", e),
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
        Protocol::Quic => {
            // QUIC uses its own connection model with multiplexed streams
            return Err(anyhow::anyhow!(
                "QUIC protocol requires QUIC endpoint, not TCP control"
            ));
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
        Protocol::Quic => {
            // Unreachable - QUIC returns early above
            Vec::new()
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

    // Push metrics to gateway if configured
    crate::output::push_gateway::maybe_push_metrics(push_gateway_url, &stats).await;

    // Notify TUI that test completed
    if let Some(tx) = &tui_tx {
        let _ = tx.try_send(ServerEvent::TestCompleted {
            id: id.to_string(),
            bytes: bytes_total,
        });
    }

    // Cleanup
    active_tests.lock().await.remove(id);
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
                    }
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

    // Divide bitrate evenly across streams (matching client behavior)
    let num_streams = sockets.len().max(1) as u64;
    let per_stream_bitrate = if bitrate == 0 {
        0 // Unlimited mode
    } else {
        bitrate / num_streams
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
                    // Server sends UDP at per-stream rate
                    let _ = udp::send_udp_paced(
                        socket,
                        per_stream_bitrate,
                        duration,
                        stream_stats,
                        cancel,
                    )
                    .await;
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
