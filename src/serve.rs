//! Server mode implementation
//!
//! Listens for incoming connections and handles bandwidth tests.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
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
const MAX_LINE_LENGTH: usize = 65536;
/// Maximum streams a client can request
const MAX_STREAMS: u8 = 128;
/// Maximum test duration a client can request (1 hour)
const MAX_TEST_DURATION: Duration = Duration::from_secs(3600);
/// Timeout for control-plane handshake reads (prevents DoS from idle connections)
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for initial first-line read on new connections (shorter to resist slow-loris)
const INITIAL_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Additional initial-read timeout budget per expected stream (high fan-out tests).
const INITIAL_READ_TIMEOUT_PER_STREAM_MS: u64 = 80;
/// Upper bound on adaptive first-line timeout to retain slow-loris resistance.
const INITIAL_READ_TIMEOUT_MAX: Duration = Duration::from_secs(20);
/// Interval between progress/stats updates sent to the client
const STATS_INTERVAL: Duration = Duration::from_secs(1);
/// How often to check for cancellation in send/receive loops
const CANCEL_CHECK_TIMEOUT: Duration = Duration::from_millis(10);
/// Brief delay before sending final result to allow buffered writes to flush
const RESULT_FLUSH_DELAY: Duration = Duration::from_millis(100);
/// Timeout for accepting a data stream connection on per-stream listeners
const STREAM_ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum time to wait for all expected data streams to connect
const STREAM_COLLECTION_TIMEOUT: Duration = Duration::from_secs(30);
/// Default UDP/QUIC bitrate when not specified by client (1 Gbps)
const DEFAULT_BITRATE_BPS: u64 = 1_000_000_000;

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
    /// Bind to specific address (overrides address_family bind address)
    pub bind_addr: Option<IpAddr>,
    /// Channel to send events to TUI
    pub tui_tx: Option<mpsc::Sender<ServerEvent>>,
    /// Enable QUIC protocol support (binds additional UDP port)
    pub enable_quic: bool,
    /// Maximum concurrent client handlers (defense against connection floods)
    pub max_concurrent: u32,
    /// Disable mDNS service registration
    pub no_mdns: bool,
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
            bind_addr: None,
            tui_tx: None,
            enable_quic: true,
            max_concurrent: 100,
            no_mdns: false,
        }
    }
}

/// Security context shared across client handlers
struct SecurityContext {
    psk: Option<String>,
    acl: Acl,
    rate_limiter: Option<Arc<RateLimiter>>,
    address_family: AddressFamily,
    bind_addr: Option<IpAddr>,
    tui_tx: Option<mpsc::Sender<ServerEvent>>,
    push_gateway_url: Option<String>,
    /// Runtime capability list for server hellos: the static list minus
    /// `single_port_udp_v1` when the startup self-test failed.
    capabilities: Vec<String>,
    /// Token → test routing table for single-port UDP hellos (issue #63).
    /// `None` when the self-test failed or the shared socket couldn't be
    /// set up — tests then fall back to legacy per-stream ports.
    udp_hello_routes: Option<UdpHelloRoutes>,
}

/// Per-test routing entry for single-port UDP hellos (issue #63),
/// registered under the test's random token before the TestAck goes out.
struct UdpHelloRoute {
    /// Hands freshly connected per-stream sockets to the test's handler
    /// collection task.
    socket_tx: mpsc::Sender<(Arc<UdpSocket>, u16)>,
    /// Hellos must come from the control connection's IP, mirroring the
    /// DataHello check for single-port TCP.
    control_peer_ip: IpAddr,
    expected_streams: u8,
    /// Client's `-w` request, applied to each connected socket.
    udp_buffer: Option<usize>,
    /// Connected sockets created so far, kept so a retried hello (whose
    /// ack was lost before the kernel started routing retries to the
    /// connected socket) gets a fresh ack instead of a duplicate socket.
    sockets: Vec<Option<Arc<UdpSocket>>>,
}

/// Synchronous mutex: the dispatcher only holds it across map operations,
/// never across socket IO, and a sync lock lets the route guard clean up
/// in Drop on every run_test exit path.
type UdpHelloRoutes = Arc<parking_lot::Mutex<HashMap<[u8; 16], UdpHelloRoute>>>;

/// Removes a test's hello route when run_test exits by any path.
struct UdpRouteGuard {
    routes: UdpHelloRoutes,
    token: [u8; 16],
}

impl Drop for UdpRouteGuard {
    fn drop(&mut self) {
        self.routes.lock().remove(&self.token);
    }
}

/// Validation verdict for an inbound single-port UDP hello.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UdpHelloVerdict {
    /// First hello for this stream: create a connected socket.
    Accept,
    /// Stream already has a socket: re-send the ack from it.
    Resend,
    UnknownToken,
    IpMismatch,
    BadStreamIndex,
}

/// Pure validation half of the hello dispatcher, factored out so the
/// token/IP/stream checks are unit-testable without sockets.
fn check_udp_hello(
    route: Option<&UdpHelloRoute>,
    pkt: &udp::UdpHelloPacket,
    src_ip: IpAddr,
) -> UdpHelloVerdict {
    let Some(route) = route else {
        return UdpHelloVerdict::UnknownToken;
    };
    // normalize_ip handles IPv4-mapped IPv6 (control over IPv4, hello on
    // a dual-stack UDP socket appears as ::ffff:x.x.x.x).
    if net::normalize_ip(src_ip) != net::normalize_ip(route.control_peer_ip) {
        return UdpHelloVerdict::IpMismatch;
    }
    if pkt.stream_index >= u16::from(route.expected_streams) {
        return UdpHelloVerdict::BadStreamIndex;
    }
    if route
        .sockets
        .get(usize::from(pkt.stream_index))
        .is_some_and(|s| s.is_some())
    {
        return UdpHelloVerdict::Resend;
    }
    UdpHelloVerdict::Accept
}

/// Single-port UDP hello dispatcher (issue #63): consumes hello
/// datagrams diverted off the shared socket, validates them against the
/// active-test routing table, and for each new stream creates the
/// connected same-port data socket, acks the client FROM that socket
/// (so the client knows the fast path exists before it sends data), and
/// hands the socket to the test's collection task.
fn spawn_udp_hello_dispatcher(
    mut hello_rx: mpsc::Receiver<crate::quic::XfrHelloDatagram>,
    routes: UdpHelloRoutes,
    local_addr: SocketAddr,
    v6only: Option<bool>,
) {
    tokio::spawn(async move {
        while let Some((data, src)) = hello_rx.recv().await {
            let Some(pkt) = udp::UdpHelloPacket::decode(&data) else {
                continue;
            };
            if pkt.kind != udp::UDP_HELLO_KIND_HELLO {
                continue;
            }
            let ack = udp::UdpHelloPacket {
                kind: udp::UDP_HELLO_KIND_ACK,
                stream_index: pkt.stream_index,
                token: pkt.token,
            }
            .encode();

            // Validate under the lock, but do socket IO outside it. The
            // dispatcher is a single task, so no other writer can race a
            // Create decision for the same stream between lock drops.
            enum Action {
                Create {
                    socket_tx: mpsc::Sender<(Arc<UdpSocket>, u16)>,
                    udp_buffer: Option<usize>,
                },
                Resend(Arc<UdpSocket>),
                Drop,
            }
            let action = {
                let map = routes.lock();
                let route = map.get(&pkt.token);
                match check_udp_hello(route, &pkt, src.ip()) {
                    UdpHelloVerdict::Accept => {
                        let route = route.expect("Accept implies route exists");
                        Action::Create {
                            socket_tx: route.socket_tx.clone(),
                            udp_buffer: route.udp_buffer,
                        }
                    }
                    UdpHelloVerdict::Resend => {
                        let socket = route
                            .and_then(|r| r.sockets[usize::from(pkt.stream_index)].clone())
                            .expect("Resend implies socket exists");
                        Action::Resend(socket)
                    }
                    verdict => {
                        debug!(
                            "UDP hello from {} rejected ({:?}) for stream {}",
                            src, verdict, pkt.stream_index
                        );
                        Action::Drop
                    }
                }
            };

            match action {
                Action::Resend(socket) => {
                    if let Err(e) = socket.send(&ack).await {
                        debug!("UDP hello ack resend to {} failed: {}", src, e);
                    }
                }
                Action::Create {
                    socket_tx,
                    udp_buffer,
                } => {
                    let socket =
                        match net::create_connected_udp_same_port(local_addr, v6only, src).await {
                            Ok(s) => Arc::new(s),
                            Err(e) => {
                                warn!(
                                    "single-port UDP: failed to create connected socket for {}: {}",
                                    src, e
                                );
                                continue;
                            }
                        };
                    if let Some(size) = udp_buffer
                        && let Err(e) = net::set_udp_buffer_size(&socket, size)
                    {
                        warn!("Failed to set UDP buffer size to {}: {}", size, e);
                    }
                    if let Err(e) = socket.send(&ack).await {
                        debug!("UDP hello ack to {} failed: {}", src, e);
                    }
                    // Store for ack-resend dedupe; the route may have been
                    // removed if the test ended while we were creating the
                    // socket — drop the socket in that case.
                    {
                        let mut map = routes.lock();
                        match map.get_mut(&pkt.token) {
                            Some(route) => {
                                route.sockets[usize::from(pkt.stream_index)] = Some(socket.clone());
                            }
                            None => continue,
                        }
                    }
                    if socket_tx.send((socket, pkt.stream_index)).await.is_err() {
                        debug!("single-port UDP: test ended before socket handoff");
                    }
                }
                Action::Drop => {}
            }
        }
    });
}

/// Hello listener for the `--no-quic` case (issue #63): without a quinn
/// endpoint there is no tee, so a plain shared socket owns the main UDP
/// port and this loop classifies inbound datagrams itself. Same
/// classifier, same low-rate-lane rules; QUIC-looking packets are
/// dropped since QUIC is disabled.
fn spawn_plain_udp_hello_listener(
    socket: UdpSocket,
    hello_tx: mpsc::Sender<crate::quic::XfrHelloDatagram>,
) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        loop {
            match socket.recv_from(&mut buf).await {
                Ok((n, src)) => {
                    if matches!(
                        udp::classify_shared_datagram(&buf[..n]),
                        udp::SharedSocketLane::XfrHello
                    ) {
                        let _ = hello_tx.try_send((buf[..n].to_vec(), src));
                    } else {
                        debug!(
                            "shared UDP socket: dropping {}-byte non-hello datagram from {}",
                            n, src
                        );
                    }
                }
                Err(e) => {
                    debug!("shared UDP socket recv error: {}", e);
                }
            }
        }
    });
}

struct ActiveTest {
    #[allow(dead_code)]
    stats: Arc<TestStats>,
    #[allow(dead_code)]
    cancel_tx: watch::Sender<bool>,
    #[allow(dead_code)]
    pause_tx: watch::Sender<bool>,
    #[allow(dead_code)]
    data_ports: Vec<u16>,
    /// Channel for receiving data connections in single-port TCP mode
    #[allow(dead_code)]
    data_stream_tx: Option<mpsc::Sender<(TcpStream, u16)>>, // (stream, stream_index)
    /// Control connection peer IP (for DataHello validation)
    control_peer_ip: std::net::IpAddr,
    /// Number of streams expected for this test (for adaptive DataHello timeout).
    expected_streams: u8,
}

/// Convert a wire-format window size (u64) to a host usize for setsockopt.
///
/// On 64-bit targets this is lossless. On 32-bit targets, values larger than
/// `usize::MAX` (≈4 GB) cannot be represented; we drop the request with a debug
/// log and let the kernel autotune instead, rather than letting `as usize` wrap
/// silently to a misleading value that would then bypass the c_int validation
/// in `tcp::configure_socket_buffers`.
fn client_window_to_host(window: Option<u64>) -> Option<usize> {
    window.and_then(|w| match usize::try_from(w) {
        Ok(v) => Some(v),
        Err(_) => {
            tracing::debug!(
                "Client requested window {} bytes which exceeds host usize::MAX; \
                 ignoring request and falling back to kernel autotune",
                w
            );
            None
        }
    })
}

/// The server has always enabled `TCP_NODELAY` on its data sockets,
/// independent of any client setting — harmless for bulk transfers
/// (Nagle never delays full-MSS writes) and it keeps the final partial
/// segment from stalling at end of test.
const SERVER_DATA_NODELAY: bool = true;

/// Effective nodelay for a server-side TCP data socket.
///
/// The client's `--tcp-nodelay` request (`TestStart.tcp_nodelay`) ORs
/// into the server's own default rather than replacing it: a client
/// that passes the flag is guaranteed nodelay on the server's sends
/// (the latency-relevant direction in `-R`/`--bidir`), while a client
/// that doesn't cannot turn off the historical always-on behavior.
/// With today's default the OR is identity-true; the threading exists
/// so the client's request stays honored if the server-side default
/// ever changes.
fn server_data_nodelay(client_requested: bool) -> bool {
    client_requested || SERVER_DATA_NODELAY
}

fn initial_read_timeout_for_peer(
    active_tests: &HashMap<String, ActiveTest>,
    peer_ip: std::net::IpAddr,
) -> Duration {
    let peer_ip = net::normalize_ip(peer_ip);
    let max_streams = active_tests
        .values()
        .filter(|t| net::normalize_ip(t.control_peer_ip) == peer_ip)
        .map(|t| u64::from(t.expected_streams))
        .max()
        .unwrap_or(0);
    let scaled =
        Duration::from_millis(max_streams.saturating_mul(INITIAL_READ_TIMEOUT_PER_STREAM_MS));
    let timeout = if scaled > INITIAL_READ_TIMEOUT {
        scaled
    } else {
        INITIAL_READ_TIMEOUT
    };
    timeout.min(INITIAL_READ_TIMEOUT_MAX)
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
        let listener = if let Some(ip) = self.config.bind_addr {
            let addr = SocketAddr::new(ip, self.config.port);
            info!("Binding to {}", addr);
            net::create_tcp_listener_on_addr(addr).await?
        } else {
            net::create_tcp_listener_auto_mptcp(self.config.port, self.config.address_family)
                .await?
        };

        // Single-port UDP (issue #63): advertise the capability only when
        // the running kernel demonstrably routes connected-socket traffic
        // past a same-port wildcard (SO_REUSEPORT group semantics vary).
        let single_port_udp_ok = net::single_port_udp_self_test(self.config.address_family).await;
        let udp_bind_addr = if let Some(ip) = self.config.bind_addr {
            SocketAddr::new(ip, self.config.port)
        } else {
            self.config.address_family.bind_addr(self.config.port)
        };
        let udp_v6only = match self.config.address_family {
            AddressFamily::V4Only => None,
            AddressFamily::V6Only => Some(true),
            AddressFamily::DualStack => Some(false),
        };
        let (udp_hello_tx, udp_hello_rx) = if single_port_udp_ok {
            let (tx, rx) = mpsc::channel::<crate::quic::XfrHelloDatagram>(256);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        // Create QUIC endpoint on the same port (UDP) - only if enabled
        let quic_endpoint = if self.config.enable_quic {
            let (cert, key) = quic::generate_self_signed_cert()?;
            let endpoint = quic::create_server_endpoint(
                udp_bind_addr,
                self.config.address_family,
                cert,
                key,
                udp_hello_tx.clone(),
            )?;
            info!("QUIC endpoint ready on port {}", self.config.port);
            Some(endpoint)
        } else {
            None
        };

        // Single-port UDP needs something listening on the main UDP port
        // for hellos. With QUIC enabled the tee inside the endpoint does
        // it; otherwise bind a plain shared socket for the hello lane.
        let mut udp_hello_active = false;
        if let Some(tx) = udp_hello_tx {
            if self.config.enable_quic {
                udp_hello_active = true;
            } else {
                match net::create_shared_udp_socket(udp_bind_addr, udp_v6only).await {
                    Ok(socket) => {
                        spawn_plain_udp_hello_listener(socket, tx);
                        udp_hello_active = true;
                    }
                    Err(e) => {
                        warn!(
                            "single-port UDP disabled: failed to bind shared UDP socket on {}: {}",
                            udp_bind_addr, e
                        );
                    }
                }
            }
        }
        let udp_hello_routes: Option<UdpHelloRoutes> = if udp_hello_active {
            let routes: UdpHelloRoutes = Arc::new(parking_lot::Mutex::new(HashMap::new()));
            // The dispatcher consumes diverted hellos for the whole server
            // lifetime; per-test entries come and go in the routes map.
            spawn_udp_hello_dispatcher(
                udp_hello_rx.expect("hello channel exists when lane is active"),
                routes.clone(),
                udp_bind_addr,
                udp_v6only,
            );
            Some(routes)
        } else {
            None
        };
        let capabilities = if udp_hello_routes.is_some() {
            crate::protocol::supported_capabilities()
        } else {
            crate::protocol::supported_capabilities_without(
                crate::protocol::SINGLE_PORT_UDP_CAPABILITY,
            )
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
            bind_addr: self.config.bind_addr,
            tui_tx: self.config.tui_tx.clone(),
            push_gateway_url: self.config.push_gateway_url.clone(),
            capabilities,
            udp_hello_routes,
        });

        // Start rate limiter cleanup task if enabled
        if let Some(limiter) = rate_limiter.clone() {
            limiter.start_cleanup_task();
        }

        // Semaphore to limit concurrent handlers (defense against connection floods)
        let handler_semaphore = Arc::new(Semaphore::new(self.config.max_concurrent as usize));

        // Pre-handshake semaphore: limits concurrent connections that haven't yet been
        // classified (Hello vs DataHello). Prevents connection-flood DoS where attackers
        // open many sockets that stall during the 5s initial read timeout.
        let conn_semaphore = Arc::new(Semaphore::new(self.config.max_concurrent as usize * 4));

        // Shutdown channel for one-off mode (watch allows multiple receivers)
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Spawn QUIC acceptor task (only if QUIC is enabled)
        if let Some(quic_endpoint) = quic_endpoint {
            let quic_security = security.clone();
            let quic_active_tests = self.active_tests.clone();
            let quic_max_duration = self.config.max_duration;
            let quic_rate_limiter = rate_limiter.clone();
            let quic_semaphore = handler_semaphore.clone();
            let quic_one_off = self.config.one_off;
            let quic_shutdown_tx = shutdown_tx.clone();
            let mut quic_shutdown_rx = shutdown_rx.clone();
            tokio::spawn(async move {
                loop {
                    // Use select! to accept connections OR receive shutdown signal
                    let incoming = tokio::select! {
                        result = quic_endpoint.accept() => {
                            match result {
                                Some(incoming) => incoming,
                                None => break, // Endpoint closed
                            }
                        }
                        _ = quic_shutdown_rx.changed() => {
                            if *quic_shutdown_rx.borrow() {
                                debug!("QUIC shutdown signal received");
                                break;
                            }
                            continue;
                        }
                    };
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
                    let shutdown_tx = quic_shutdown_tx.clone();
                    let one_off = quic_one_off;

                    let handle = tokio::spawn(async move {
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

                        match &result {
                            Ok(()) => true,
                            Err(e) => {
                                error!("QUIC client error {}: {}", peer_addr, e);
                                false
                            }
                        }
                    });

                    if one_off {
                        // Only signal shutdown if test completed successfully
                        let shutdown_tx = shutdown_tx.clone();
                        tokio::spawn(async move {
                            if let Ok(true) = handle.await {
                                let _ = shutdown_tx.send(true);
                            }
                        });
                    }
                }
            });
        }

        // Register mDNS service for discovery (unless --no-mdns)
        #[cfg(feature = "discovery")]
        let _mdns = if self.config.no_mdns {
            None
        } else {
            register_mdns_service(self.config.port)
        };

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

        // TCP accept loop uses the same shutdown channel as QUIC
        let mut tcp_shutdown_rx = shutdown_rx.clone();

        loop {
            // Use select! to accept connections OR receive shutdown signal
            let (stream, peer_addr) = tokio::select! {
                result = listener.accept() => result?,
                _ = tcp_shutdown_rx.changed() => {
                    if *tcp_shutdown_rx.borrow() {
                        debug!("Shutdown signal received, exiting accept loop");
                        break;
                    }
                    continue;
                }
            };
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

            // Acquire pre-handshake permit (limits concurrent unclassified connections)
            let conn_permit = match conn_semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    debug!(
                        "Pre-handshake connection limit reached, dropping: {}",
                        peer_addr
                    );
                    drop(stream);
                    continue;
                }
            };

            // Spawn a task per connection to avoid slow-loris blocking the accept loop.
            // The spawned task reads the first line, parses, and routes.
            let active_tests = self.active_tests.clone();
            let handler_semaphore = handler_semaphore.clone();
            let security = security.clone();
            let base_port = self.config.port;
            let max_duration = self.config.max_duration;
            let one_off = self.config.one_off;
            let shutdown_tx = shutdown_tx.clone();

            tokio::spawn(async move {
                let result = handle_new_connection(
                    stream,
                    peer_addr,
                    active_tests,
                    handler_semaphore,
                    security,
                    base_port,
                    max_duration,
                    one_off,
                    shutdown_tx,
                )
                .await;
                // Release pre-handshake permit when connection is classified/done
                drop(conn_permit);
                if let Err(e) = result {
                    debug!("Connection handling error from {}: {}", peer_addr, e);
                }
            });
        }

        Ok(())
    }
}

/// Handle a newly accepted TCP connection.
/// Reads the first line, parses the message type, and routes accordingly.
/// Runs in its own spawned task to prevent slow-loris blocking the accept loop.
#[allow(clippy::too_many_arguments)]
async fn handle_new_connection(
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    handler_semaphore: Arc<Semaphore>,
    security: Arc<SecurityContext>,
    base_port: u16,
    max_duration: Option<Duration>,
    one_off: bool,
    shutdown_tx: watch::Sender<bool>,
) -> anyhow::Result<()> {
    let peer_ip = peer_addr.ip();
    let initial_timeout = {
        let tests = active_tests.lock().await;
        initial_read_timeout_for_peer(&tests, peer_ip)
    };

    // Read first line with adaptive timeout: short by default to resist slow-loris,
    // but extended for high fan-out active tests where DataHello bursts may retransmit.
    let line = tokio::time::timeout(
        initial_timeout,
        read_first_line_unbuffered(&mut stream, MAX_LINE_LENGTH),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "Initial read timeout ({initial_timeout:?}) from {}",
            peer_addr
        )
    })??;

    let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

    match msg {
        ControlMessage::DataHello {
            test_id,
            stream_index,
        } => {
            // Quick validation: reject unknown test_ids immediately (Fix 2: DataHello flood)
            let test_exists = active_tests.lock().await.contains_key(&test_id);
            if !test_exists {
                debug!("DataHello for unknown test {} from {}", test_id, peer_addr);
                return Ok(());
            }
            // Route data connection (no semaphore/rate-limit needed, control connection holds those)
            route_data_hello(stream, peer_addr, active_tests, test_id, stream_index).await?;
        }
        ControlMessage::Hello { .. } => {
            // Control connection - acquire rate limit and semaphore
            let rate_limit_guard = if let Some(limiter) = &security.rate_limiter {
                if let Err(e) = limiter.check(peer_ip) {
                    warn!("Rate limit exceeded for {}: {}", peer_addr, e);
                    if let Some(tx) = &security.tui_tx {
                        let _ = tx.try_send(ServerEvent::ConnectionBlocked);
                    }
                    return Ok(());
                }
                Some(RateLimitGuard::new(limiter.clone(), peer_ip))
            } else {
                None
            };

            let permit = match handler_semaphore.try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    warn!("Max concurrent handlers reached, rejecting: {}", peer_addr);
                    return Ok(());
                }
            };

            // The detailed connect line (software, protocol, capability
            // deltas) is logged from the Hello processing in
            // handle_client_with_first_message via log_client_hello.
            let handle = tokio::spawn(async move {
                let _permit = permit;
                let _rate_guard = rate_limit_guard;

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

                match &result {
                    Ok(()) => true, // test completed successfully
                    Err(e) => {
                        error!("Client error {}: {}", peer_addr, e);
                        false // handshake/auth/test failed
                    }
                }
            });

            if one_off {
                // Only signal shutdown if a test actually completed successfully
                // (failed handshakes/auth should not terminate the server)
                if let Ok(true) = handle.await {
                    let _ = shutdown_tx.send(true);
                }
            }
        }
        other => {
            warn!("Unexpected first message from {}: {:?}", peer_addr, other);
        }
    }

    Ok(())
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
        ControlMessage::Hello {
            version,
            client,
            capabilities,
            ..
        } => {
            log_client_hello(
                peer_addr,
                &version,
                client.as_ref(),
                &capabilities,
                &security.capabilities,
            );
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
                let hello = ControlMessage::server_hello_with_auth_and_capabilities(
                    nonce.clone(),
                    security.capabilities.clone(),
                );
                ctrl_send
                    .write_all(format!("{}\n", hello.serialize()?).as_bytes())
                    .await?;
                Some(nonce)
            } else {
                let hello =
                    ControlMessage::server_hello_with_capabilities(security.capabilities.clone());
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
                let psk = security.psk.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("PSK required for authentication but not configured")
                })?;
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
            ..
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
                udp_token: None,
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

/// One-line observability for every control connection: client software,
/// protocol version, and which of the server's capabilities the client
/// does NOT advertise — i.e. the features that will silently fall back
/// for this session. "Which client version was that" is the first
/// question for any field report, so the log answers it up front.
fn log_client_hello(
    peer_addr: SocketAddr,
    version: &str,
    client: Option<&String>,
    client_capabilities: &Option<Vec<String>>,
    server_capabilities: &[String],
) {
    info!(
        "Client connected: {} ({}, protocol {})",
        peer_addr,
        client.map(String::as_str).unwrap_or("unknown client"),
        version
    );
    let missing: Vec<&str> = server_capabilities
        .iter()
        .filter(|cap| {
            !client_capabilities
                .as_ref()
                .is_some_and(|caps| caps.iter().any(|c| c == *cap))
        })
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        info!(
            "Client {} lacks capabilities: {} (affected features fall back this session)",
            peer_addr,
            missing.join(", ")
        );
    }
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
    if let Err(e) = tcp::configure_control_stream(&stream) {
        warn!("Failed to set TCP_NODELAY on control stream: {}", e);
    }
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    // Process pre-read Hello message
    let (auth_nonce, client_capabilities) = match first_msg {
        ControlMessage::Hello {
            version,
            client,
            capabilities,
            ..
        } => {
            log_client_hello(
                peer_addr,
                &version,
                client.as_ref(),
                &capabilities,
                &security.capabilities,
            );
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
                let hello = ControlMessage::server_hello_with_auth_and_capabilities(
                    nonce.clone(),
                    security.capabilities.clone(),
                );
                writer
                    .write_all(format!("{}\n", hello.serialize()?).as_bytes())
                    .await?;
                (Some(nonce), capabilities)
            } else {
                let hello =
                    ControlMessage::server_hello_with_capabilities(security.capabilities.clone());
                writer
                    .write_all(format!("{}\n", hello.serialize()?).as_bytes())
                    .await?;
                (None, capabilities)
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
                let psk = security.psk.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("PSK required for authentication but not configured")
                })?;
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

    // Determine which optional capabilities the client supports
    let client_supports_single_port =
        crate::protocol::capability_advertised(&client_capabilities, "single_port_tcp");
    let client_supports_udp_feedback =
        crate::protocol::capability_advertised(&client_capabilities, "udp_feedback_v1");
    let client_supports_single_port_udp = crate::protocol::capability_advertised(
        &client_capabilities,
        crate::protocol::SINGLE_PORT_UDP_CAPABILITY,
    );

    // Continue with test handling
    handle_test_request(
        &mut reader,
        &mut writer,
        peer_addr,
        active_tests,
        server_max_duration,
        security,
        client_supports_single_port,
        client_supports_udp_feedback,
        client_supports_single_port_udp,
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
    if let Err(e) = tcp::configure_control_stream(&stream) {
        warn!("Failed to set TCP_NODELAY on control stream: {}", e);
    }
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
                let psk = security.psk.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("PSK required for authentication but not configured")
                })?;
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
    // Note: dead code path - assumes single-port capable client and UDP feedback support
    handle_test_request(
        &mut reader,
        &mut writer,
        peer_addr,
        active_tests,
        server_max_duration,
        security,
        true,
        true,
        true,
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
                let hello = ControlMessage::server_hello_with_auth_and_capabilities(
                    nonce.clone(),
                    security.capabilities.clone(),
                );
                writer
                    .write_all(format!("{}\n", hello.serialize()?).as_bytes())
                    .await?;
                Ok(Some(nonce))
            } else {
                let hello =
                    ControlMessage::server_hello_with_capabilities(security.capabilities.clone());
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
#[allow(clippy::too_many_arguments)]
async fn handle_test_request(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    peer_addr: SocketAddr,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    server_max_duration: Option<Duration>,
    security: &SecurityContext,
    client_supports_single_port: bool,
    client_supports_udp_feedback: bool,
    client_supports_single_port_udp: bool,
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
            congestion,
            mptcp,
            dscp,
            window_size,
            zerocopy,
            mtu_probe,
            tcp_nodelay,
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

            // Validate congestion control algorithm (TCP only)
            if protocol == Protocol::Tcp
                && let Some(ref algo) = congestion
                && let Err(e) = tcp::validate_congestion(algo)
            {
                let error = ControlMessage::error(format!(
                    "Unsupported congestion control algorithm '{}': {}",
                    algo, e
                ));
                writer
                    .write_all(format!("{}\n", error.serialize()?).as_bytes())
                    .await?;
                return Err(anyhow::anyhow!("Invalid congestion algorithm"));
            }

            let duration_display = if duration == Duration::ZERO {
                "∞".to_string()
            } else {
                format!("{}s", duration.as_secs())
            };
            let protocol_display = if mptcp && protocol == Protocol::Tcp {
                "MPTCP"
            } else {
                match protocol {
                    Protocol::Tcp => "TCP",
                    Protocol::Udp => "UDP",
                    Protocol::Quic => "QUIC",
                }
            };
            info!(
                "Test requested: {} {} streams, {} mode, {}",
                protocol_display, streams, direction, duration_display
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
                congestion,
                active_tests.clone(),
                security.address_family,
                security.bind_addr,
                peer_addr,
                security.tui_tx.clone(),
                &security.push_gateway_url,
                client_supports_single_port,
                client_supports_udp_feedback,
                // Single-port UDP needs both peers on board: the client
                // must know to send hellos, the server must have a
                // working hello lane (self-test + shared socket).
                if client_supports_single_port_udp {
                    security.udp_hello_routes.clone()
                } else {
                    None
                },
                dscp,
                window_size,
                zerocopy,
                mtu_probe,
                tcp_nodelay,
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

/// Compute per-direction byte/throughput totals (from the CLIENT's perspective)
/// when the test is bidirectional. For unidirectional tests returns all None —
/// the existing `bytes_total` and `throughput_mbps` fields already carry the
/// single-direction number.
///
/// Direction convention: `TestResult.bytes_sent` means "bytes the *client* sent"
/// (its upload direction). Server-local counters are reversed from that view
/// — what the server sent went to the client (the client's downloads), and
/// what the server received came from the client (the client's uploads). So
/// we flip when emitting.
fn directional_totals(
    direction: Direction,
    stats: &TestStats,
    duration_ms: u64,
) -> (Option<u64>, Option<u64>, Option<f64>, Option<f64>) {
    if direction != Direction::Bidir {
        return (None, None, None, None);
    }
    // Swap: server-sent == client-received, server-received == client-sent.
    let client_bytes_sent = stats.total_bytes_received();
    let client_bytes_received = stats.total_bytes_sent();
    let mbps = |bytes: u64| {
        if duration_ms == 0 {
            0.0
        } else {
            (bytes as f64 * 8.0) / (duration_ms as f64 / 1000.0) / 1_000_000.0
        }
    };
    (
        Some(client_bytes_sent),
        Some(client_bytes_received),
        Some(mbps(client_bytes_sent)),
        Some(mbps(client_bytes_received)),
    )
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
    let (pause_tx, pause_rx) = watch::channel(false);

    // Store active test
    {
        let mut tests = active_tests.lock().await;
        tests.insert(
            id.to_string(),
            ActiveTest {
                stats: stats.clone(),
                cancel_tx,
                pause_tx,
                data_ports: vec![],
                data_stream_tx: None,
                control_peer_ip: peer_addr.ip(),
                expected_streams: streams,
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
        let pause = pause_rx.clone();
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
                                quic::send_quic_data(send, stream_stats, duration, cancel, pause)
                                    .await
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
                        let send_pause = pause;

                        let send_handle = tokio::spawn(async move {
                            let _ = quic::send_quic_data(
                                send,
                                send_stats,
                                duration,
                                send_cancel,
                                send_pause,
                            )
                            .await;
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

    // Run the data-plane and interval loop. The body is wrapped so we can
    // always remove the active test entry and tear down live metrics, even if
    // an error or cancellation exits early.
    let run_result: anyhow::Result<(u64, f64)> = async {
        // Send interval updates
        let mut interval_timer = tokio::time::interval(STATS_INTERVAL);
        // Skip stale ticks rather than bursting them on unblock: if write_all stalls
        // under back-pressure, Burst would emit several stale interval samples with
        // fresh client-side arrival timestamps once the writer unblocks — both the
        // timestamps and the throughput numbers attached to those samples are
        // misleading. Skip drops them; cumulative state in StreamStats atomics still
        // surfaces correctly on the next live tick and at end-of-test.
        interval_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
                let aggregate = stats.to_aggregate_with_direction(&intervals, direction == Direction::Bidir);

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
            CANCEL_CHECK_TIMEOUT,
            read_bounded_line(&mut ctrl_reader, &mut line),
        )
        .await;

        if let Ok(Ok(n)) = read_result
            && n > 0
        {
            match ControlMessage::deserialize(line.trim()) {
                Ok(ControlMessage::Cancel {
                    id: cancel_id,
                    reason,
                }) if cancel_id == id => {
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
                Ok(ControlMessage::Pause { id: pause_id }) if pause_id == id => {
                    info!("QUIC test {} paused", id);
                    if let Some(test) = active_tests.lock().await.get(id) {
                        let _ = test.pause_tx.send(true);
                    }
                }
                Ok(ControlMessage::Resume { id: resume_id }) if resume_id == id => {
                    info!("QUIC test {} resumed", id);
                    if let Some(test) = active_tests.lock().await.get(id) {
                        let _ = test.pause_tx.send(false);
                    }
                }
                _ => {}
            }
        }
    }

    // Signal handlers to stop
    if let Some(test) = active_tests.lock().await.get(id) {
        let _ = test.cancel_tx.send(true);
    }

    // Wait for handlers to complete
    let results = futures::future::join_all(handles).await;
    for result in results {
        if let Err(e) = result
            && e.is_panic()
        {
            error!("QUIC stream handler panicked: {:?}", e);
        }
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

    // Bidir: split up/down reporting for asymmetric links (issue #56).
    let (bytes_sent, bytes_received, throughput_send_mbps, throughput_recv_mbps) =
        directional_totals(direction, &stats, duration_ms);

    let result = ControlMessage::Result(TestResult {
        id: id.to_string(),
        bytes_total,
        duration_ms,
        throughput_mbps,
        streams: stream_results,
        tcp_info: None,
        udp_stats: None,
        bytes_sent,
        bytes_received,
        throughput_send_mbps,
        throughput_recv_mbps,
        mtu_probe: None,
    });

    ctrl_send
        .write_all(format!("{}\n", result.serialize()?).as_bytes())
        .await?;

    // Finish the control stream to ensure result is sent
    ctrl_send.finish()?;

    // Give client time to receive the result before connection closes
    tokio::time::sleep(RESULT_FLUSH_DELAY).await;

    Ok((bytes_total, throughput_mbps))
}
.await;

    // Always remove the active entry and stop data handlers.
    remove_active_test(&active_tests, id).await;

    match run_result {
        Ok((bytes_total, throughput_mbps)) => {
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

            info!(
                "QUIC test {} complete: {:.2} Mbps, {} bytes",
                id, throughput_mbps, bytes_total
            );

            Ok(())
        }
        Err(e) => {
            #[cfg(feature = "prometheus")]
            crate::output::prometheus::on_test_aborted(&stats);

            if let Some(tx) = &tui_tx {
                let _ = tx.try_send(ServerEvent::TestCompleted {
                    id: id.to_string(),
                    bytes: stats.total_bytes(),
                });
            }

            error!("QUIC test {} ended with error: {}", id, e);
            Err(e)
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
    congestion: Option<String>,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    address_family: AddressFamily,
    bind_addr: Option<IpAddr>,
    peer_addr: SocketAddr,
    tui_tx: Option<mpsc::Sender<ServerEvent>>,
    push_gateway_url: &Option<String>,
    client_supports_single_port: bool,
    client_supports_udp_feedback: bool,
    udp_single_port_routes: Option<UdpHelloRoutes>,
    dscp: Option<u8>,
    client_window_size: Option<u64>,
    zerocopy: bool,
    mtu_probe: bool,
    tcp_nodelay: bool,
) -> anyhow::Result<(u64, u64, f64)> {
    let mut line = String::new();

    // For TCP: single-port mode (data connections come on control port)
    // For UDP: single-port mode (hello-routed connected sockets on the
    // main port, issue #63) or legacy per-stream sockets
    let mut data_ports = Vec::new();
    let mut udp_sockets: Vec<Arc<UdpSocket>> = Vec::new();
    let mut udp_token: Option<[u8; 16]> = None;
    let mut udp_socket_rx: Option<mpsc::Receiver<(Arc<UdpSocket>, u16)>> = None;
    let mut _udp_route_guard: Option<UdpRouteGuard> = None;

    // Create cancel channel early so fallback listeners can use it
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let (pause_tx, pause_rx) = watch::channel(false);

    let (data_stream_tx, data_stream_rx) = match protocol {
        Protocol::Tcp if client_supports_single_port => {
            // Single-port mode: data connections will be routed via channel
            let (tx, rx) = mpsc::channel::<(TcpStream, u16)>(streams as usize);
            (Some(tx), Some(rx))
        }
        Protocol::Tcp => {
            // Multi-port fallback for legacy clients without single_port_tcp capability
            let (tx, rx) = mpsc::channel::<(TcpStream, u16)>(streams as usize);
            let expected_ip = net::normalize_ip(peer_addr.ip());
            for i in 0..streams {
                let listener = if let Some(ip) = bind_addr {
                    net::create_tcp_listener_on_addr(SocketAddr::new(ip, 0)).await?
                } else {
                    net::create_tcp_listener_auto_mptcp(0, address_family).await?
                };
                data_ports.push(listener.local_addr()?.port());
                debug!(
                    "TCP data port {} allocated for stream {}",
                    data_ports.last().unwrap(),
                    i
                );
                let tx = tx.clone();
                let stream_index = i as u16;
                let mut cancel = cancel_rx.clone();
                tokio::spawn(async move {
                    // Use select! with cancel to avoid leaking listener tasks
                    let accept_result = tokio::select! {
                        result = listener.accept() => result,
                        _ = cancel.changed() => return,
                    };
                    match accept_result {
                        Ok((stream, data_peer)) => {
                            // Validate peer IP matches control connection
                            let actual_ip = net::normalize_ip(data_peer.ip());
                            if actual_ip != expected_ip {
                                warn!(
                                    "Multi-port data connection from unauthorized IP {} (expected {})",
                                    data_peer.ip(),
                                    expected_ip
                                );
                                return;
                            }
                            let _ = tx.send((stream, stream_index)).await;
                        }
                        Err(e) => {
                            warn!("Failed to accept on data port: {}", e);
                        }
                    }
                });
            }
            // Don't store tx in active_tests since we don't need DataHello routing
            (None, Some(rx))
        }
        Protocol::Udp if udp_single_port_routes.is_some() => {
            // Single-port mode (issue #63): no ports allocated. The client
            // sends a token-bearing hello per stream to the main port; the
            // dispatcher creates connected same-port sockets and hands
            // them to this test through the channel registered here.
            let routes = udp_single_port_routes
                .as_ref()
                .expect("guarded by match arm");
            let (socket_tx, socket_rx) = mpsc::channel::<(Arc<UdpSocket>, u16)>(streams as usize);
            let token = {
                use std::collections::hash_map::Entry;
                let mut map = routes.lock();
                loop {
                    let mut candidate = [0u8; 16];
                    rand::RngExt::fill(&mut rand::rng(), &mut candidate[..]);
                    if let Entry::Vacant(entry) = map.entry(candidate) {
                        entry.insert(UdpHelloRoute {
                            socket_tx,
                            control_peer_ip: peer_addr.ip(),
                            expected_streams: streams,
                            udp_buffer: client_window_to_host(client_window_size),
                            sockets: vec![None; streams as usize],
                        });
                        break candidate;
                    }
                }
            };
            _udp_route_guard = Some(UdpRouteGuard {
                routes: routes.clone(),
                token,
            });
            udp_token = Some(token);
            udp_socket_rx = Some(socket_rx);
            (None, None)
        }
        Protocol::Udp => {
            // Apply the client's `-w` to each receive socket. Without this,
            // high-rate UDP runs against weak/loaded receivers can saturate
            // the kernel UDP buffer and cause silent tail drops that don't
            // surface as sequence-gap loss until traffic stops (issue #70).
            let udp_buffer = client_window_to_host(client_window_size);
            for _ in 0..streams {
                let socket = if let Some(ip) = bind_addr {
                    net::create_udp_socket_bound(SocketAddr::new(ip, 0)).await?
                } else {
                    net::create_udp_socket(0, address_family).await?
                };
                if let Some(size) = udp_buffer
                    && let Err(e) = net::set_udp_buffer_size(&socket, size)
                {
                    warn!("Failed to set UDP buffer size to {}: {}", size, e);
                }
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

    // Store active test BEFORE sending TestAck to avoid race condition
    // (client may send DataHello immediately after receiving TestAck)
    {
        let mut tests = active_tests.lock().await;
        tests.insert(
            id.to_string(),
            ActiveTest {
                stats: stats.clone(),
                cancel_tx,
                pause_tx,
                data_ports: data_ports.clone(),
                data_stream_tx,
                control_peer_ip: peer_addr.ip(),
                expected_streams: streams,
            },
        );
    }

    // Send test ack with allocated ports (or the single-port UDP token)
    let ack = ControlMessage::TestAck {
        id: id.to_string(),
        data_ports: data_ports.clone(),
        udp_token: udp_token.as_ref().map(udp::encode_hello_token),
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

    // Run the data-plane and interval loop. The body is wrapped so we can
    // always remove the active test entry and tear down live metrics, even if
    // an error or cancellation exits early.
    let run_result: anyhow::Result<(u64, u64, f64)> = async {
        // Spawn data stream handlers
    // For TCP and single-port UDP: spawn stream collection in background
    // to not block the interval loop
    // For legacy UDP: handlers are spawned immediately
    let (collection_handle, udp_handles) = match protocol {
        Protocol::Tcp => {
            // Single-port mode: spawn stream collection in background
            // This allows interval loop and cancel handling to run while waiting for streams
            if let Some(rx) = data_stream_rx {
                let stats_clone = stats.clone();
                let cancel_clone = cancel_rx.clone();
                let pause_clone = pause_rx.clone();
                let handle = tokio::spawn(async move {
                    spawn_tcp_stream_handlers(
                        rx,
                        streams as usize,
                        stats_clone,
                        direction,
                        duration,
                        cancel_clone,
                        congestion.clone(),
                        bitrate,
                        pause_clone,
                        dscp,
                        client_window_size,
                        zerocopy,
                        tcp_nodelay,
                    )
                    .await
                });
                (Some(handle), Vec::new())
            } else {
                (None, Vec::new())
            }
        }
        Protocol::Udp if udp_socket_rx.is_some() => {
            // Single-port mode: connected sockets stream in from the hello
            // dispatcher as the client's per-stream handshakes land; the
            // collection task mirrors the TCP DataHello flow. Covers both
            // bulk transfers and --probe-mtu (handled per socket inside).
            let rx = udp_socket_rx.take().expect("guarded by match arm");
            let token = udp_token.expect("single-port UDP always has a token");
            let handle = tokio::spawn(spawn_single_port_udp_handlers(
                rx,
                streams as usize,
                token,
                stats.clone(),
                direction,
                duration,
                bitrate.unwrap_or(DEFAULT_BITRATE_BPS),
                cancel_rx.clone(),
                pause_rx.clone(),
                dscp,
                client_supports_udp_feedback,
                mtu_probe,
            ));
            (Some(handle), Vec::new())
        }
        Protocol::Udp if mtu_probe => {
            // Probe mode replaces the bulk handlers entirely: every
            // socket answers XFRP probes with ack + same-size echo until
            // the control channel ends the test. DF is set so oversized
            // echoes die at the constraining hop instead of fragmenting;
            // a failure to set it degrades reverse-direction results, so
            // it warns rather than aborting the probe.
            let handles = udp_sockets
                .iter()
                .map(|socket| {
                    let ipv6 = socket.local_addr().map(|a| a.is_ipv6()).unwrap_or(false);
                    if let Err(e) = net::set_dont_fragment(socket, ipv6) {
                        warn!(
                            "MTU probe: could not set don't-fragment on echo socket: {} \
                             (oversized echoes may fragment and overstate the reverse path)",
                            e
                        );
                    }
                    let socket = socket.clone();
                    let cancel = cancel_rx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = udp::respond_mtu_probes(socket, cancel, None).await {
                            error!("MTU probe responder error: {}", e);
                        }
                    })
                })
                .collect();
            (None, handles)
        }
        Protocol::Udp => {
            let handles = spawn_udp_handlers(
                udp_sockets,
                stats.clone(),
                direction,
                duration,
                bitrate.unwrap_or(DEFAULT_BITRATE_BPS),
                cancel_rx.clone(),
                pause_rx.clone(),
                dscp,
                client_supports_udp_feedback,
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
    let mut interval_timer = tokio::time::interval(STATS_INTERVAL);
    interval_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
                let aggregate = stats.to_aggregate_with_direction(&intervals, direction == Direction::Bidir);

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

        // Check for cancel/pause/resume messages (bounded read)
        let read_result =
            tokio::time::timeout(CANCEL_CHECK_TIMEOUT, read_bounded_line(reader, &mut line)).await;

        if let Ok(Ok(n)) = read_result
            && n > 0
        {
            match ControlMessage::deserialize(line.trim()) {
                Ok(ControlMessage::Cancel {
                    id: cancel_id,
                    reason,
                }) if cancel_id == id => {
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
                Ok(ControlMessage::Pause { id: pause_id }) if pause_id == id => {
                    info!("Test {} paused", id);
                    if let Some(test) = active_tests.lock().await.get(id) {
                        let _ = test.pause_tx.send(true);
                    }
                }
                Ok(ControlMessage::Resume { id: resume_id }) if resume_id == id => {
                    info!("Test {} resumed", id);
                    if let Some(test) = active_tests.lock().await.get(id) {
                        let _ = test.pause_tx.send(false);
                    }
                }
                _ => {}
            }
        }
    }

    // Signal handlers to stop
    if let Some(test) = active_tests.lock().await.get(id) {
        let _ = test.cancel_tx.send(true);
    }

    // Wait for all data handlers to complete
    match (collection_handle, udp_handles) {
        (Some(handle), _) => {
            // TCP / single-port UDP: wait for the stream collection task,
            // then wait for individual handlers
            match handle.await {
                Ok(stream_handles) => {
                    let results = futures::future::join_all(stream_handles).await;
                    for result in results {
                        if let Err(e) = result
                            && e.is_panic()
                        {
                            error!("Stream handler panicked: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    if e.is_panic() {
                        error!("Stream collection task panicked: {:?}", e);
                    } else {
                        warn!("Stream collection task was cancelled");
                    }
                }
            }
        }
        (None, handles) if !handles.is_empty() => {
            // UDP/QUIC: wait for handlers directly
            let results = futures::future::join_all(handles).await;
            for result in results {
                if let Err(e) = result
                    && e.is_panic()
                {
                    error!("UDP/QUIC stream handler panicked: {:?}", e);
                }
            }
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

    // For bidir tests, expose per-direction totals so clients can distinguish
    // upload from download throughput on asymmetric links (issue #56).
    let (bytes_sent, bytes_received, throughput_send_mbps, throughput_recv_mbps) =
        directional_totals(direction, &stats, duration_ms);

    let result = ControlMessage::Result(TestResult {
        id: id.to_string(),
        bytes_total,
        duration_ms,
        throughput_mbps,
        streams: stream_results,
        tcp_info: stats.get_tcp_info(),
        udp_stats: stats.aggregate_udp_stats(),
        bytes_sent,
        bytes_received,
        throughput_send_mbps,
        throughput_recv_mbps,
        mtu_probe: None,
    });

    // Send result FIRST so the client isn't blocked by slow post-processing
    // (push gateway retries, metrics hooks, etc.)
    writer
        .write_all(format!("{}\n", result.serialize()?).as_bytes())
        .await?;

    Ok((bytes_total, duration_ms, throughput_mbps))
}
.await;

    // Always remove the active entry and stop data handlers.
    remove_active_test(&active_tests, id).await;

    match run_result {
        Ok((bytes_total, duration_ms, throughput_mbps)) => {
            #[cfg(feature = "prometheus")]
            crate::output::prometheus::on_test_complete(&stats);

            crate::output::push_gateway::maybe_push_metrics(push_gateway_url, &stats).await;

            if let Some(tx) = &tui_tx {
                let _ = tx.try_send(ServerEvent::TestCompleted {
                    id: id.to_string(),
                    bytes: bytes_total,
                });
            }

            info!(
                "Test {} complete: {:.2} Mbps, {} bytes",
                id, throughput_mbps, bytes_total
            );

            Ok((bytes_total, duration_ms, throughput_mbps))
        }
        Err(e) => {
            #[cfg(feature = "prometheus")]
            crate::output::prometheus::on_test_aborted(&stats);

            if let Some(tx) = &tui_tx {
                let _ = tx.try_send(ServerEvent::TestCompleted {
                    id: id.to_string(),
                    bytes: stats.total_bytes(),
                });
            }

            error!("Test {} ended with error: {}", id, e);
            Err(e)
        }
    }
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
#[allow(dead_code, clippy::too_many_arguments)]
async fn spawn_tcp_handlers(
    listeners: Vec<TcpListener>,
    stats: Arc<TestStats>,
    direction: Direction,
    duration: Duration,
    cancel: watch::Receiver<bool>,
    congestion: Option<String>,
    bitrate: Option<u64>,
    pause: watch::Receiver<bool>,
    client_window_size: Option<u64>,
) -> Vec<JoinHandle<()>> {
    let num_streams = listeners.len().max(1) as u64;
    let per_stream_bitrate = bitrate.map(|b| if b == 0 { 0 } else { (b / num_streams).max(1) });
    let mut handles = Vec::new();

    for (i, listener) in listeners.into_iter().enumerate() {
        let cancel = cancel.clone();
        let pause = pause.clone();
        let stream_stats = stats.streams[i].clone();
        let test_stats = stats.clone();
        let congestion = congestion.clone();

        let handle = tokio::spawn(async move {
            // Timeout on accept to prevent blocking forever if client never connects
            let accept_result =
                tokio::time::timeout(STREAM_ACCEPT_TIMEOUT, listener.accept()).await;

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

            // Respect the client's window_size if they passed -w; otherwise leave
            // kernel autotuning alone. Keep nodelay on to match historical behavior.
            let config = TcpConfig {
                nodelay: SERVER_DATA_NODELAY,
                window_size: client_window_to_host(client_window_size),
                congestion: congestion.clone(),
                random_payload: true,
                ..Default::default()
            };

            // Store fd for TCP_INFO interval polling
            #[cfg(unix)]
            {
                use std::os::unix::io::AsRawFd;
                stream_stats.set_tcp_info_fd(stream.as_raw_fd());
            }

            // Capture TCP_INFO before transfer starts
            if let Some(info) = tcp::get_stream_tcp_info(&stream) {
                test_stats.add_tcp_info(info);
            }

            match direction {
                Direction::Upload => {
                    // Server receives data
                    match tcp::receive_data(stream, stream_stats.clone(), cancel, config).await {
                        Ok(Some(info)) => test_stats.add_tcp_info(info),
                        Ok(None) => {}
                        Err(e) => tracing::warn!("Stream {} receive error: {}", i, e),
                    }
                }
                Direction::Download => {
                    // Server sends data - capture final TCP_INFO for RTT/retransmits
                    match tcp::send_data(
                        stream,
                        stream_stats.clone(),
                        duration,
                        config,
                        cancel,
                        per_stream_bitrate,
                        pause,
                    )
                    .await
                    {
                        Ok(Some(info)) => test_stats.add_tcp_info(info),
                        Ok(None) => {}
                        Err(e) => tracing::warn!("Stream {} send error: {}", i, e),
                    }
                }
                Direction::Bidir => {
                    // Configure socket BEFORE splitting (nodelay, window, buffers)
                    if let Err(e) = tcp::configure_stream(&stream, &config) {
                        tracing::error!("Failed to configure TCP socket: {}", e);
                        stream_stats.clear_tcp_info_fd();
                        return;
                    }

                    // Split socket for concurrent send/receive
                    let (read_half, write_half) = stream.into_split();

                    let send_stats = stream_stats.clone();
                    let recv_stats = stream_stats.clone();
                    let final_stats = stream_stats.clone();
                    let send_cancel = cancel.clone();
                    let recv_cancel = cancel;
                    let send_pause = pause;

                    let send_config = TcpConfig {
                        buffer_size: config.buffer_size,
                        nodelay: config.nodelay,
                        window_size: config.window_size,
                        congestion: config.congestion.clone(),
                        random_payload: true,
                        zerocopy: config.zerocopy,
                    };
                    let recv_config = config;

                    let send_handle = tokio::spawn(async move {
                        tcp::send_data_half(
                            write_half,
                            send_stats,
                            duration,
                            send_config,
                            send_cancel,
                            per_stream_bitrate,
                            send_pause,
                        )
                        .await
                    });

                    let recv_handle = tokio::spawn(async move {
                        tcp::receive_data_half(read_half, recv_stats, recv_cancel, recv_config)
                            .await
                    });

                    // Wait for both to complete. Use the clamp-time TCP_INFO snapshot
                    // returned by send_data_half rather than re-reading post-reunite.
                    let (send_result, recv_result) = tokio::join!(send_handle, recv_handle);
                    if let (Ok(Ok((write_half, send_tcp_info))), Ok(Ok(read_half))) =
                        (send_result, recv_result)
                    {
                        if let Some(info) = send_tcp_info {
                            final_stats.add_retransmits(info.retransmits);
                            test_stats.add_tcp_info(info);
                        }
                        let _ = read_half.reunite(write_half);
                    }
                }
            }

            // Clear fd to prevent stale fd reuse after stream closes
            stream_stats.clear_tcp_info_fd();
        });
        handles.push(handle);
    }

    handles
}

/// Single-port TCP mode: receive streams from channel and spawn handlers
#[allow(clippy::too_many_arguments)]
async fn spawn_tcp_stream_handlers(
    mut rx: mpsc::Receiver<(TcpStream, u16)>,
    num_streams: usize,
    stats: Arc<TestStats>,
    direction: Direction,
    duration: Duration,
    cancel: watch::Receiver<bool>,
    congestion: Option<String>,
    bitrate: Option<u64>,
    pause: watch::Receiver<bool>,
    dscp: Option<u8>,
    client_window_size: Option<u64>,
    zerocopy: bool,
    tcp_nodelay: bool,
) -> Vec<JoinHandle<()>> {
    let per_stream_bitrate = bitrate.map(|b| {
        if b == 0 {
            0
        } else {
            (b / num_streams as u64).max(1)
        }
    });
    let mut handles = Vec::new();
    let mut received = vec![false; num_streams];
    let deadline = tokio::time::Instant::now() + STREAM_COLLECTION_TIMEOUT;
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
            result = cancel.changed() => {
                match result {
                    Ok(()) if *cancel.borrow() => {
                        warn!("Test cancelled during stream collection");
                        break;
                    }
                    Ok(()) => continue,
                    Err(_) => {
                        warn!("Cancel channel closed during stream collection");
                        break;
                    }
                }
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
                let pause = pause.clone();
                let stream_stats = stats.streams[i].clone();
                let test_stats = stats.clone();

                let congestion = congestion.clone();
                let handle = tokio::spawn(async move {
                    let config = TcpConfig {
                        nodelay: server_data_nodelay(tcp_nodelay),
                        window_size: client_window_to_host(client_window_size),
                        congestion,
                        random_payload: true,
                        zerocopy,
                        ..Default::default()
                    };

                    // Store fd for TCP_INFO interval polling
                    #[cfg(unix)]
                    {
                        use std::os::unix::io::AsRawFd;
                        stream_stats.set_tcp_info_fd(stream.as_raw_fd());
                    }

                    // Capture TCP_INFO before transfer starts
                    if let Some(info) = tcp::get_stream_tcp_info(&stream) {
                        test_stats.add_tcp_info(info);
                    }

                    // Apply DSCP/TOS marking if requested by client
                    if let Some(tos) = dscp
                        && let Err(e) = crate::net::set_tos_on_tcp(&stream, tos)
                    {
                        tracing::warn!("Failed to set DSCP on server TCP socket: {}", e);
                    }

                    match direction {
                        Direction::Upload => {
                            match tcp::receive_data(stream, stream_stats.clone(), cancel, config)
                                .await
                            {
                                Ok(Some(info)) => test_stats.add_tcp_info(info),
                                Ok(None) => {}
                                Err(e) => tracing::warn!("Stream {} receive error: {}", i, e),
                            }
                        }
                        Direction::Download => {
                            match tcp::send_data(
                                stream,
                                stream_stats.clone(),
                                duration,
                                config,
                                cancel,
                                per_stream_bitrate,
                                pause,
                            )
                            .await
                            {
                                Ok(Some(info)) => test_stats.add_tcp_info(info),
                                Ok(None) => {}
                                Err(e) => tracing::warn!("Stream {} send error: {}", i, e),
                            }
                        }
                        Direction::Bidir => {
                            if let Err(e) = tcp::configure_stream(&stream, &config) {
                                tracing::error!("Failed to configure TCP socket: {}", e);
                                stream_stats.clear_tcp_info_fd();
                                return;
                            }
                            let (read_half, write_half) = stream.into_split();

                            let send_stats = stream_stats.clone();
                            let recv_stats = stream_stats.clone();
                            let final_stats = stream_stats.clone();
                            let send_cancel = cancel.clone();
                            let recv_cancel = cancel;
                            let send_pause = pause;
                            let send_config = config.clone();
                            let recv_config = config;

                            let send_handle = tokio::spawn(async move {
                                tcp::send_data_half(
                                    write_half,
                                    send_stats,
                                    duration,
                                    send_config,
                                    send_cancel,
                                    per_stream_bitrate,
                                    send_pause,
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
                            if let (Ok(Ok((write_half, send_tcp_info))), Ok(Ok(read_half))) =
                                (send_result, recv_result)
                            {
                                if let Some(info) = send_tcp_info {
                                    final_stats.add_retransmits(info.retransmits);
                                    test_stats.add_tcp_info(info);
                                }
                                let _ = read_half.reunite(write_half);
                            }
                        }
                    }

                    // Clear fd to prevent stale fd reuse after stream closes
                    stream_stats.clear_tcp_info_fd();
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

#[allow(clippy::too_many_arguments)]
async fn spawn_udp_handlers(
    sockets: Vec<Arc<UdpSocket>>,
    stats: Arc<TestStats>,
    direction: Direction,
    duration: Duration,
    bitrate: u64,
    cancel: watch::Receiver<bool>,
    pause: watch::Receiver<bool>,
    dscp: Option<u8>,
    client_supports_udp_feedback: bool,
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
        // Apply DSCP/TOS marking if requested by client
        if let Some(tos) = dscp
            && let Err(e) = crate::net::set_tos_on_udp(&socket, tos)
        {
            tracing::warn!("Failed to set DSCP on server UDP socket {}: {}", i, e);
        }

        let stream_stats = stats.streams[i].clone();
        let test_stats = stats.clone();
        let cancel = cancel.clone();
        let pause = pause.clone();

        let handle = tokio::spawn(async move {
            match direction {
                Direction::Upload => {
                    // Server receives UDP - capture stats. Feedback emission
                    // is gated on negotiated capability so old clients (which
                    // wouldn't know to listen) never see a 36-byte packet
                    // they don't understand.
                    if let Ok((udp_stats, _bytes)) = udp::receive_udp(
                        socket,
                        stream_stats,
                        cancel,
                        pause,
                        client_supports_udp_feedback,
                        None,
                    )
                    .await
                    {
                        test_stats.add_udp_stats(udp_stats);
                    }
                }
                Direction::Download => {
                    // Wait for client's hello packet to learn their address
                    match udp::wait_for_client(&socket, STREAM_ACCEPT_TIMEOUT).await {
                        Ok(client_addr) => {
                            // Server sends UDP at per-stream rate to client
                            let _ = udp::send_udp_paced(
                                socket,
                                Some(client_addr),
                                per_stream_bitrate,
                                duration,
                                stream_stats,
                                cancel,
                                pause,
                                true,
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
                    match udp::wait_for_client(&socket, STREAM_ACCEPT_TIMEOUT).await {
                        Ok(client_addr) => {
                            // UDP can send/receive concurrently on same socket
                            let send_socket = socket.clone();
                            let recv_socket = socket;
                            let send_stats = stream_stats.clone();
                            let recv_stats = stream_stats;
                            let send_cancel = cancel.clone();
                            let recv_cancel = cancel;
                            let send_pause = pause.clone();
                            let recv_pause = pause;
                            let test_stats_copy = test_stats.clone();

                            let send_handle = tokio::spawn(async move {
                                let _ = udp::send_udp_paced(
                                    send_socket,
                                    Some(client_addr),
                                    per_stream_bitrate,
                                    duration,
                                    send_stats,
                                    send_cancel,
                                    send_pause,
                                    true,
                                )
                                .await;
                            });

                            let recv_handle = tokio::spawn(async move {
                                // Bidir: pass `false` even if the peer
                                // advertised udp_feedback_v1. The client's
                                // bidir recv half doesn't spawn a feedback
                                // consumer (only upload mode does), so any
                                // feedback we emitted here would be
                                // received as bytes that don't belong to
                                // the test data flow. Feedback is
                                // upload-mode-only by design.
                                if let Ok((udp_stats, _bytes)) = udp::receive_udp(
                                    recv_socket,
                                    recv_stats,
                                    recv_cancel,
                                    recv_pause,
                                    false,
                                    None,
                                )
                                .await
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

/// Single-port UDP mode (issue #63): receive connected per-stream
/// sockets from the hello dispatcher and spawn the per-direction
/// handlers on each, mirroring `spawn_tcp_stream_handlers`. Each
/// handler carries the precomputed hello ack so retried hellos (which
/// the kernel routes to the connected socket once it exists) get
/// re-acked instead of polluting the data accounting.
#[allow(clippy::too_many_arguments)]
async fn spawn_single_port_udp_handlers(
    mut rx: mpsc::Receiver<(Arc<UdpSocket>, u16)>,
    num_streams: usize,
    token: [u8; 16],
    stats: Arc<TestStats>,
    direction: Direction,
    duration: Duration,
    bitrate: u64,
    cancel: watch::Receiver<bool>,
    pause: watch::Receiver<bool>,
    dscp: Option<u8>,
    client_supports_udp_feedback: bool,
    mtu_probe: bool,
) -> Vec<JoinHandle<()>> {
    let per_stream_bitrate = if bitrate == 0 {
        0 // Unlimited mode (explicit -b 0)
    } else {
        (bitrate / num_streams.max(1) as u64).max(1)
    };
    let mut handles = Vec::new();
    let mut received = vec![false; num_streams];
    let deadline = tokio::time::Instant::now() + STREAM_COLLECTION_TIMEOUT;
    let mut cancel = cancel;

    while received.iter().any(|&r| !r) {
        if *cancel.borrow() {
            warn!("Test cancelled during UDP stream collection");
            break;
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            warn!("Timeout waiting for all single-port UDP streams");
            break;
        }

        let stream_result = tokio::select! {
            biased;
            result = cancel.changed() => {
                match result {
                    Ok(()) if *cancel.borrow() => {
                        warn!("Test cancelled during UDP stream collection");
                        break;
                    }
                    Ok(()) => continue,
                    Err(_) => {
                        warn!("Cancel channel closed during UDP stream collection");
                        break;
                    }
                }
            }
            result = tokio::time::timeout(remaining, rx.recv()) => result,
        };

        match stream_result {
            Ok(Some((socket, stream_index))) => {
                let i = stream_index as usize;
                if i >= num_streams {
                    warn!("Invalid UDP stream index: {}", stream_index);
                    continue;
                }
                if received[i] {
                    warn!("Duplicate UDP stream index: {}", stream_index);
                    continue;
                }
                received[i] = true;

                // Apply DSCP/TOS marking if requested by client
                if let Some(tos) = dscp
                    && let Err(e) = crate::net::set_tos_on_udp(&socket, tos)
                {
                    warn!("Failed to set DSCP on server UDP socket {}: {}", i, e);
                }

                let ack = udp::UdpHelloPacket {
                    kind: udp::UDP_HELLO_KIND_ACK,
                    stream_index,
                    token,
                }
                .encode();
                let stream_stats = stats.streams[i].clone();
                let test_stats = stats.clone();
                let cancel = cancel.clone();
                let pause = pause.clone();

                let handle = tokio::spawn(async move {
                    if mtu_probe {
                        // Probe mode replaces bulk handlers, same as the
                        // legacy branch in run_test; DF is best-effort.
                        let ipv6 = socket.local_addr().map(|a| a.is_ipv6()).unwrap_or(false);
                        if let Err(e) = net::set_dont_fragment(&socket, ipv6) {
                            warn!(
                                "MTU probe: could not set don't-fragment on echo socket: {} \
                                 (oversized echoes may fragment and overstate the reverse path)",
                                e
                            );
                        }
                        if let Err(e) = udp::respond_mtu_probes(socket, cancel, Some(ack)).await {
                            error!("MTU probe responder error: {}", e);
                        }
                        return;
                    }
                    match direction {
                        Direction::Upload => {
                            if let Ok((udp_stats, _bytes)) = udp::receive_udp(
                                socket,
                                stream_stats,
                                cancel,
                                pause,
                                client_supports_udp_feedback,
                                Some(ack),
                            )
                            .await
                            {
                                test_stats.add_udp_stats(udp_stats);
                            }
                        }
                        Direction::Download => {
                            // Nothing else recvs on this socket in download
                            // mode, so a dedicated responder keeps answering
                            // hello retries while the sender runs.
                            let responder_socket = socket.clone();
                            let responder_cancel = cancel.clone();
                            let responder = tokio::spawn(async move {
                                udp::respond_single_port_hellos(
                                    responder_socket,
                                    ack,
                                    responder_cancel,
                                )
                                .await;
                            });
                            let _ = udp::send_udp_paced(
                                socket,
                                None, // connected socket
                                per_stream_bitrate,
                                duration,
                                stream_stats,
                                cancel,
                                pause,
                                true,
                            )
                            .await;
                            let _ = responder.await;
                        }
                        Direction::Bidir => {
                            let send_socket = socket.clone();
                            let recv_socket = socket;
                            let send_stats = stream_stats.clone();
                            let recv_stats = stream_stats;
                            let send_cancel = cancel.clone();
                            let recv_cancel = cancel;
                            let send_pause = pause.clone();
                            let recv_pause = pause;

                            let send_handle = tokio::spawn(async move {
                                let _ = udp::send_udp_paced(
                                    send_socket,
                                    None, // connected socket
                                    per_stream_bitrate,
                                    duration,
                                    send_stats,
                                    send_cancel,
                                    send_pause,
                                    true,
                                )
                                .await;
                            });

                            let recv_handle = tokio::spawn(async move {
                                // Feedback stays upload-mode-only (see the
                                // legacy bidir handler); the recv side also
                                // re-acks hello retries here.
                                if let Ok((udp_stats, _bytes)) = udp::receive_udp(
                                    recv_socket,
                                    recv_stats,
                                    recv_cancel,
                                    recv_pause,
                                    false,
                                    Some(ack),
                                )
                                .await
                                {
                                    test_stats.add_udp_stats(udp_stats);
                                }
                            });

                            let _ = tokio::join!(send_handle, recv_handle);
                        }
                    }
                });
                handles.push(handle);
            }
            Ok(None) => {
                warn!("UDP socket channel closed");
                break;
            }
            Err(_) => {
                warn!("Timeout waiting for single-port UDP stream");
                break;
            }
        }
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

/// Remove an active test entry and signal any running data handlers to stop.
/// This is the cleanup path used after both normal completion and early errors.
async fn remove_active_test(active_tests: &Mutex<HashMap<String, ActiveTest>>, id: &str) {
    if let Some(test) = active_tests.lock().await.remove(id) {
        let _ = test.cancel_tx.send(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_remove_active_test_cleans_entry_and_sends_cancel() {
        let active_tests = Arc::new(Mutex::new(HashMap::new()));
        let id = "leak-test";
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (pause_tx, _) = watch::channel(false);
        active_tests.lock().await.insert(
            id.to_string(),
            ActiveTest {
                stats: Arc::new(TestStats::new(id.to_string(), 1)),
                cancel_tx,
                pause_tx,
                data_ports: vec![],
                data_stream_tx: None,
                control_peer_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                expected_streams: 1,
            },
        );

        remove_active_test(active_tests.as_ref(), id).await;

        assert!(!active_tests.lock().await.contains_key(id));
        assert!(*cancel_rx.borrow(), "cleanup must send cancel signal");
    }

    fn make_active_test(streams: u8, ip: std::net::IpAddr) -> ActiveTest {
        let (cancel_tx, _) = watch::channel(false);
        let (pause_tx, _) = watch::channel(false);
        ActiveTest {
            stats: Arc::new(TestStats::new("test".to_string(), streams.max(1))),
            cancel_tx,
            pause_tx,
            data_ports: vec![],
            data_stream_tx: None,
            control_peer_ip: ip,
            expected_streams: streams,
        }
    }

    #[test]
    fn test_server_data_nodelay_honors_client_and_never_disables() {
        // A client that passed --tcp-nodelay must end up with nodelay on the
        // server's data sockets (the bulk-send direction in -R/--bidir)...
        assert!(server_data_nodelay(true));
        // ...and a client that didn't gets exactly the server default, which
        // is always-on today — so the historical behavior cannot regress.
        assert_eq!(server_data_nodelay(false), SERVER_DATA_NODELAY);
    }

    #[test]
    fn test_initial_read_timeout_for_peer_default() {
        let tests: HashMap<String, ActiveTest> = HashMap::new();
        assert_eq!(
            initial_read_timeout_for_peer(
                &tests,
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            ),
            INITIAL_READ_TIMEOUT
        );
    }

    #[test]
    fn test_initial_read_timeout_for_peer_scales_and_caps() {
        let mut tests = HashMap::new();
        let peer = std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 2));
        tests.insert("a".to_string(), make_active_test(4, peer));
        assert_eq!(
            initial_read_timeout_for_peer(&tests, peer),
            INITIAL_READ_TIMEOUT
        );

        tests.insert("b".to_string(), make_active_test(128, peer));
        assert_eq!(
            initial_read_timeout_for_peer(&tests, peer),
            Duration::from_millis(128 * INITIAL_READ_TIMEOUT_PER_STREAM_MS)
        );

        tests.insert("c".to_string(), make_active_test(255, peer));
        assert_eq!(
            initial_read_timeout_for_peer(&tests, peer),
            INITIAL_READ_TIMEOUT_MAX
        );
    }

    #[test]
    fn test_initial_read_timeout_for_peer_ignores_other_clients() {
        let mut tests = HashMap::new();
        let peer_a = std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 2));
        let peer_b = std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 3));
        tests.insert("a".to_string(), make_active_test(128, peer_a));
        tests.insert("b".to_string(), make_active_test(4, peer_b));

        assert_eq!(
            initial_read_timeout_for_peer(&tests, peer_b),
            INITIAL_READ_TIMEOUT
        );
    }

    fn make_udp_route(streams: u8, ip: IpAddr) -> UdpHelloRoute {
        let (socket_tx, _rx) = mpsc::channel(1);
        UdpHelloRoute {
            socket_tx,
            control_peer_ip: ip,
            expected_streams: streams,
            udp_buffer: None,
            sockets: vec![None; streams as usize],
        }
    }

    fn make_hello(stream_index: u16, token: [u8; 16]) -> udp::UdpHelloPacket {
        udp::UdpHelloPacket {
            kind: udp::UDP_HELLO_KIND_HELLO,
            stream_index,
            token,
        }
    }

    #[test]
    fn test_check_udp_hello_unknown_token_rejected() {
        // A hello whose token misses the routing table must be dropped —
        // this is the token-mismatch path (the map is keyed by token, so
        // a wrong token IS a missed lookup).
        let pkt = make_hello(0, [9; 16]);
        let src: IpAddr = "10.0.0.2".parse().unwrap();
        assert_eq!(
            check_udp_hello(None, &pkt, src),
            UdpHelloVerdict::UnknownToken
        );
    }

    #[test]
    fn test_check_udp_hello_ip_mismatch_rejected() {
        let route = make_udp_route(2, "10.0.0.2".parse().unwrap());
        let pkt = make_hello(0, [1; 16]);
        assert_eq!(
            check_udp_hello(Some(&route), &pkt, "10.0.0.3".parse().unwrap()),
            UdpHelloVerdict::IpMismatch
        );
    }

    #[test]
    fn test_check_udp_hello_accepts_and_normalizes_mapped_ip() {
        // Control over IPv4, hello observed as IPv4-mapped IPv6 on the
        // dual-stack shared socket: must still match.
        let route = make_udp_route(2, "10.0.0.2".parse().unwrap());
        let pkt = make_hello(1, [1; 16]);
        let mapped: IpAddr = "::ffff:10.0.0.2".parse().unwrap();
        assert_eq!(
            check_udp_hello(Some(&route), &pkt, mapped),
            UdpHelloVerdict::Accept
        );
    }

    #[test]
    fn test_check_udp_hello_stream_bounds_and_resend() {
        let mut route = make_udp_route(2, "10.0.0.2".parse().unwrap());
        let src: IpAddr = "10.0.0.2".parse().unwrap();

        let out_of_range = make_hello(2, [1; 16]);
        assert_eq!(
            check_udp_hello(Some(&route), &out_of_range, src),
            UdpHelloVerdict::BadStreamIndex
        );

        // A retried hello for a stream that already has its connected
        // socket re-acks instead of creating a second socket.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let socket = runtime
            .block_on(tokio::net::UdpSocket::bind("127.0.0.1:0"))
            .unwrap();
        route.sockets[1] = Some(Arc::new(socket));
        let retry = make_hello(1, [1; 16]);
        assert_eq!(
            check_udp_hello(Some(&route), &retry, src),
            UdpHelloVerdict::Resend
        );
    }

    #[test]
    fn test_initial_read_timeout_for_peer_normalizes_ipv4_mapped_ipv6() {
        let mut tests = HashMap::new();
        let mapped = std::net::IpAddr::V6("::ffff:10.0.0.2".parse().unwrap());
        let v4 = std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 2));
        tests.insert("a".to_string(), make_active_test(128, mapped));
        assert_eq!(
            initial_read_timeout_for_peer(&tests, v4),
            Duration::from_millis(128 * INITIAL_READ_TIMEOUT_PER_STREAM_MS)
        );
    }
}
