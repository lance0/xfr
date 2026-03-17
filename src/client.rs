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
        }
    }
}

pub struct TestProgress {
    pub elapsed_ms: u64,
    pub total_bytes: u64,
    pub throughput_mbps: f64,
    pub streams: Vec<StreamInterval>,
    pub rtt_us: Option<u32>,
    pub cwnd: Option<u32>,
    /// Cumulative retransmits from local TCP_INFO (sender-side, for upload/bidir)
    pub total_retransmits: Option<u64>,
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
        }
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
        let (stream, peer_addr) = net::connect_tcp(
            &self.config.host,
            self.config.port,
            self.config.address_family,
            control_bind_addr,
            self.config.mptcp,
        )
        .await?;

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
        };
        writer
            .write_all(format!("{}\n", test_start.serialize()?).as_bytes())
            .await?;

        // Read test ack
        read_bounded_line(&mut reader, &mut line).await?;
        let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

        let data_ports = match msg {
            ControlMessage::TestAck { data_ports, .. } => {
                debug!("Server allocated ports: {:?}", data_ports);
                data_ports
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

        // Create stats
        let stats = Arc::new(TestStats::new(test_id.clone(), self.config.streams));
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (cancel_request_tx, mut cancel_request_rx) = watch::channel(false);

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
                self.spawn_udp_streams(
                    &data_ports,
                    server_ip,
                    stats.clone(),
                    cancel_rx.clone(),
                    pause_rx.clone(),
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
        let deadline = tokio::time::Instant::now() + timeout_duration;
        let local_end_deadline =
            local_stop_deadline(tokio::time::Instant::now(), self.config.duration);
        let mut local_stop_sent = false;

        let mut test_result: anyhow::Result<TestResult> =
            Err(anyhow::anyhow!("Connection closed without result"));

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
                        let _ = tx
                            .send(TestProgress {
                                elapsed_ms,
                                total_bytes: aggregate.bytes,
                                throughput_mbps: aggregate.throughput_mbps,
                                rtt_us,
                                cwnd,
                                total_retransmits,
                                streams,
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
                    test_result = Err(anyhow::anyhow!("Test was cancelled"));
                    break;
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
            let test_id = test_id.clone();
            let stream_index = i as u16;
            let handshake_limiter = handshake_limiter.clone();

            let mut config = TcpConfig::with_auto_detect(
                self.config.tcp_nodelay,
                self.config.window_size,
                self.config.bitrate,
            );
            config.congestion = self.config.tcp_congestion.clone();
            config.random_payload = self.config.random_payload;

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

                                if let (Ok(write_half), Ok(read_half)) = (send_result, recv_result)
                                    && let Ok(stream) = read_half.reunite(write_half)
                                    && let Some(info) = tcp::get_stream_tcp_info(&stream)
                                {
                                    stream_stats.set_final_tcp_info(info);
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

    async fn spawn_udp_streams(
        &self,
        data_ports: &[u16],
        server_addr: SocketAddr,
        stats: Arc<TestStats>,
        cancel: watch::Receiver<bool>,
        pause: watch::Receiver<bool>,
    ) -> anyhow::Result<Vec<tokio::task::JoinHandle<()>>> {
        let base_bind_addr = sequential_bind_base(
            self.config.bind_addr,
            self.config.sequential_ports,
            data_ports.len(),
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

        if data_ports.len() != self.config.streams as usize {
            return Err(anyhow::anyhow!(
                "Server returned {} data ports but {} streams were requested",
                data_ports.len(),
                self.config.streams
            ));
        }

        let mut handles = Vec::new();

        for (i, &port) in data_ports.iter().enumerate() {
            let server_port = SocketAddr::new(server_addr.ip(), port);
            let stream_stats = stats.streams[i].clone();
            let cancel = cancel.clone();
            let pause = pause.clone();
            let direction = self.config.direction;
            let duration = self.config.duration;
            let random_payload = self.config.random_payload;
            let bind_addr = stream_bind_addr(base_bind_addr, self.config.sequential_ports, i);

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

                match direction {
                    Direction::Upload => {
                        if let Err(e) = udp::send_udp_paced(
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
                        // Send hello packets concurrently with receiving
                        // This ensures server learns our address even if first packets are missed
                        let hello_socket = socket.clone();
                        let hello_cancel = cancel.clone();
                        let hello_handle = tokio::spawn(async move {
                            // Send hello packets every 100ms until cancelled or 5 seconds
                            for _ in 0..50 {
                                if *hello_cancel.borrow() {
                                    break;
                                }
                                let _ = hello_socket.send(&[0u8; 1]).await;
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        });

                        if let Err(e) = udp::receive_udp(socket, stream_stats, cancel, pause).await
                        {
                            error!("UDP receive error: {}", e);
                        }
                        hello_handle.abort();
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
                            if let Err(e) =
                                udp::receive_udp(recv_socket, recv_stats, recv_cancel, recv_pause)
                                    .await
                            {
                                error!("UDP bidir receive error: {}", e);
                            }
                        });

                        let _ = tokio::join!(send_handle, recv_handle);
                    }
                }
            }));
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(handles)
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
        let connection = quic::connect(&endpoint, addr).await?;

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
        let deadline = tokio::time::Instant::now() + timeout_duration;

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
                    let _ = cancel_tx.send(true);
                    return Err(anyhow::anyhow!("Test was cancelled"));
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
