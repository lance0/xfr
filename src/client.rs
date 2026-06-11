//! Client mode implementation
//!
//! Connects to a server and runs bandwidth tests.

use parking_lot::Mutex;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Semaphore, mpsc, watch};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Maximum control message line length to prevent memory DoS
const MAX_LINE_LENGTH: usize = 65536;
const STREAM_JOIN_TIMEOUT_BASE: Duration = Duration::from_secs(2);
const STREAM_JOIN_TIMEOUT_PER_STREAM_MS: u64 = 50;
const SINGLE_PORT_HANDSHAKE_FANOUT_MAX: usize = 16;

fn stream_join_timeout(streams: u8) -> Duration {
    let scaled =
        Duration::from_millis(u64::from(streams).saturating_mul(STREAM_JOIN_TIMEOUT_PER_STREAM_MS));
    if scaled > STREAM_JOIN_TIMEOUT_BASE {
        scaled
    } else {
        STREAM_JOIN_TIMEOUT_BASE
    }
}

fn single_port_handshake_parallelism(streams: u8) -> usize {
    usize::from(streams).clamp(1, SINGLE_PORT_HANDSHAKE_FANOUT_MAX)
}

fn local_stop_deadline(
    start: tokio::time::Instant,
    duration: Duration,
) -> Option<tokio::time::Instant> {
    if duration == Duration::ZERO {
        None
    } else {
        Some(start + duration)
    }
}

/// Per-stream and total receive-byte deltas since the previous call,
/// advancing `last_recv_bytes` in place. Used in download mode to derive
/// interval throughput from the client's own receive counters instead of
/// the server's send-side counts (issue #81): a UDP sender's bytes_sent
/// climbs at the requested rate no matter what the wire delivers, so only
/// the receiver can report wire truth.
fn udp_recv_interval_deltas(
    stats: &crate::stats::TestStats,
    last_recv_bytes: &mut [u64],
) -> (u64, Vec<u64>) {
    let deltas: Vec<u64> = stats
        .streams
        .iter()
        .enumerate()
        .map(|(idx, stream)| {
            let cumulative = stream
                .bytes_received
                .load(std::sync::atomic::Ordering::Relaxed);
            let prev = match last_recv_bytes.get_mut(idx) {
                Some(slot) => std::mem::replace(slot, cumulative),
                None => 0,
            };
            cumulative.saturating_sub(prev)
        })
        .collect();
    let total = deltas.iter().sum();
    (total, deltas)
}

/// Cumulative UDP receive progress (packets received / lost) summed across
/// the client's own streams. In download mode the client is the receiver,
/// so this is the authoritative live-loss source; the server has no
/// receiver-side counters to report.
fn udp_local_progress(stats: &crate::stats::TestStats) -> crate::protocol::UdpIntervalProgress {
    stats.streams.iter().fold(
        crate::protocol::UdpIntervalProgress::default(),
        |mut acc, stream| {
            let p = stream.udp_progress_snapshot();
            acc.packets_received += p.packets_received;
            acc.packets_lost += p.packets_lost;
            acc
        },
    )
}

/// Replace the sender-side byte accounting in a download-mode UDP result
/// with the client's own receive counters (issue #81). The server's
/// `bytes_total` counts every `send_to` the kernel accepted — over a
/// constrained link that tracks the requested `-b` rate, not what arrived.
/// Also fills `udp_stats` (loss/jitter) from the client's receiver-side
/// trackers, which the server cannot know in download mode. Must run after
/// the receive tasks have been joined so their final `UdpStats` are
/// recorded.
fn overlay_udp_download_result(result: &mut TestResult, stats: &crate::stats::TestStats) {
    let mbps = |bytes: u64| {
        if result.duration_ms > 0 {
            (bytes as f64 * 8.0) / (result.duration_ms as f64 / 1000.0) / 1_000_000.0
        } else {
            0.0
        }
    };
    let received = stats.total_bytes_received();
    result.bytes_total = received;
    result.throughput_mbps = mbps(received);
    for stream_result in result.streams.iter_mut() {
        if let Some(local) = stats.streams.get(stream_result.id as usize) {
            let bytes = local
                .bytes_received
                .load(std::sync::atomic::Ordering::Relaxed);
            stream_result.bytes = bytes;
            stream_result.throughput_mbps = mbps(bytes);
        }
    }
    // Server-side aggregate is None in download mode (it received nothing);
    // keep it if a future server ever reports something rather than clobber.
    if result.udp_stats.is_none() {
        result.udp_stats = stats.aggregate_udp_stats();
    }
}

/// Bidir analog of [`overlay_udp_download_result`]: the download direction of
/// a bidirectional UDP result (`bytes_received` from the client's view) is
/// computed server-side from the server's *send* counters, which inflate the
/// same way issue #81 described — every kernel-accepted `send_to` counts,
/// whether or not it survived the wire. Replace it with the client's own
/// receive counters. The upload direction (`bytes_sent`) is already
/// receiver-truth — the server counts what actually arrived — so it is kept,
/// and the combined totals are recomputed from the two corrected halves.
/// Pre-split servers (no `bytes_sent` reported) only get the receive-side
/// fields filled in; the combined total is left alone rather than rebuilt
/// from a half-known pair.
fn overlay_udp_bidir_result(result: &mut TestResult, stats: &crate::stats::TestStats) {
    let mbps = |bytes: u64| {
        if result.duration_ms > 0 {
            (bytes as f64 * 8.0) / (result.duration_ms as f64 / 1000.0) / 1_000_000.0
        } else {
            0.0
        }
    };
    let received = stats.total_bytes_received();
    result.bytes_received = Some(received);
    result.throughput_recv_mbps = Some(mbps(received));
    if let Some(sent) = result.bytes_sent {
        result.bytes_total = sent + received;
        result.throughput_mbps = mbps(sent + received);
    }
}

use crate::auth;
use crate::net::{self, AddressFamily};
use crate::protocol::{
    ControlMessage, Direction, PROTOCOL_VERSION, Protocol, StreamInterval, TestResult,
    versions_compatible,
};
use crate::quic;
use crate::stats::TestStats;
use crate::tcp::{self, TcpConfig};
use crate::udp;

/// Result of a pause toggle attempt
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseResult {
    /// Pause/resume applied to transport and server
    Applied,
    /// Server doesn't support pause/resume
    Unsupported,
    /// No active test running or channels not ready
    NotReady,
}

/// How zero-copy TCP sends (sendfile(2), issue #33) were selected.
///
/// `Auto` and `Requested` are identical on the wire and the send path; they
/// differ only in how loudly downgrades are reported. The default-on path
/// stays quiet when zero-copy isn't available (non-Linux, old kernels, old
/// servers), while an explicit `-Z` warns so the user knows their request
/// didn't take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZerocopyMode {
    /// Zero-copy disabled (`--no-zerocopy`, or non-TCP protocols).
    Off,
    /// On by default for TCP; downgrades are silent.
    Auto,
    /// Explicitly requested (`-Z`); downgrades warn.
    Requested,
}

impl ZerocopyMode {
    pub fn enabled(self) -> bool {
        !matches!(self, ZerocopyMode::Off)
    }
}

#[derive(Clone)]
pub struct ClientConfig {
    pub host: String,
    pub port: u16,
    pub protocol: Protocol,
    pub streams: u8,
    pub duration: Duration,
    pub direction: Direction,
    pub bitrate: Option<u64>,
    pub tcp_nodelay: bool,
    pub tcp_congestion: Option<String>,
    pub window_size: Option<usize>,
    /// Pre-shared key for authentication
    pub psk: Option<String>,
    /// Address family preference
    pub address_family: AddressFamily,
    /// Local address to bind to for data sockets. For TCP with `--cport`,
    /// control still uses the same IP but an ephemeral port.
    pub bind_addr: Option<SocketAddr>,
    /// Use sequential ports for multi-stream (--cport with -P) on TCP/UDP
    pub sequential_ports: bool,
    /// Use MPTCP (Multi-Path TCP) instead of regular TCP
    pub mptcp: bool,
    /// Use random payload data for client-sent traffic
    pub random_payload: bool,
    /// Zero-copy TCP sends via sendfile(2) (Linux only, issue #33). Applies
    /// to client-sent traffic and is forwarded to the server for its sends
    /// in download/bidir modes. On by default for TCP ([`ZerocopyMode::Auto`]);
    /// falls back to regular writes when unsupported.
    pub zerocopy: ZerocopyMode,
    /// DSCP/TOS value for IP_TOS socket option
    pub dscp: Option<u8>,
    /// Run a path-MTU probe instead of a throughput test (issue #64,
    /// `--probe-mtu`). UDP only; requires a server advertising
    /// `mtu_probe_v1`.
    pub mtu_probe: bool,
    /// Bound on establishing the control connection (TCP connect or
    /// QUIC handshake). `None` leaves the OS defaults, which against a
    /// dead or filtered server can mean minutes — CI and scripted runs
    /// set this for a fast, clear failure instead.
    pub connect_timeout: Option<Duration>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: crate::protocol::DEFAULT_PORT,
            protocol: Protocol::Tcp,
            streams: 1,
            duration: Duration::from_secs(10),
            direction: Direction::Upload,
            bitrate: None,
            tcp_nodelay: false,
            tcp_congestion: None,
            window_size: None,
            psk: None,
            address_family: AddressFamily::default(),
            bind_addr: None,
            sequential_ports: false,
            mptcp: false,
            random_payload: true,
            zerocopy: ZerocopyMode::Auto,
            dscp: None,
            mtu_probe: false,
            connect_timeout: None,
        }
    }
}

#[derive(Clone)]
pub struct TestProgress {
    pub elapsed_ms: u64,
    pub total_bytes: u64,
    pub throughput_mbps: f64,
    pub streams: Vec<StreamInterval>,
    pub rtt_us: Option<u32>,
    pub cwnd: Option<u32>,
    /// Cumulative retransmits from local TCP_INFO (sender-side, for upload/bidir)
    pub total_retransmits: Option<u64>,
    /// Per-direction interval bytes (bidirectional tests only, from client's
    /// perspective). `bytes_sent` is what the client sent this interval.
    pub bytes_sent: Option<u64>,
    pub bytes_received: Option<u64>,
    pub throughput_send_mbps: Option<f64>,
    pub throughput_recv_mbps: Option<f64>,
    /// Cumulative UDP packet counts as of this interval. `None` means the
    /// server didn't ship the field (TCP run, no traffic yet, or paired with
    /// a pre-0.9.11 server). The TUI distinguishes None from a real zero so
    /// stale/unknown data renders as a freshness signal, not as "no loss".
    pub udp_progress: Option<crate::protocol::UdpIntervalProgress>,
    /// True when this update was synthesized from a UDP-receiver-feedback
    /// path (server emitted cumulative counts back over UDP at 2 Hz under
    /// the `udp_feedback_v1` capability) rather than from a TCP control
    /// `Interval` message. Consumers should update only `udp_progress` /
    /// derived loss state and preserve all other field values from the
    /// most recent full interval — the feedback-only update knows nothing
    /// about throughput, byte counts, retransmits, RTT, or jitter.
    pub udp_feedback_only: bool,
}

/// Producer-side monotonic-denominator filter. Both the TCP control
/// `udp_progress` decode path and the UDP feedback aggregator route
/// updates through this so a delayed TCP `Interval` message can never
/// clobber a fresher feedback reading (or vice versa). Both paths
/// report cumulative monotonic counts from the same `StreamStats`
/// atomics on the server, so last-monotonic-write-wins is correct.
///
/// This also rejects an injected feedback packet that carries a lower
/// count than what we've seen — a minor anti-spoofing property.
pub struct UdpProgressFilter {
    max_denominator: std::sync::atomic::AtomicU64,
}

impl UdpProgressFilter {
    pub fn new() -> Self {
        Self {
            max_denominator: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Returns `Some(progress)` if the cumulative `(received + lost)`
    /// denominator is at-least-as-fresh as anything we've seen before;
    /// `None` to reject the update as stale.
    ///
    /// Uses `fetch_update` for an atomic compare-and-swap rather than a
    /// load/store pair: two producers (TCP control decode site and the
    /// UDP feedback aggregator) can race here, and a load-compare-store
    /// can let the *lower* denominator win the store after the higher
    /// one already committed, which would re-admit a later stale update
    /// and weaken the freshness guarantee. `fetch_update` retries until
    /// the CAS succeeds, so the post-condition is "no smaller value
    /// ever overwrites a larger one."
    pub fn apply(
        &self,
        progress: crate::protocol::UdpIntervalProgress,
    ) -> Option<crate::protocol::UdpIntervalProgress> {
        let denom = progress
            .packets_received
            .saturating_add(progress.packets_lost);
        let result = self.max_denominator.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |current| {
                if denom >= current { Some(denom) } else { None }
            },
        );
        match result {
            Ok(_) => Some(progress),
            Err(_) => None,
        }
    }
}

impl Default for UdpProgressFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-test, per-stream UDP feedback aggregator. Each
/// `udp::receive_udp_feedback_only` task owns one slot (indexed by
/// stream_id) and writes its latest cumulative `(received, lost)`
/// snapshot whenever a feedback packet decodes. A 1 Hz consumer task
/// reads all slots, sums them into a single `UdpIntervalProgress`,
/// applies the producer-side filter, and emits a feedback-only
/// `TestProgress` to consumers.
pub type UdpFeedbackAggregator = Arc<Mutex<Vec<crate::protocol::UdpIntervalProgress>>>;

/// Spawn the per-test UDP-feedback consumer task. Once per second it
/// snapshots all per-stream feedback slots, sums them into a single
/// cumulative aggregate, runs it through the producer-side monotonic
/// filter, and sends a feedback-only `TestProgress` to consumers (TUI,
/// plain-text, JSON-stream). Exits when `cancel` becomes true.
///
/// The 1 Hz cadence matches the existing TCP-control interval cadence,
/// so consumers don't see double the update rate. The 2 Hz wire
/// cadence on the server emission side is the redundancy buffer for
/// dropped feedback packets — the consumer task always reads the
/// freshest slot and ignores the rest.
fn spawn_udp_feedback_consumer(
    aggregator: UdpFeedbackAggregator,
    filter: Arc<UdpProgressFilter>,
    progress_tx: Option<mpsc::Sender<TestProgress>>,
    mut cancel: watch::Receiver<bool>,
) {
    let Some(tx) = progress_tx else {
        // No consumer to feed — skip the task entirely.
        return;
    };
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        // Skip stale ticks. Cumulative counts make the next tick
        // self-correcting; redelivering a backlog of stale snapshots
        // helps no one.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let aggregate = {
                        let slots = aggregator.lock();
                        let mut total = crate::protocol::UdpIntervalProgress {
                            packets_received: 0,
                            packets_lost: 0,
                        };
                        for s in slots.iter() {
                            total.packets_received =
                                total.packets_received.saturating_add(s.packets_received);
                            total.packets_lost =
                                total.packets_lost.saturating_add(s.packets_lost);
                        }
                        total
                    };
                    // Skip empty aggregates — the consumer task starts
                    // before any feedback packet has arrived, and an
                    // all-zero update would mask a real prior value
                    // already in TUI state.
                    if aggregate.packets_received == 0 && aggregate.packets_lost == 0 {
                        continue;
                    }
                    if let Some(filtered) = filter.apply(aggregate) {
                        let elapsed_ms = start.elapsed().as_millis() as u64;
                        // Feedback-only update: udp_progress carries the
                        // truth, every other field is sentinel/None and
                        // consumers preserve their last full-interval
                        // values.
                        let _ = tx
                            .send(TestProgress {
                                elapsed_ms,
                                total_bytes: 0,
                                throughput_mbps: 0.0,
                                streams: Vec::new(),
                                rtt_us: None,
                                cwnd: None,
                                total_retransmits: None,
                                bytes_sent: None,
                                bytes_received: None,
                                throughput_send_mbps: None,
                                throughput_recv_mbps: None,
                                udp_progress: Some(filtered),
                                udp_feedback_only: true,
                            })
                            .await;
                    }
                }
                result = cancel.changed() => {
                    // Treat sender-dropped (Err) as cancel: without this
                    // guard, a panic in the parent task would drop the
                    // watch sender, every subsequent cancel.changed()
                    // call would return Err immediately, and *borrow()
                    // would observe the last-known `false`, busy-looping
                    // through the select! arm forever.
                    if result.is_err() || *cancel.borrow() {
                        break;
                    }
                }
            }
        }
    });
}

pub struct Client {
    config: ClientConfig,
    /// Signals data stream handlers to stop
    cancel_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
    /// Signals the control loop to send a Cancel message to server
    cancel_request_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
    /// Signals data stream handlers to pause/resume
    pause_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
    /// Signals the control loop to send a Pause/Resume message to server
    pause_request_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
    /// Whether the server supports pause/resume capability
    server_supports_pause: Arc<Mutex<Option<bool>>>,
    /// Server's advertised version string (from ServerHello), exposed via
    /// [`Client::server_version`] for the TUI to display.
    server_version: Arc<Mutex<Option<String>>>,
}

impl Client {
    pub fn new(config: ClientConfig) -> Self {
        Self {
            config,
            cancel_tx: Arc::new(Mutex::new(None)),
            cancel_request_tx: Arc::new(Mutex::new(None)),
            pause_tx: Arc::new(Mutex::new(None)),
            pause_request_tx: Arc::new(Mutex::new(None)),
            server_supports_pause: Arc::new(Mutex::new(None)),
            server_version: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns the server's advertised version string (from the `Hello`
    /// handshake) if it has been received, otherwise `None`.
    pub fn server_version(&self) -> Option<String> {
        self.server_version.lock().clone()
    }

    pub async fn run(
        &self,
        progress_tx: Option<mpsc::Sender<TestProgress>>,
    ) -> anyhow::Result<TestResult> {
        info!("Connecting to {}:{}...", self.config.host, self.config.port);

        if self.config.protocol == Protocol::Quic && self.config.bitrate.is_some() {
            warn!("Bitrate limit (-b) not implemented for QUIC; running at full speed");
        }
        if self.config.protocol != Protocol::Tcp && self.config.tcp_congestion.is_some() {
            warn!(
                "--congestion is only supported for TCP; ignoring for {}",
                self.config.protocol
            );
        }

        // Use QUIC transport if selected
        if self.config.protocol == Protocol::Quic {
            return self.run_quic(progress_tx).await;
        }

        let control_bind_addr = control_bind_addr(&self.config);
        let connect = net::connect_tcp(
            &self.config.host,
            self.config.port,
            self.config.address_family,
            control_bind_addr,
            self.config.mptcp,
        );
        let (stream, peer_addr) = match self.config.connect_timeout {
            Some(limit) => tokio::time::timeout(limit, connect).await.map_err(|_| {
                anyhow::anyhow!(
                    "Timed out connecting to {}:{} after {:?} (--connect-timeout)",
                    self.config.host,
                    self.config.port,
                    limit
                )
            })??,
            None => connect.await?,
        };

        if self.config.protocol == Protocol::Tcp {
            validate_tcp_control_port(stream.local_addr()?, &self.config)?;
        }

        self.run_test(stream, peer_addr, progress_tx).await
    }

    async fn run_test(
        &self,
        stream: TcpStream,
        server_ip: SocketAddr,
        progress_tx: Option<mpsc::Sender<TestProgress>>,
    ) -> anyhow::Result<TestResult> {
        // Reset pause state from any previous run
        *self.server_supports_pause.lock() = None;
        *self.pause_tx.lock() = None;
        *self.pause_request_tx.lock() = None;
        // Reset server version too — a reused Client connecting to a server
        // that doesn't advertise `server` in its Hello must not carry the
        // prior peer's string forward into the TUI display.
        *self.server_version.lock() = None;

        if let Err(e) = tcp::configure_control_stream(&stream) {
            warn!("Failed to set TCP_NODELAY on control stream: {}", e);
        }
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();

        // Send client hello
        let hello = ControlMessage::client_hello();
        writer
            .write_all(format!("{}\n", hello.serialize()?).as_bytes())
            .await?;

        // Read server hello (bounded to prevent DoS)
        read_bounded_line(&mut reader, &mut line).await?;
        let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

        // Track server capabilities for feature detection
        let server_capabilities;

        match msg {
            ControlMessage::Hello {
                version,
                capabilities,
                auth,
                server,
                ..
            } => {
                if !versions_compatible(&version, PROTOCOL_VERSION) {
                    return Err(anyhow::anyhow!(
                        "Incompatible protocol version: {} (client: {})",
                        version,
                        PROTOCOL_VERSION
                    ));
                }
                debug!("Server capabilities: {:?}", capabilities);
                server_capabilities = capabilities;
                // Direct assignment (Option<String> → Option<String>) so that
                // a handshake with an absent `server` field clears any stale
                // value left over from a previous peer in the same process.
                *self.server_version.lock() = server;

                // Handle authentication if server requires it
                if let Some(challenge) = auth {
                    debug!("Server requires {} authentication", challenge.method);

                    let psk = self.config.psk.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("Server requires authentication but no PSK configured")
                    })?;

                    let response = auth::compute_response(&challenge.nonce, psk);
                    let auth_msg = ControlMessage::auth_response(response);
                    writer
                        .write_all(format!("{}\n", auth_msg.serialize()?).as_bytes())
                        .await?;

                    // Read auth result
                    read_bounded_line(&mut reader, &mut line).await?;
                    let auth_result: ControlMessage = ControlMessage::deserialize(line.trim())?;

                    match auth_result {
                        ControlMessage::AuthSuccess => {
                            info!("Authentication successful");
                        }
                        ControlMessage::Error { message } => {
                            return Err(anyhow::anyhow!("Authentication failed: {}", message));
                        }
                        _ => {
                            return Err(anyhow::anyhow!("Unexpected auth response"));
                        }
                    }
                }
            }
            ControlMessage::Error { message } => {
                return Err(anyhow::anyhow!("Server error: {}", message));
            }
            _ => {
                return Err(anyhow::anyhow!("Unexpected response from server"));
            }
        }

        // Check server pause/resume capability
        let supports_pause = server_capabilities
            .as_ref()
            .map(|caps| caps.iter().any(|c| c == "pause_resume"))
            .unwrap_or(false);
        *self.server_supports_pause.lock() = Some(supports_pause);

        // Check server udp_feedback_v1 capability. We spawn the upload-mode
        // feedback receive path only when the server has advertised this —
        // otherwise the server isn't going to send anything for us to recv.
        let server_supports_udp_feedback =
            crate::protocol::capability_advertised(&server_capabilities, "udp_feedback_v1");

        // Zero-copy on the server's sends (download/bidir) rides on the
        // TestStart.zerocopy field; servers predating zerocopy_v1 silently
        // ignore it. Only an explicit -Z warns about that — with the
        // default-on mode the downgrade is invisible and harmless.
        if self.config.zerocopy == ZerocopyMode::Requested
            && matches!(
                self.config.direction,
                Direction::Download | Direction::Bidir
            )
            && !crate::protocol::capability_advertised(&server_capabilities, "zerocopy_v1")
        {
            warn!("Server does not support zero-copy; server-sent traffic will use regular writes");
        }

        // MTU probing needs active server cooperation (ack + echo per
        // probe); an old server would silently count probes as junk
        // data and the search would read as "everything blocked".
        // Refuse up front instead.
        if self.config.mtu_probe
            && !crate::protocol::capability_advertised(&server_capabilities, "mtu_probe_v1")
        {
            return Err(anyhow::anyhow!(
                "Server does not support MTU probing (mtu_probe_v1 capability missing); \
                 upgrade the server to use --probe-mtu"
            ));
        }

        // Validate congestion algorithm before starting test (TCP only)
        if self.config.protocol == Protocol::Tcp
            && let Some(ref algo) = self.config.tcp_congestion
        {
            tcp::validate_congestion(algo).map_err(|e| {
                anyhow::anyhow!("Unsupported congestion control algorithm '{}': {}", algo, e)
            })?;
        }

        // Send test start
        let test_id = Uuid::new_v4().to_string();
        let test_start = ControlMessage::TestStart {
            id: test_id.clone(),
            protocol: self.config.protocol,
            streams: self.config.streams,
            duration_secs: self.config.duration.as_secs() as u32,
            direction: self.config.direction,
            bitrate: self.config.bitrate,
            congestion: self.config.tcp_congestion.clone(),
            mptcp: self.config.mptcp,
            dscp: self.config.dscp,
            window_size: self.config.window_size.map(|w| w as u64),
            zerocopy: self.config.protocol == Protocol::Tcp && self.config.zerocopy.enabled(),
            mtu_probe: self.config.mtu_probe,
            tcp_nodelay: self.config.protocol == Protocol::Tcp && self.config.tcp_nodelay,
        };
        writer
            .write_all(format!("{}\n", test_start.serialize()?).as_bytes())
            .await?;

        // Read test ack
        read_bounded_line(&mut reader, &mut line).await?;
        let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

        let (data_ports, udp_token) = match msg {
            ControlMessage::TestAck {
                data_ports,
                udp_token,
                ..
            } => {
                debug!("Server allocated ports: {:?}", data_ports);
                (data_ports, udp_token)
            }
            ControlMessage::Error { message } => {
                return Err(anyhow::anyhow!("Server error: {}", message));
            }
            _ => {
                return Err(anyhow::anyhow!("Expected test_ack"));
            }
        };

        // Validate single-port mode: empty data_ports requires server capability
        if data_ports.is_empty() && self.config.protocol == Protocol::Tcp {
            let has_single_port = server_capabilities
                .as_ref()
                .map(|caps| caps.iter().any(|c| c == "single_port_tcp"))
                .unwrap_or(false);

            if !has_single_port {
                return Err(anyhow::anyhow!(
                    "Server returned empty data_ports but doesn't support single_port_tcp capability. \
                    The server may be an older version that is incompatible with this client."
                ));
            }
        }

        // Single-port UDP (issue #63): the server signals the mode with an
        // empty port list plus a routing token, and must have advertised
        // the capability. Anything else with empty ports is a broken peer.
        let single_port_udp_token: Option<[u8; udp::UDP_HELLO_TOKEN_LEN]> =
            if self.config.protocol == Protocol::Udp && data_ports.is_empty() {
                let server_supports = crate::protocol::capability_advertised(
                    &server_capabilities,
                    crate::protocol::SINGLE_PORT_UDP_CAPABILITY,
                );
                match udp_token.as_deref().and_then(udp::parse_hello_token) {
                    Some(token) if server_supports => Some(token),
                    _ => {
                        return Err(anyhow::anyhow!(
                            "Server returned no UDP data ports but single-port UDP was not \
                             negotiated (missing {} capability or routing token). The server \
                             may be an incompatible version.",
                            crate::protocol::SINGLE_PORT_UDP_CAPABILITY
                        ));
                    }
                }
            } else {
                None
            };

        // --probe-mtu: the handshake above is shared with throughput
        // tests, but from here probe mode runs its own exchange on the
        // data socket instead of bulk streams, then cancels the test.
        if self.config.mtu_probe {
            return self
                .run_mtu_probe_exchange(
                    &mut reader,
                    &mut writer,
                    &test_id,
                    server_ip,
                    &data_ports,
                    single_port_udp_token,
                )
                .await;
        }

        // Create stats
        let stats = Arc::new(TestStats::new(test_id.clone(), self.config.streams));
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (cancel_request_tx, mut cancel_request_rx) = watch::channel(false);
        // Producer-side monotonic-denominator filter shared by the TCP
        // control udp_progress decode path and the UDP feedback aggregator.
        // Either source can update the consumer-visible loss reading; the
        // filter rejects out-of-order writes from whichever path arrives
        // late.
        let udp_progress_filter = Arc::new(UdpProgressFilter::new());

        // Store cancel senders for external cancellation
        *self.cancel_tx.lock() = Some(cancel_tx.clone());
        *self.cancel_request_tx.lock() = Some(cancel_request_tx);

        // Create pause channels
        let (pause_tx, pause_rx) = watch::channel(false);
        let (pause_request_tx, mut pause_request_rx) = watch::channel(false);
        *self.pause_tx.lock() = Some(pause_tx.clone());
        *self.pause_request_tx.lock() = Some(pause_request_tx);

        // Connect data streams using the resolved IP from control connection
        let stream_handles = match self.config.protocol {
            Protocol::Tcp => {
                self.spawn_tcp_streams(
                    &data_ports,
                    server_ip,
                    stats.clone(),
                    cancel_rx.clone(),
                    &test_id,
                    pause_rx.clone(),
                )
                .await?
            }
            Protocol::Udp => {
                // Spawn the UDP feedback aggregator + 1 Hz consumer task
                // when both peers advertise the capability AND we're in
                // upload mode (download already has authoritative recv-side
                // stats locally; bidir's recv half also has local stats).
                let feedback_aggregator =
                    if server_supports_udp_feedback && self.config.direction == Direction::Upload {
                        let agg: UdpFeedbackAggregator = Arc::new(Mutex::new(vec![
                            crate::protocol::UdpIntervalProgress::default();
                            self.config.streams as usize
                        ]));
                        spawn_udp_feedback_consumer(
                            agg.clone(),
                            udp_progress_filter.clone(),
                            progress_tx.clone(),
                            cancel_rx.clone(),
                        );
                        Some(agg)
                    } else {
                        None
                    };
                self.spawn_udp_streams(
                    &data_ports,
                    server_ip,
                    stats.clone(),
                    cancel_rx.clone(),
                    pause_rx.clone(),
                    feedback_aggregator,
                    single_port_udp_token,
                )
                .await?
            }
            Protocol::Quic => {
                // QUIC uses its own connection model - should not reach here
                return Err(anyhow::anyhow!(
                    "QUIC protocol uses run_quic(), not run_test()"
                ));
            }
        };

        // Read interval updates and final result with timeout
        // For infinite duration, use 1 year timeout (effectively no timeout)
        let timeout_duration = if self.config.duration == Duration::ZERO {
            Duration::from_secs(365 * 24 * 3600) // 1 year
        } else {
            self.config.duration + Duration::from_secs(30)
        };
        let mut deadline = tokio::time::Instant::now() + timeout_duration;
        let local_end_deadline =
            local_stop_deadline(tokio::time::Instant::now(), self.config.duration);
        let mut local_stop_sent = false;

        let mut test_result: anyhow::Result<TestResult> =
            Err(anyhow::anyhow!("Connection closed without result"));

        // Receiver-side interval baselines for download-mode UDP (issue #81):
        // live throughput is derived from our own receive counters, not the
        // server's send-side counts.
        let udp_download =
            self.config.protocol == Protocol::Udp && self.config.direction == Direction::Download;
        // Bidir UDP has the same problem on its download half: the server
        // reports its own send counters as the client's `bytes_received`.
        let udp_bidir =
            self.config.protocol == Protocol::Udp && self.config.direction == Direction::Bidir;
        let mut last_recv_bytes: Vec<u64> = vec![0; stats.streams.len()];
        let mut last_recv_instant = tokio::time::Instant::now();

        loop {
            // Check for external cancel request while waiting for server messages
            tokio::select! {
                read_result = tokio::time::timeout_at(deadline, read_bounded_line(&mut reader, &mut line)) => {
                    match read_result {
                        Ok(Ok(0)) => {
                            // EOF
                            break;
                        }
                        Ok(Ok(_)) => {
                            // Got data, process it below
                        }
                        Ok(Err(e)) => {
                            test_result = Err(e);
                            break;
                        }
                        Err(_) => {
                            // Timeout
                            test_result = Err(anyhow::anyhow!("Timeout waiting for server response"));
                            break;
                        }
                    }
                }
                _ = cancel_request_rx.changed() => {
                    if *cancel_request_rx.borrow() {
                        // Send cancel message to server
                        let cancel_msg = ControlMessage::Cancel {
                            id: test_id.clone(),
                            reason: "User requested cancellation".to_string(),
                        };
                        let serialized = match cancel_msg.serialize() {
                            Ok(s) => s,
                            Err(e) => {
                                test_result = Err(anyhow::anyhow!("Failed to serialize cancel message: {}", e));
                                break;
                            }
                        };
                        let _ = writer
                            .write_all(format!("{}\n", serialized).as_bytes())
                            .await;
                        let _ = cancel_tx.send(true);
                        // Continue loop to receive Cancelled response
                    }
                }
                _ = pause_request_rx.changed() => {
                    let paused = *pause_request_rx.borrow();
                    // Always pause local data loops
                    let _ = pause_tx.send(paused);
                    // Send protocol message only if server supports it
                    if supports_pause {
                        let msg = if paused {
                            ControlMessage::Pause { id: test_id.clone() }
                        } else {
                            ControlMessage::Resume { id: test_id.clone() }
                        };
                        let serialized = match msg.serialize() {
                            Ok(s) => s,
                            Err(e) => {
                                test_result = Err(anyhow::anyhow!("Failed to serialize pause/resume message: {}", e));
                                break;
                            }
                        };
                        if writer
                            .write_all(format!("{}\n", serialized).as_bytes())
                            .await
                            .is_err()
                        {
                            warn!("Failed to send pause/resume to server");
                        }
                    }
                }
                _ = async {
                    if let Some(local_end) = local_end_deadline {
                        tokio::time::sleep_until(local_end).await;
                    }
                }, if !local_stop_sent && local_end_deadline.is_some() => {
                    // Stop local data loops at local test end instead of waiting for Result.
                    // This narrows the race where server-side teardown can trigger RSTs while
                    // client send loops are still writing.
                    local_stop_sent = true;
                    let _ = cancel_tx.send(true);
                    debug!("Local test duration reached; stopping data streams while awaiting final control message");
                }
            }

            if line.is_empty() {
                continue;
            }

            let msg: ControlMessage = match ControlMessage::deserialize(line.trim()) {
                Ok(msg) => msg,
                Err(e) => {
                    test_result = Err(anyhow::anyhow!("Failed to parse server message: {}", e));
                    break;
                }
            };

            match msg {
                ControlMessage::Interval {
                    elapsed_ms,
                    streams,
                    aggregate,
                    ..
                } => {
                    if let Some(ref tx) = progress_tx {
                        // Only overlay local TCP_INFO for sender-side contexts (Upload/Bidir).
                        // In Download mode, server is the sender and has the correct metrics.
                        let is_sender =
                            matches!(self.config.direction, Direction::Upload | Direction::Bidir);
                        let (rtt_us, cwnd, total_retransmits) = if is_sender {
                            if let Some((rtt, retrans, cw)) = stats.poll_local_tcp_info() {
                                (Some(rtt), Some(cw), Some(retrans))
                            } else {
                                (aggregate.rtt_us, aggregate.cwnd, None)
                            }
                        } else {
                            (aggregate.rtt_us, aggregate.cwnd, None)
                        };
                        // Issue #81: in download mode the server is the UDP
                        // sender, so aggregate.bytes counts send_to() calls
                        // the kernel accepted — over a constrained link that
                        // tracks the requested -b rate, not what arrived.
                        // We are the receiver; report our own counters.
                        let (total_bytes, throughput_mbps, streams) = if udp_download {
                            let now = tokio::time::Instant::now();
                            let dt = now.duration_since(last_recv_instant).as_secs_f64();
                            last_recv_instant = now;
                            let (interval_total, deltas) =
                                udp_recv_interval_deltas(&stats, &mut last_recv_bytes);
                            let mbps = if dt > 0.0 {
                                (interval_total as f64 * 8.0) / dt / 1_000_000.0
                            } else {
                                0.0
                            };
                            let streams: Vec<StreamInterval> = streams
                                .into_iter()
                                .map(|mut s| {
                                    if let Some(delta) = deltas.get(s.id as usize) {
                                        s.bytes = *delta;
                                    }
                                    s
                                })
                                .collect();
                            (interval_total, mbps, streams)
                        } else {
                            (aggregate.bytes, aggregate.throughput_mbps, streams)
                        };
                        // Bidir UDP: the download half of the server's split
                        // (`bytes_received` from our view) is its send-side
                        // counter — same inflation as above. Substitute our
                        // own receive deltas; the upload half is already
                        // receiver-truth (the server counts what arrived).
                        // The combined totals are recomputed from the two
                        // corrected halves when the server reported a split.
                        let (bytes_received, throughput_recv_mbps, total_bytes, throughput_mbps) =
                            if udp_bidir {
                                let now = tokio::time::Instant::now();
                                let dt = now.duration_since(last_recv_instant).as_secs_f64();
                                last_recv_instant = now;
                                let (interval_recv, _) =
                                    udp_recv_interval_deltas(&stats, &mut last_recv_bytes);
                                let recv_mbps = if dt > 0.0 {
                                    (interval_recv as f64 * 8.0) / dt / 1_000_000.0
                                } else {
                                    0.0
                                };
                                let (total, total_mbps) =
                                    match (aggregate.bytes_sent, aggregate.throughput_send_mbps) {
                                        (Some(sent), Some(send_mbps)) => {
                                            (sent + interval_recv, send_mbps + recv_mbps)
                                        }
                                        _ => (total_bytes, throughput_mbps),
                                    };
                                (Some(interval_recv), Some(recv_mbps), total, total_mbps)
                            } else {
                                (
                                    aggregate.bytes_received,
                                    aggregate.throughput_recv_mbps,
                                    total_bytes,
                                    throughput_mbps,
                                )
                            };
                        // Apply the producer-side monotonic-denominator
                        // filter so a delayed TCP `Interval` cannot clobber
                        // a fresher reading we already saw via UDP feedback.
                        // Both paths feed the same filter; older-or-same
                        // counts are silently rejected (None -> consumers
                        // preserve their previous loss state).
                        // In download mode our own receive trackers are the
                        // authoritative source; the server has no
                        // receiver-side counters to report there.
                        let filtered_udp_progress = if udp_download {
                            udp_progress_filter.apply(udp_local_progress(&stats))
                        } else {
                            aggregate
                                .udp_progress
                                .and_then(|p| udp_progress_filter.apply(p))
                        };
                        let _ = tx
                            .send(TestProgress {
                                elapsed_ms,
                                total_bytes,
                                throughput_mbps,
                                rtt_us,
                                cwnd,
                                total_retransmits,
                                streams,
                                bytes_sent: aggregate.bytes_sent,
                                bytes_received,
                                throughput_send_mbps: aggregate.throughput_send_mbps,
                                throughput_recv_mbps,
                                udp_progress: filtered_udp_progress,
                                udp_feedback_only: false,
                            })
                            .await;
                    }
                }
                ControlMessage::Result(mut result) => {
                    // Server (receiver) can't report sender-side TCP_INFO metrics
                    // (retransmits, RTT, cwnd are all 0 on receiver side).
                    // Overlay client-side snapshots when we're the sender.
                    let is_sender =
                        matches!(self.config.direction, Direction::Upload | Direction::Bidir);
                    if is_sender {
                        for (i, stream_result) in result.streams.iter_mut().enumerate() {
                            if let Some(info) =
                                stats.streams.get(i).and_then(|s| s.final_tcp_info())
                            {
                                stream_result.retransmits = Some(info.retransmits);
                            }
                        }
                        if let Some(info) = stats.final_local_tcp_info() {
                            result.tcp_info = Some(info);
                        }
                    }
                    test_result = Ok(result);
                    break;
                }
                ControlMessage::Error { message } => {
                    test_result = Err(anyhow::anyhow!("Server error: {}", message));
                    break;
                }
                ControlMessage::Cancelled { .. } => {
                    // Server acknowledged cancel — it will send Result next.
                    // Tighten deadline to wait briefly for the final result.
                    deadline = tokio::time::Instant::now() + Duration::from_secs(3);
                    line.clear();
                    continue;
                }
                _ => {
                    debug!("Unexpected message: {:?}", msg);
                }
            }
        }

        // Signal stream tasks to stop and wait for them to finish cleanly
        let _ = cancel_tx.send(true);
        let mut stream_handles = stream_handles;
        let join_timeout = stream_join_timeout(self.config.streams);
        match tokio::time::timeout(
            join_timeout,
            futures::future::join_all(stream_handles.iter_mut()),
        )
        .await
        {
            Ok(results) => {
                for result in results {
                    if let Err(e) = result
                        && e.is_panic()
                    {
                        error!("Data stream task panicked: {:?}", e);
                    }
                }
            }
            Err(_) => {
                warn!(
                    "Timed out waiting {:?} for {} data streams to stop; aborting remaining tasks",
                    join_timeout, self.config.streams
                );
                for handle in &stream_handles {
                    handle.abort();
                }
            }
        }

        // Issue #81: the server's Result carries sender-side byte counts in
        // download mode; overlay receiver-side truth. After the join above so
        // the receive tasks have recorded their final UdpStats (loss/jitter).
        if udp_download && let Ok(ref mut result) = test_result {
            overlay_udp_download_result(result, &stats);
        }
        // Bidir UDP: same receiver-truth substitution, scoped to the
        // download half of the split (upload is already receiver-truth).
        if udp_bidir && let Ok(ref mut result) = test_result {
            overlay_udp_bidir_result(result, &stats);
        }

        test_result
    }

    async fn spawn_tcp_streams(
        &self,
        data_ports: &[u16],
        server_addr: SocketAddr,
        stats: Arc<TestStats>,
        cancel: watch::Receiver<bool>,
        test_id: &str,
        pause: watch::Receiver<bool>,
    ) -> anyhow::Result<Vec<tokio::task::JoinHandle<()>>> {
        // Single-port mode: connect all streams to control port with DataHello
        let single_port_mode = data_ports.is_empty();
        let control_port = self.config.port;
        let test_id = test_id.to_string();

        if !single_port_mode && data_ports.len() != self.config.streams as usize {
            return Err(anyhow::anyhow!(
                "Server returned {} data ports but {} streams were requested",
                data_ports.len(),
                self.config.streams
            ));
        }

        let per_stream_bitrate = self.config.bitrate.map(|b| {
            if b == 0 {
                0
            } else {
                (b / self.config.streams as u64).max(1)
            }
        });

        let base_bind_addr = sequential_bind_base(
            self.config.bind_addr,
            self.config.sequential_ports,
            self.config.streams as usize,
        )?;

        let mut handles = Vec::new();
        let handshake_limiter = if single_port_mode {
            Some(Arc::new(Semaphore::new(single_port_handshake_parallelism(
                self.config.streams,
            ))))
        } else {
            None
        };

        #[allow(clippy::needless_range_loop)] // Intentional: single-port mode has empty data_ports
        for i in 0..self.config.streams as usize {
            let port = if single_port_mode {
                control_port
            } else {
                data_ports[i]
            };
            let addr = SocketAddr::new(server_addr.ip(), port);
            let stream_stats = stats.streams[i].clone();
            let cancel = cancel.clone();
            let pause = pause.clone();
            let direction = self.config.direction;
            let duration = self.config.duration;
            let bind_addr = stream_bind_addr(base_bind_addr, self.config.sequential_ports, i);
            let mptcp = self.config.mptcp;
            let dscp = self.config.dscp;
            let test_id = test_id.clone();
            let stream_index = i as u16;
            let handshake_limiter = handshake_limiter.clone();

            let config = TcpConfig {
                nodelay: self.config.tcp_nodelay,
                window_size: self.config.window_size,
                congestion: self.config.tcp_congestion.clone(),
                random_payload: self.config.random_payload,
                zerocopy: self.config.zerocopy.enabled(),
                ..Default::default()
            };

            handles.push(tokio::spawn(async move {
                // In single-port mode, limit concurrent connect+DataHello handshakes
                // to avoid control-port burst loss under high stream counts.
                let handshake_permit = if let Some(limiter) = handshake_limiter {
                    match limiter.acquire_owned().await {
                        Ok(permit) => Some(permit),
                        Err(_) => {
                            error!("Handshake limiter closed unexpectedly");
                            return;
                        }
                    }
                } else {
                    None
                };

                let local_bind = bind_addr.map(|local| net::match_bind_family(local, addr));
                match net::connect_tcp_with_bind(addr, local_bind, mptcp).await {
                    Ok(mut stream) => {
                        debug!("Connected to data port {}", port);

                        // Set DSCP/TOS marking if requested
                        if let Some(tos) = dscp
                            && let Err(e) = net::set_tos_on_tcp(&stream, tos)
                        {
                            warn!("Failed to set IP_TOS: {}", e);
                        }

                        // Single-port mode: send DataHello to identify stream
                        if single_port_mode {
                            let hello = ControlMessage::DataHello {
                                test_id,
                                stream_index,
                            };
                            let serialized = match hello.serialize() {
                                Ok(s) => s,
                                Err(e) => {
                                    error!("Failed to serialize DataHello: {}", e);
                                    return;
                                }
                            };
                            if let Err(e) = stream
                                .write_all(format!("{}\n", serialized).as_bytes())
                                .await
                            {
                                error!("Failed to send DataHello: {}", e);
                                return;
                            }
                        }
                        // Handshake is complete; release permit before data transfer.
                        drop(handshake_permit);

                        // Store fd for local TCP_INFO polling (sender-side only)
                        #[cfg(unix)]
                        if matches!(direction, Direction::Upload | Direction::Bidir) {
                            use std::os::unix::io::AsRawFd;
                            stream_stats.set_tcp_info_fd(stream.as_raw_fd());
                        }

                        match direction {
                            Direction::Upload => {
                                // Client sends data
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
                                    Ok(Some(info)) => stream_stats.set_final_tcp_info(info),
                                    Ok(None) => {}
                                    Err(e) => error!("Send error: {}", e),
                                }
                            }
                            Direction::Download => {
                                // Client receives data
                                if let Err(e) =
                                    tcp::receive_data(stream, stream_stats.clone(), cancel, config)
                                        .await
                                {
                                    error!("Receive error: {}", e);
                                }
                            }
                            Direction::Bidir => {
                                // Configure socket BEFORE splitting (nodelay, window, buffers)
                                if let Err(e) = tcp::configure_stream(&stream, &config) {
                                    error!("Failed to configure TCP socket: {}", e);
                                    stream_stats.clear_tcp_info_fd();
                                    return;
                                }

                                // Split socket for concurrent send/receive
                                let (read_half, write_half) = stream.into_split();

                                let send_stats = stream_stats.clone();
                                let recv_stats = stream_stats.clone();
                                let send_cancel = cancel.clone();
                                let recv_cancel = cancel;
                                let send_pause = pause;

                                let send_config = TcpConfig {
                                    buffer_size: config.buffer_size,
                                    nodelay: config.nodelay,
                                    window_size: config.window_size,
                                    congestion: config.congestion.clone(),
                                    random_payload: config.random_payload,
                                    zerocopy: config.zerocopy,
                                };
                                let recv_config = config;

                                let (send_result, recv_result) = tokio::join!(
                                    tcp::send_data_half(
                                        write_half,
                                        send_stats,
                                        duration,
                                        send_config,
                                        send_cancel,
                                        per_stream_bitrate,
                                        send_pause,
                                    ),
                                    tcp::receive_data_half(
                                        read_half,
                                        recv_stats,
                                        recv_cancel,
                                        recv_config,
                                    )
                                );

                                if let Err(e) = &send_result {
                                    error!("Bidir send error: {}", e);
                                }
                                if let Err(e) = &recv_result {
                                    error!("Bidir receive error: {}", e);
                                }

                                // Use the clamp-time TCP_INFO snapshot returned by
                                // send_data_half: re-reading after reunite would see a later,
                                // larger bytes_acked that could exceed the clamped bytes_sent.
                                if let (Ok((write_half, send_tcp_info)), Ok(read_half)) =
                                    (send_result, recv_result)
                                {
                                    if let Some(info) = send_tcp_info {
                                        stream_stats.set_final_tcp_info(info);
                                    }
                                    // Reunite to keep the socket lifetime symmetric; dropped next.
                                    let _ = read_half.reunite(write_half);
                                }
                            }
                        }
                        stream_stats.clear_tcp_info_fd();
                    }
                    Err(e) => {
                        error!("Failed to connect to data port {}: {}", port, e);
                    }
                }
            }));
        }

        // Give streams time to connect
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(handles)
    }

    /// The `--probe-mtu` data exchange (issue #64): walk payload sizes
    /// against the server's probe responder on the (single) UDP data
    /// socket, then cancel the test on the control channel and wait for
    /// the server's Result so the connection closes out the normal way.
    /// The probe report rides home on the result's `mtu_probe` field.
    #[allow(clippy::too_many_arguments)]
    async fn run_mtu_probe_exchange(
        &self,
        reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
        writer: &mut tokio::net::tcp::OwnedWriteHalf,
        test_id: &str,
        server_ip: SocketAddr,
        data_ports: &[u16],
        single_port_token: Option<[u8; udp::UDP_HELLO_TOKEN_LEN]>,
    ) -> anyhow::Result<TestResult> {
        // Single-port mode probes the main port after a hello/ack
        // handshake; legacy mode uses the allocated ephemeral port.
        let port = match single_port_token {
            Some(_) => self.config.port,
            None => data_ports
                .first()
                .copied()
                .ok_or_else(|| anyhow::anyhow!("Server allocated no UDP port for the MTU probe"))?,
        };
        let server_addr = SocketAddr::new(server_ip.ip(), port);

        let socket = net::create_udp_socket_for_remote(server_addr).await?;
        socket.connect(server_addr).await?;
        let ipv6 = socket.local_addr().map(|a| a.is_ipv6()).unwrap_or(false);
        // Without DF the kernel fragments oversized probes and every
        // size "passes" — the forward results would be meaningless.
        // Reverse stays valid (the server sets DF on its echoes), so
        // degrade with a warning rather than refuse.
        if let Err(e) = net::set_dont_fragment(&socket, ipv6) {
            warn!(
                "Could not set don't-fragment on probe socket: {} \
                 (client-to-server results may overstate the path)",
                e
            );
        }

        // Hello/ack first so the server's connected socket and probe
        // responder exist before the first probe flies (issue #63).
        if let Some(token) = single_port_token {
            udp::single_port_udp_handshake(&socket, &token, 0).await?;
        }

        let report = crate::probe::run_probe(&socket, ipv6).await?;

        // The probe converged well before the test duration; cancel so
        // the server sends its Result now instead of at the deadline.
        let cancel_msg = ControlMessage::Cancel {
            id: test_id.to_string(),
            reason: "MTU probe complete".to_string(),
        };
        writer
            .write_all(format!("{}\n", cancel_msg.serialize()?).as_bytes())
            .await?;

        // Drain control messages (Intervals that accumulated during the
        // probe, the Cancelled ack) until the Result lands. If the test
        // deadline beat our cancel, the Result may already be queued.
        let mut line = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut result = loop {
            line.clear();
            let n = tokio::time::timeout_at(deadline, read_bounded_line(reader, &mut line))
                .await
                .map_err(|_| anyhow::anyhow!("Timeout waiting for probe result"))??;
            if n == 0 {
                return Err(anyhow::anyhow!("Connection closed before probe result"));
            }
            match ControlMessage::deserialize(line.trim())? {
                ControlMessage::Result(result) => break result,
                ControlMessage::Error { message } => {
                    return Err(anyhow::anyhow!("Server error: {}", message));
                }
                _ => continue,
            }
        };

        result.mtu_probe = Some(report);
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn_udp_streams(
        &self,
        data_ports: &[u16],
        server_addr: SocketAddr,
        stats: Arc<TestStats>,
        cancel: watch::Receiver<bool>,
        pause: watch::Receiver<bool>,
        feedback_aggregator: Option<UdpFeedbackAggregator>,
        single_port_token: Option<[u8; udp::UDP_HELLO_TOKEN_LEN]>,
    ) -> anyhow::Result<Vec<tokio::task::JoinHandle<()>>> {
        let single_port = single_port_token.is_some();
        let stream_count = self.config.streams as usize;
        let base_bind_addr = sequential_bind_base(
            self.config.bind_addr,
            self.config.sequential_ports,
            stream_count,
        )?;

        let bitrate = self.config.bitrate.unwrap_or(1_000_000_000); // 1 Gbps default

        // Calculate per-stream bitrate, clamping to at least 1 bps to prevent
        // integer division underflow (e.g., 100 bps / 8 streams = 12 bps, not 0)
        // Only bitrate=0 means unlimited (explicit -b 0)
        let stream_bitrate = if bitrate == 0 {
            0 // Unlimited mode (explicit -b 0)
        } else {
            (bitrate / self.config.streams as u64).max(1)
        };

        if !single_port && data_ports.len() != stream_count {
            return Err(anyhow::anyhow!(
                "Server returned {} data ports but {} streams were requested",
                data_ports.len(),
                self.config.streams
            ));
        }

        // Single-port mode (issue #63): set up every socket and complete
        // every hello/ack handshake BEFORE any data task starts. The ack
        // proves the server's connected same-port socket exists, which is
        // what keeps line-rate data off the shared QUIC socket; a missing
        // ack fails the whole test loudly instead of degrading quietly.
        if let Some(token) = single_port_token {
            let server_port = SocketAddr::new(server_addr.ip(), self.config.port);
            let mut sockets = Vec::with_capacity(stream_count);
            for i in 0..stream_count {
                let bind_addr = stream_bind_addr(base_bind_addr, self.config.sequential_ports, i);
                let socket = self
                    .create_udp_stream_socket(bind_addr, server_port)
                    .await?;
                sockets.push(socket);
            }
            futures::future::try_join_all(
                sockets
                    .iter()
                    .enumerate()
                    .map(|(i, socket)| udp::single_port_udp_handshake(socket, &token, i as u16)),
            )
            .await?;

            let mut handles = Vec::with_capacity(stream_count);
            for (i, socket) in sockets.into_iter().enumerate() {
                handles.push(tokio::spawn(run_udp_stream_io(
                    socket,
                    self.config.direction,
                    self.config.duration,
                    stream_bitrate,
                    self.config.random_payload,
                    stats.streams[i].clone(),
                    stats.clone(),
                    cancel.clone(),
                    pause.clone(),
                    feedback_aggregator.clone(),
                    i,
                    true,
                )));
            }
            return Ok(handles);
        }

        let mut handles = Vec::new();

        for (i, &port) in data_ports.iter().enumerate() {
            let server_port = SocketAddr::new(server_addr.ip(), port);
            let stream_stats = stats.streams[i].clone();
            let test_stats = stats.clone();
            let cancel = cancel.clone();
            let pause = pause.clone();
            let direction = self.config.direction;
            let duration = self.config.duration;
            let random_payload = self.config.random_payload;
            let bind_addr = stream_bind_addr(base_bind_addr, self.config.sequential_ports, i);
            let dscp = self.config.dscp;
            let window_size = self.config.window_size;
            let feedback_aggregator = feedback_aggregator.clone();
            let stream_index = i;

            handles.push(tokio::spawn(async move {
                // Create UDP socket matching the server's address family for cross-platform compatibility.
                // macOS dual-stack sockets behave differently than Linux, so we match the server's family.
                let socket = if let Some(local) = bind_addr {
                    let local = net::match_bind_family(local, server_port);
                    match net::create_udp_socket_bound(local).await {
                        Ok(s) => Arc::new(s),
                        Err(e) => {
                            error!("Failed to bind UDP socket to {}: {}", local, e);
                            return;
                        }
                    }
                } else {
                    match net::create_udp_socket_for_remote(server_port).await {
                        Ok(s) => Arc::new(s),
                        Err(e) => {
                            error!("Failed to create UDP socket: {}", e);
                            return;
                        }
                    }
                };

                if let Err(e) = socket.connect(server_port).await {
                    error!("Failed to connect UDP socket: {}", e);
                    return;
                }

                debug!("UDP connected to {}", server_port);

                // Set DSCP/TOS marking if requested
                if let Some(tos) = dscp
                    && let Err(e) = net::set_tos_on_udp(&socket, tos)
                {
                    warn!("Failed to set IP_TOS on UDP socket: {}", e);
                }

                // Apply -w / --window to the UDP socket. Symmetric with the
                // server side; primarily affects SO_SNDBUF on the sender so
                // a fast client doesn't block on a small kernel send queue
                // when the network can absorb the traffic. Set both buffers
                // to keep the bidir path consistent.
                if let Some(size) = window_size
                    && let Err(e) = net::set_udp_buffer_size(&socket, size)
                {
                    warn!("Failed to set UDP buffer size to {}: {}", size, e);
                }

                run_udp_stream_io(
                    socket,
                    direction,
                    duration,
                    stream_bitrate,
                    random_payload,
                    stream_stats,
                    test_stats,
                    cancel,
                    pause,
                    feedback_aggregator,
                    stream_index,
                    false,
                )
                .await;
            }));
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(handles)
    }

    /// Create, connect, and configure one client-side UDP data socket
    /// (single-port mode, where socket setup happens before the spawned
    /// task so handshake failures can fail the test loudly).
    async fn create_udp_stream_socket(
        &self,
        bind_addr: Option<SocketAddr>,
        server_port: SocketAddr,
    ) -> anyhow::Result<Arc<tokio::net::UdpSocket>> {
        let socket = if let Some(local) = bind_addr {
            let local = net::match_bind_family(local, server_port);
            net::create_udp_socket_bound(local)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to bind UDP socket to {}: {}", local, e))?
        } else {
            net::create_udp_socket_for_remote(server_port)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create UDP socket: {}", e))?
        };
        socket
            .connect(server_port)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect UDP socket: {}", e))?;
        debug!("UDP connected to {}", server_port);

        if let Some(tos) = self.config.dscp
            && let Err(e) = net::set_tos_on_udp(&socket, tos)
        {
            warn!("Failed to set IP_TOS on UDP socket: {}", e);
        }
        // See spawn_udp_streams for why both buffers are set.
        if let Some(size) = self.config.window_size
            && let Err(e) = net::set_udp_buffer_size(&socket, size)
        {
            warn!("Failed to set UDP buffer size to {}: {}", size, e);
        }
        Ok(Arc::new(socket))
    }

    /// Run a test using QUIC transport
    async fn run_quic(
        &self,
        progress_tx: Option<mpsc::Sender<TestProgress>>,
    ) -> anyhow::Result<TestResult> {
        use tokio::io::BufReader;

        // Reset pause state from any previous run
        *self.server_supports_pause.lock() = None;
        *self.pause_tx.lock() = None;
        *self.pause_request_tx.lock() = None;
        // Reset server version too — a reused Client connecting to a server
        // that doesn't advertise `server` in its Hello must not carry the
        // prior peer's string forward into the TUI display.
        *self.server_version.lock() = None;

        // Resolve target address first, then create endpoint with matching address family
        let addr = net::resolve_host(
            &self.config.host,
            self.config.port,
            self.config.address_family,
        )?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No address found for {}", self.config.host))?;

        // Create QUIC endpoint with address family matching the target
        let endpoint = quic::create_client_endpoint(addr, self.config.bind_addr)?;

        info!("Connecting via QUIC to {}...", addr);
        let connection = match self.config.connect_timeout {
            Some(limit) => tokio::time::timeout(limit, quic::connect(&endpoint, addr))
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "Timed out connecting to {} after {:?} (--connect-timeout)",
                        addr,
                        limit
                    )
                })??,
            None => quic::connect(&endpoint, addr).await?,
        };

        // Open control stream (bidirectional)
        let (mut ctrl_send, ctrl_recv) = connection.open_bi().await?;
        let mut ctrl_reader = BufReader::new(ctrl_recv);
        let mut line = String::new();

        // Send client hello
        let hello = ControlMessage::client_hello();
        ctrl_send
            .write_all(format!("{}\n", hello.serialize()?).as_bytes())
            .await?;

        // Read server hello (bounded to prevent DoS)
        read_bounded_line(&mut ctrl_reader, &mut line).await?;
        let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

        let server_capabilities;
        match msg {
            ControlMessage::Hello {
                version,
                capabilities,
                auth,
                server,
                ..
            } => {
                if !versions_compatible(&version, PROTOCOL_VERSION) {
                    return Err(anyhow::anyhow!(
                        "Incompatible protocol version: {} (client: {})",
                        version,
                        PROTOCOL_VERSION
                    ));
                }
                debug!("Server capabilities: {:?}", capabilities);
                server_capabilities = capabilities;
                // Direct assignment (Option<String> → Option<String>) so that
                // a handshake with an absent `server` field clears any stale
                // value left over from a previous peer in the same process.
                *self.server_version.lock() = server;

                // Handle authentication if server requires it
                if let Some(challenge) = auth {
                    debug!("Server requires {} authentication", challenge.method);

                    let psk = self.config.psk.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("Server requires authentication but no PSK configured")
                    })?;

                    let response = auth::compute_response(&challenge.nonce, psk);
                    let auth_msg = ControlMessage::auth_response(response);
                    ctrl_send
                        .write_all(format!("{}\n", auth_msg.serialize()?).as_bytes())
                        .await?;

                    // Read auth result
                    read_bounded_line(&mut ctrl_reader, &mut line).await?;
                    let auth_result: ControlMessage = ControlMessage::deserialize(line.trim())?;

                    match auth_result {
                        ControlMessage::AuthSuccess => {
                            info!("Authentication successful");
                        }
                        ControlMessage::Error { message } => {
                            return Err(anyhow::anyhow!("Authentication failed: {}", message));
                        }
                        _ => {
                            return Err(anyhow::anyhow!("Unexpected auth response"));
                        }
                    }
                }
            }
            ControlMessage::Error { message } => {
                return Err(anyhow::anyhow!("Server error: {}", message));
            }
            _ => {
                return Err(anyhow::anyhow!("Unexpected response from server"));
            }
        }

        // Check server pause/resume capability
        let supports_pause = server_capabilities
            .as_ref()
            .map(|caps| caps.iter().any(|c| c == "pause_resume"))
            .unwrap_or(false);
        *self.server_supports_pause.lock() = Some(supports_pause);

        // Send test start
        let test_id = Uuid::new_v4().to_string();
        let test_start = ControlMessage::TestStart {
            id: test_id.clone(),
            protocol: Protocol::Quic,
            streams: self.config.streams,
            duration_secs: self.config.duration.as_secs() as u32,
            direction: self.config.direction,
            bitrate: self.config.bitrate,
            congestion: None,
            mptcp: false,
            dscp: None,         // QUIC manages its own TOS via the transport layer
            window_size: None,  // QUIC flow control is handled at the transport layer
            zerocopy: false,    // sendfile is incompatible with QUIC's userspace encryption
            mtu_probe: false,   // probe mode is UDP-only
            tcp_nodelay: false, // QUIC manages its own transport; Nagle is a TCP concept
        };
        ctrl_send
            .write_all(format!("{}\n", test_start.serialize()?).as_bytes())
            .await?;

        // Read test ack
        read_bounded_line(&mut ctrl_reader, &mut line).await?;
        let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

        match msg {
            ControlMessage::TestAck { .. } => {
                debug!("Server acknowledged QUIC test");
            }
            ControlMessage::Error { message } => {
                return Err(anyhow::anyhow!("Server error: {}", message));
            }
            _ => {
                return Err(anyhow::anyhow!("Expected test_ack"));
            }
        }

        // Create stats and cancel channels
        let stats = Arc::new(TestStats::new(test_id.clone(), self.config.streams));
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (cancel_request_tx, mut cancel_request_rx) = watch::channel(false);

        *self.cancel_tx.lock() = Some(cancel_tx.clone());
        *self.cancel_request_tx.lock() = Some(cancel_request_tx);

        // Create pause channels
        let (pause_tx, pause_rx) = watch::channel(false);
        let (pause_request_tx, mut pause_request_rx) = watch::channel(false);
        *self.pause_tx.lock() = Some(pause_tx.clone());
        *self.pause_request_tx.lock() = Some(pause_request_tx);

        // Spawn data streams based on direction
        for i in 0..self.config.streams {
            let stream_stats = stats.streams[i as usize].clone();
            let cancel = cancel_rx.clone();
            let pause = pause_rx.clone();
            let duration = self.config.duration;
            let direction = self.config.direction;
            let conn = connection.clone();

            tokio::spawn(async move {
                match direction {
                    Direction::Upload => {
                        // Open unidirectional stream for sending
                        match conn.open_uni().await {
                            Ok(send) => {
                                if let Err(e) = quic::send_quic_data(
                                    send,
                                    stream_stats,
                                    duration,
                                    cancel,
                                    pause,
                                )
                                .await
                                {
                                    error!("QUIC send error: {}", e);
                                }
                            }
                            Err(e) => error!("Failed to open send stream: {}", e),
                        }
                    }
                    Direction::Download => {
                        // Accept unidirectional stream from server
                        match conn.accept_uni().await {
                            Ok(recv) => {
                                if let Err(e) =
                                    quic::receive_quic_data(recv, stream_stats, cancel).await
                                {
                                    error!("QUIC receive error: {}", e);
                                }
                            }
                            Err(e) => error!("Failed to accept receive stream: {}", e),
                        }
                    }
                    Direction::Bidir => {
                        // Open bidirectional stream for both directions
                        match conn.open_bi().await {
                            Ok((send, recv)) => {
                                let send_stats = stream_stats.clone();
                                let recv_stats = stream_stats;
                                let send_cancel = cancel.clone();
                                let recv_cancel = cancel;
                                let send_pause = pause;

                                let send_handle = tokio::spawn(async move {
                                    if let Err(e) = quic::send_quic_data(
                                        send,
                                        send_stats,
                                        duration,
                                        send_cancel,
                                        send_pause,
                                    )
                                    .await
                                    {
                                        error!("QUIC bidir send error: {}", e);
                                    }
                                });

                                let recv_handle = tokio::spawn(async move {
                                    if let Err(e) =
                                        quic::receive_quic_data(recv, recv_stats, recv_cancel).await
                                    {
                                        error!("QUIC bidir receive error: {}", e);
                                    }
                                });

                                let _ = tokio::join!(send_handle, recv_handle);
                            }
                            Err(e) => error!("Failed to open bidir stream: {}", e),
                        }
                    }
                }
            });
        }

        // Read interval updates and final result
        // For infinite duration, use 1 year timeout (effectively no timeout)
        let timeout_duration = if self.config.duration == Duration::ZERO {
            Duration::from_secs(365 * 24 * 3600) // 1 year
        } else {
            self.config.duration + Duration::from_secs(30)
        };
        let mut deadline = tokio::time::Instant::now() + timeout_duration;

        loop {
            tokio::select! {
                read_result = tokio::time::timeout_at(deadline, read_bounded_line(&mut ctrl_reader, &mut line)) => {
                    match read_result {
                        Ok(Ok(0)) => break,
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            let _ = cancel_tx.send(true);
                            return Err(e);
                        }
                        Err(_) => {
                            let _ = cancel_tx.send(true);
                            return Err(anyhow::anyhow!("Timeout waiting for server response"));
                        }
                    }
                }
                _ = cancel_request_rx.changed() => {
                    if *cancel_request_rx.borrow() {
                        let cancel_msg = ControlMessage::Cancel {
                            id: test_id.clone(),
                            reason: "User requested cancellation".to_string(),
                        };
                        let _ = ctrl_send.write_all(format!("{}\n", cancel_msg.serialize()?).as_bytes()).await;
                        let _ = cancel_tx.send(true);
                    }
                }
                _ = pause_request_rx.changed() => {
                    let paused = *pause_request_rx.borrow();
                    let _ = pause_tx.send(paused);
                    if supports_pause {
                        let msg = if paused {
                            ControlMessage::Pause { id: test_id.clone() }
                        } else {
                            ControlMessage::Resume { id: test_id.clone() }
                        };
                        if ctrl_send.write_all(format!("{}\n", msg.serialize()?).as_bytes()).await.is_err() {
                            warn!("Failed to send pause/resume to server");
                        }
                    }
                }
            }

            if line.is_empty() {
                continue;
            }

            let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

            match msg {
                ControlMessage::Interval {
                    elapsed_ms,
                    streams,
                    aggregate,
                    ..
                } => {
                    if let Some(ref tx) = progress_tx {
                        let _ = tx
                            .send(TestProgress {
                                elapsed_ms,
                                total_bytes: aggregate.bytes,
                                throughput_mbps: aggregate.throughput_mbps,
                                rtt_us: aggregate.rtt_us,
                                cwnd: aggregate.cwnd,
                                total_retransmits: None,
                                streams,
                                bytes_sent: aggregate.bytes_sent,
                                bytes_received: aggregate.bytes_received,
                                throughput_send_mbps: aggregate.throughput_send_mbps,
                                throughput_recv_mbps: aggregate.throughput_recv_mbps,
                                udp_progress: aggregate.udp_progress,
                                udp_feedback_only: false,
                            })
                            .await;
                    }
                }
                ControlMessage::Result(result) => {
                    let _ = cancel_tx.send(true);
                    endpoint.close(0u32.into(), b"done");
                    return Ok(result);
                }
                ControlMessage::Error { message } => {
                    let _ = cancel_tx.send(true);
                    return Err(anyhow::anyhow!("Server error: {}", message));
                }
                ControlMessage::Cancelled { .. } => {
                    // Server acknowledged cancel — it will send Result next.
                    // Tighten deadline to wait briefly for the final result.
                    let _ = cancel_tx.send(true);
                    deadline = tokio::time::Instant::now() + Duration::from_secs(3);
                    line.clear();
                    continue;
                }
                _ => {
                    debug!("Unexpected message: {:?}", msg);
                }
            }
        }

        Err(anyhow::anyhow!("Connection closed without result"))
    }

    /// Cancel a running test.
    ///
    /// This sends a Cancel message to the server and signals local data stream
    /// handlers to stop. The server will respond with a Cancelled message.
    pub fn cancel(&self) -> anyhow::Result<()> {
        // Signal the control loop to send a Cancel message to server
        if let Some(tx) = self.cancel_request_tx.lock().as_ref() {
            let _ = tx.send(true);
            Ok(())
        } else {
            Err(anyhow::anyhow!("No test is currently running"))
        }
    }

    /// Toggle pause on a running test.
    /// Returns Applied if pause was toggled, Unsupported if server lacks capability,
    /// or NotReady if capabilities aren't negotiated yet or no active channel exists.
    pub fn pause(&self) -> PauseResult {
        match *self.server_supports_pause.lock() {
            None => return PauseResult::NotReady,
            Some(false) => return PauseResult::Unsupported,
            Some(true) => {}
        }
        if let Some(tx) = self.pause_request_tx.lock().as_ref() {
            let current = *tx.borrow();
            if tx.send(!current).is_ok() {
                return PauseResult::Applied;
            }
        }
        PauseResult::NotReady
    }
}

/// Per-direction client-side UDP stream IO on an already-connected,
/// already-configured socket. Shared by the legacy per-port path and the
/// single-port path (issue #63); `single_port` only suppresses the
/// legacy 1-byte address-discovery hellos in download mode — the
/// token-bearing handshake has already told the server where we are.
#[allow(clippy::too_many_arguments)]
async fn run_udp_stream_io(
    socket: Arc<tokio::net::UdpSocket>,
    direction: Direction,
    duration: Duration,
    stream_bitrate: u64,
    random_payload: bool,
    stream_stats: Arc<crate::stats::StreamStats>,
    test_stats: Arc<TestStats>,
    cancel: watch::Receiver<bool>,
    pause: watch::Receiver<bool>,
    feedback_aggregator: Option<UdpFeedbackAggregator>,
    stream_index: usize,
    single_port: bool,
) {
    match direction {
        Direction::Upload => {
            if let Some(aggregator) = feedback_aggregator {
                // Server advertised udp_feedback_v1: spawn the
                // feedback recv task alongside the sender so
                // live UDP loss can be reported back without
                // riding the TCP control channel that competes
                // with the saturated upload.
                let send_socket = socket.clone();
                let recv_socket = socket;
                let send_cancel = cancel.clone();
                let recv_cancel = cancel;
                let send_handle = tokio::spawn(async move {
                    if let Err(e) = udp::send_udp_paced(
                        send_socket,
                        None,
                        stream_bitrate,
                        duration,
                        stream_stats,
                        send_cancel,
                        pause,
                        random_payload,
                    )
                    .await
                    {
                        error!("UDP send error: {}", e);
                    }
                });
                let recv_handle = tokio::spawn(async move {
                    if let Err(e) = udp::receive_udp_feedback_only(
                        recv_socket,
                        aggregator,
                        stream_index,
                        recv_cancel,
                    )
                    .await
                    {
                        debug!("UDP feedback recv error on stream {}: {}", stream_index, e);
                    }
                });
                let _ = tokio::join!(send_handle, recv_handle);
            } else if let Err(e) = udp::send_udp_paced(
                socket,
                None, // Connected socket, no target needed
                stream_bitrate,
                duration,
                stream_stats,
                cancel,
                pause,
                random_payload,
            )
            .await
            {
                error!("UDP send error: {}", e);
            }
        }
        Direction::Download => {
            // Legacy mode: send hello packets concurrently with receiving
            // so the server learns our address even if first packets are
            // missed. Single-port mode already did a token handshake.
            let hello_handle = if single_port {
                None
            } else {
                let hello_socket = socket.clone();
                let hello_cancel = cancel.clone();
                Some(tokio::spawn(async move {
                    // Send hello packets every 100ms until cancelled or 5 seconds
                    for _ in 0..50 {
                        if *hello_cancel.borrow() {
                            break;
                        }
                        let _ = hello_socket.send(&[0u8; 1]).await;
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }))
            };

            // Keep the receiver-side UdpStats (loss/jitter):
            // run_test overlays them onto the server's
            // sender-side Result (issue #81).
            match udp::receive_udp(socket, stream_stats, cancel, pause, false, None).await {
                Ok((udp_stats, _bytes)) => test_stats.add_udp_stats(udp_stats),
                Err(e) => error!("UDP receive error: {}", e),
            }
            if let Some(handle) = hello_handle {
                handle.abort();
            }
        }
        Direction::Bidir => {
            // UDP can send/receive concurrently on same socket
            let send_socket = socket.clone();
            let recv_socket = socket;
            let send_stats = stream_stats.clone();
            let recv_stats = stream_stats;
            let send_cancel = cancel.clone();
            let recv_cancel = cancel;
            let send_pause = pause.clone();
            let recv_pause = pause;

            let send_handle = tokio::spawn(async move {
                if let Err(e) = udp::send_udp_paced(
                    send_socket,
                    None, // Connected socket, no target needed
                    stream_bitrate,
                    duration,
                    send_stats,
                    send_cancel,
                    send_pause,
                    random_payload,
                )
                .await
                {
                    error!("UDP bidir send error: {}", e);
                }
            });

            let recv_handle = tokio::spawn(async move {
                if let Err(e) = udp::receive_udp(
                    recv_socket,
                    recv_stats,
                    recv_cancel,
                    recv_pause,
                    false,
                    None,
                )
                .await
                {
                    error!("UDP bidir receive error: {}", e);
                }
            });

            let _ = tokio::join!(send_handle, recv_handle);
        }
    }
}

/// Read a line with bounded length to prevent memory DoS from malicious server
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

fn control_bind_addr(config: &ClientConfig) -> Option<SocketAddr> {
    if config.protocol != Protocol::Tcp {
        return config.bind_addr;
    }

    config.bind_addr.map(|mut addr| {
        if addr.port() != 0 {
            addr.set_port(0);
        }
        addr
    })
}

fn sequential_bind_base(
    bind_addr: Option<SocketAddr>,
    sequential_ports: bool,
    stream_count: usize,
) -> anyhow::Result<Option<SocketAddr>> {
    if sequential_ports {
        let addr = bind_addr.ok_or_else(|| {
            anyhow::anyhow!(
                "Invalid client config: sequential_ports requires bind_addr with a fixed port"
            )
        })?;
        if addr.port() == 0 {
            return Err(anyhow::anyhow!(
                "Invalid client config: sequential_ports requires a non-zero bind port"
            ));
        }
        let max_port = u32::from(addr.port()) + stream_count.saturating_sub(1) as u32;
        if max_port > u16::MAX as u32 {
            return Err(anyhow::anyhow!(
                "Invalid client config: sequential_ports requires ports {}-{}, which exceeds {}",
                addr.port(),
                max_port,
                u16::MAX
            ));
        }
        Ok(Some(addr))
    } else {
        Ok(bind_addr)
    }
}

fn stream_bind_addr(
    base_bind_addr: Option<SocketAddr>,
    sequential_ports: bool,
    stream_index: usize,
) -> Option<SocketAddr> {
    if sequential_ports {
        base_bind_addr.map(|mut addr| {
            let stream_offset = stream_index as u16;
            addr.set_port(addr.port() + stream_offset);
            addr
        })
    } else {
        base_bind_addr
    }
}

fn tcp_data_port_range(config: &ClientConfig) -> Option<(u16, u16)> {
    if config.protocol != Protocol::Tcp {
        return None;
    }

    let bind_addr = config.bind_addr?;
    if bind_addr.port() == 0 {
        return None;
    }

    let start = bind_addr.port();
    let end = if config.sequential_ports {
        start.saturating_add(u16::from(config.streams).saturating_sub(1))
    } else {
        start
    };
    Some((start, end))
}

fn validate_tcp_control_port(
    control_local_addr: SocketAddr,
    config: &ClientConfig,
) -> anyhow::Result<()> {
    if let Some((start, end)) = tcp_data_port_range(config) {
        let port = control_local_addr.port();
        if (start..=end).contains(&port) {
            anyhow::bail!(
                "TCP control connection used local port {}, which overlaps requested data source ports {}-{}. \
                 Choose a different --cport range (preferably outside the OS ephemeral range) and retry.",
                port,
                start,
                end
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::UdpIntervalProgress;

    fn test_stats_with_recv_bytes(per_stream: &[u64]) -> TestStats {
        let stats = TestStats::new("t".to_string(), per_stream.len() as u8);
        for (stream, &bytes) in stats.streams.iter().zip(per_stream) {
            stream
                .bytes_received
                .store(bytes, std::sync::atomic::Ordering::Relaxed);
        }
        stats
    }

    #[test]
    fn udp_recv_interval_deltas_advance_baseline() {
        let stats = test_stats_with_recv_bytes(&[1000, 500]);
        let mut last = vec![0u64; 2];

        let (total, deltas) = udp_recv_interval_deltas(&stats, &mut last);
        assert_eq!(total, 1500);
        assert_eq!(deltas, vec![1000, 500]);

        // No traffic since: deltas must be zero, not re-counted cumulatives.
        let (total, deltas) = udp_recv_interval_deltas(&stats, &mut last);
        assert_eq!(total, 0);
        assert_eq!(deltas, vec![0, 0]);

        stats.streams[1]
            .bytes_received
            .store(800, std::sync::atomic::Ordering::Relaxed);
        let (total, deltas) = udp_recv_interval_deltas(&stats, &mut last);
        assert_eq!(total, 300);
        assert_eq!(deltas, vec![0, 300]);
    }

    #[test]
    fn udp_local_progress_sums_across_streams() {
        let stats = test_stats_with_recv_bytes(&[0, 0]);
        stats.streams[0]
            .udp_packets_received
            .store(100, std::sync::atomic::Ordering::Relaxed);
        stats.streams[0]
            .udp_lost
            .store(7, std::sync::atomic::Ordering::Relaxed);
        stats.streams[1]
            .udp_packets_received
            .store(50, std::sync::atomic::Ordering::Relaxed);
        stats.streams[1]
            .udp_lost
            .store(3, std::sync::atomic::Ordering::Relaxed);

        let p = udp_local_progress(&stats);
        assert_eq!(p.packets_received, 150);
        assert_eq!(p.packets_lost, 10);
    }

    /// Issue #81: a download-mode UDP result carrying the server's send-side
    /// counts (which track the requested -b rate, not the wire) must be
    /// replaced with the client's own receive totals, and the receiver-side
    /// UdpStats (loss/jitter) must be attached.
    #[test]
    fn overlay_udp_download_result_replaces_sender_counts() {
        use crate::protocol::{StreamResult, UdpStats};

        // Client actually received 30 MB across two streams...
        let stats = test_stats_with_recv_bytes(&[20_000_000, 10_000_000]);
        stats.add_udp_stats(UdpStats {
            packets_sent: 0,
            packets_received: 21_000,
            lost: 9_000,
            lost_percent: 30.0,
            jitter_ms: 0.4,
            out_of_order: 2,
            jitter_max_ms: Some(1.2),
            packet_size: Some(1400),
        });

        // ...but the server reported 300 MB of send-side attempts.
        let mut result = TestResult {
            id: "t".to_string(),
            bytes_total: 300_000_000,
            duration_ms: 1_000,
            throughput_mbps: 2_400.0,
            streams: vec![
                StreamResult {
                    id: 0,
                    bytes: 200_000_000,
                    throughput_mbps: 1_600.0,
                    retransmits: None,
                    jitter_ms: None,
                    lost: None,
                },
                StreamResult {
                    id: 1,
                    bytes: 100_000_000,
                    throughput_mbps: 800.0,
                    retransmits: None,
                    jitter_ms: None,
                    lost: None,
                },
            ],
            tcp_info: None,
            udp_stats: None,
            bytes_sent: None,
            bytes_received: None,
            throughput_send_mbps: None,
            throughput_recv_mbps: None,
            mtu_probe: None,
        };

        overlay_udp_download_result(&mut result, &stats);

        assert_eq!(result.bytes_total, 30_000_000);
        assert!((result.throughput_mbps - 240.0).abs() < 0.001);
        assert_eq!(result.streams[0].bytes, 20_000_000);
        assert!((result.streams[0].throughput_mbps - 160.0).abs() < 0.001);
        assert_eq!(result.streams[1].bytes, 10_000_000);
        let udp = result.udp_stats.expect("receiver-side UdpStats attached");
        assert_eq!(udp.lost, 9_000);
        assert!((udp.jitter_ms - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn overlay_udp_download_result_zero_duration_yields_zero_mbps() {
        let stats = test_stats_with_recv_bytes(&[1_000]);
        let mut result = TestResult {
            id: "t".to_string(),
            bytes_total: 5_000,
            duration_ms: 0,
            throughput_mbps: 99.0,
            streams: vec![],
            tcp_info: None,
            udp_stats: None,
            bytes_sent: None,
            bytes_received: None,
            throughput_send_mbps: None,
            throughput_recv_mbps: None,
            mtu_probe: None,
        };
        overlay_udp_download_result(&mut result, &stats);
        assert_eq!(result.bytes_total, 1_000);
        assert_eq!(result.throughput_mbps, 0.0);
    }

    /// Bidir UDP: the download half of the server's split is its send-side
    /// counter and must be replaced with the client's receive totals. The
    /// upload half is already receiver-truth (counted by the server as it
    /// arrived) and must be preserved; the combined totals are rebuilt from
    /// the corrected pair.
    #[test]
    fn overlay_udp_bidir_result_replaces_download_half_only() {
        let stats = test_stats_with_recv_bytes(&[10_000_000, 5_000_000]);

        let mut result = TestResult {
            id: "t".to_string(),
            // Server combined: 20 MB upload truth + 300 MB send-side attempts.
            bytes_total: 320_000_000,
            duration_ms: 1_000,
            throughput_mbps: 2_560.0,
            streams: vec![],
            tcp_info: None,
            udp_stats: None,
            bytes_sent: Some(20_000_000),
            bytes_received: Some(300_000_000),
            throughput_send_mbps: Some(160.0),
            throughput_recv_mbps: Some(2_400.0),
            mtu_probe: None,
        };

        overlay_udp_bidir_result(&mut result, &stats);

        // Upload half untouched.
        assert_eq!(result.bytes_sent, Some(20_000_000));
        assert_eq!(result.throughput_send_mbps, Some(160.0));
        // Download half replaced with our 15 MB receive truth.
        assert_eq!(result.bytes_received, Some(15_000_000));
        assert!((result.throughput_recv_mbps.unwrap() - 120.0).abs() < 0.001);
        // Combined rebuilt from the corrected halves.
        assert_eq!(result.bytes_total, 35_000_000);
        assert!((result.throughput_mbps - 280.0).abs() < 0.001);
    }

    /// A pre-split server (no bytes_sent in the result) gives us no trusted
    /// upload number, so only the receive-side fields are filled in; the
    /// combined total is left as reported rather than rebuilt from a
    /// half-known pair.
    #[test]
    fn overlay_udp_bidir_result_without_server_split_keeps_total() {
        let stats = test_stats_with_recv_bytes(&[15_000_000]);

        let mut result = TestResult {
            id: "t".to_string(),
            bytes_total: 320_000_000,
            duration_ms: 1_000,
            throughput_mbps: 2_560.0,
            streams: vec![],
            tcp_info: None,
            udp_stats: None,
            bytes_sent: None,
            bytes_received: None,
            throughput_send_mbps: None,
            throughput_recv_mbps: None,
            mtu_probe: None,
        };

        overlay_udp_bidir_result(&mut result, &stats);

        assert_eq!(result.bytes_received, Some(15_000_000));
        assert!((result.throughput_recv_mbps.unwrap() - 120.0).abs() < 0.001);
        assert_eq!(result.bytes_total, 320_000_000);
        assert!((result.throughput_mbps - 2_560.0).abs() < 0.001);
    }

    #[test]
    fn udp_progress_filter_accepts_first_update() {
        let f = UdpProgressFilter::new();
        let p = UdpIntervalProgress {
            packets_received: 100,
            packets_lost: 5,
        };
        assert!(f.apply(p).is_some());
    }

    #[test]
    fn udp_progress_filter_accepts_higher_denominator() {
        let f = UdpProgressFilter::new();
        f.apply(UdpIntervalProgress {
            packets_received: 100,
            packets_lost: 5,
        });
        let p2 = UdpIntervalProgress {
            packets_received: 200,
            packets_lost: 10,
        };
        assert!(f.apply(p2).is_some());
    }

    #[test]
    fn udp_progress_filter_rejects_lower_denominator() {
        let f = UdpProgressFilter::new();
        f.apply(UdpIntervalProgress {
            packets_received: 200,
            packets_lost: 10,
        });
        let stale = UdpIntervalProgress {
            packets_received: 150,
            packets_lost: 8,
        };
        assert!(f.apply(stale).is_none());
    }

    #[test]
    fn udp_progress_filter_accepts_equal_denominator() {
        // Cumulative monotonic counts: equal denominator means the
        // same observation, which should pass through unchanged so
        // duplicate updates are idempotent.
        let f = UdpProgressFilter::new();
        f.apply(UdpIntervalProgress {
            packets_received: 100,
            packets_lost: 5,
        });
        let same = UdpIntervalProgress {
            packets_received: 100,
            packets_lost: 5,
        };
        assert!(f.apply(same).is_some());
    }

    #[test]
    fn udp_progress_filter_concurrent_max_holds() {
        // Two threads racing through apply() with denominators
        // straddling a current max. After both complete, the
        // observable max must equal the larger denominator — never
        // the smaller one. A naive load/compare/store implementation
        // would let the lower writer win the final store under
        // interleaving; fetch_update retries until the CAS succeeds.
        use std::sync::Arc;
        use std::sync::atomic::Ordering;
        use std::thread;

        for _ in 0..256 {
            let f = Arc::new(UdpProgressFilter::new());
            let f1 = f.clone();
            let f2 = f.clone();
            let h1 = thread::spawn(move || {
                f1.apply(UdpIntervalProgress {
                    packets_received: 1_000_000,
                    packets_lost: 0,
                });
            });
            let h2 = thread::spawn(move || {
                f2.apply(UdpIntervalProgress {
                    packets_received: 500_000,
                    packets_lost: 0,
                });
            });
            h1.join().unwrap();
            h2.join().unwrap();
            assert_eq!(
                f.max_denominator.load(Ordering::Relaxed),
                1_000_000,
                "lower-denominator producer must not clobber higher"
            );
        }
    }

    #[test]
    fn test_stream_join_timeout_scaling() {
        assert_eq!(stream_join_timeout(1), Duration::from_secs(2));
        assert_eq!(stream_join_timeout(40), Duration::from_secs(2));
        assert_eq!(stream_join_timeout(128), Duration::from_millis(6400));
    }

    #[test]
    fn test_single_port_handshake_parallelism() {
        assert_eq!(single_port_handshake_parallelism(1), 1);
        assert_eq!(single_port_handshake_parallelism(8), 8);
        assert_eq!(single_port_handshake_parallelism(16), 16);
        assert_eq!(single_port_handshake_parallelism(32), 16);
        assert_eq!(single_port_handshake_parallelism(128), 16);
    }

    #[test]
    fn test_local_stop_deadline_none_for_infinite_duration() {
        let start = tokio::time::Instant::now();
        assert!(local_stop_deadline(start, Duration::ZERO).is_none());
    }

    #[test]
    fn test_local_stop_deadline_set_for_finite_duration() {
        let start = tokio::time::Instant::now();
        let duration = Duration::from_secs(10);
        assert_eq!(local_stop_deadline(start, duration), Some(start + duration));
    }

    #[test]
    fn test_control_bind_addr_uses_ephemeral_port_for_tcp_cport() {
        let config = ClientConfig {
            protocol: Protocol::Tcp,
            bind_addr: Some("0.0.0.0:5300".parse().unwrap()),
            ..Default::default()
        };

        assert_eq!(
            control_bind_addr(&config),
            Some("0.0.0.0:0".parse().unwrap())
        );
    }

    #[test]
    fn test_stream_bind_addr_assigns_sequential_ports() {
        let base = Some("0.0.0.0:5300".parse().unwrap());

        assert_eq!(
            stream_bind_addr(base, true, 2),
            Some("0.0.0.0:5302".parse().unwrap())
        );
        assert_eq!(stream_bind_addr(base, false, 2), base);
    }

    #[test]
    fn test_validate_tcp_control_port_detects_overlap() {
        let config = ClientConfig {
            protocol: Protocol::Tcp,
            streams: 4,
            bind_addr: Some("0.0.0.0:5300".parse().unwrap()),
            sequential_ports: true,
            ..Default::default()
        };

        let err = validate_tcp_control_port("127.0.0.1:5302".parse().unwrap(), &config)
            .unwrap_err()
            .to_string();
        assert!(err.contains("overlaps requested data source ports 5300-5303"));
    }
}
