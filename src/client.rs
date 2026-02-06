//! Client mode implementation
//!
//! Connects to a server and runs bandwidth tests.

use parking_lot::Mutex;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Maximum control message line length to prevent memory DoS
const MAX_LINE_LENGTH: usize = 8192;

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
    /// Local address to bind to
    pub bind_addr: Option<SocketAddr>,
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
}

pub struct Client {
    config: ClientConfig,
    /// Signals data stream handlers to stop
    cancel_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
    /// Signals the control loop to send a Cancel message to server
    cancel_request_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
}

impl Client {
    pub fn new(config: ClientConfig) -> Self {
        Self {
            config,
            cancel_tx: Arc::new(Mutex::new(None)),
            cancel_request_tx: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn run(
        &self,
        progress_tx: Option<mpsc::Sender<TestProgress>>,
    ) -> anyhow::Result<TestResult> {
        info!("Connecting to {}:{}...", self.config.host, self.config.port);

        // Warn if bitrate limit is set for non-UDP protocols (only UDP supports pacing)
        if self.config.protocol == Protocol::Tcp && self.config.bitrate.is_some() {
            warn!("Bitrate limit (-b) only works for UDP; TCP will run at full speed");
        }
        if self.config.protocol == Protocol::Quic && self.config.bitrate.is_some() {
            warn!("Bitrate limit (-b) not implemented for QUIC; running at full speed");
        }

        // Use QUIC transport if selected
        if self.config.protocol == Protocol::Quic {
            return self.run_quic(progress_tx).await;
        }

        let (stream, peer_addr) = net::connect_tcp(
            &self.config.host,
            self.config.port,
            self.config.address_family,
            self.config.bind_addr,
        )
        .await?;

        self.run_test(stream, peer_addr, progress_tx).await
    }

    async fn run_test(
        &self,
        stream: TcpStream,
        server_ip: SocketAddr,
        progress_tx: Option<mpsc::Sender<TestProgress>>,
    ) -> anyhow::Result<TestResult> {
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

        // Connect data streams using the resolved IP from control connection
        match self.config.protocol {
            Protocol::Tcp => {
                self.spawn_tcp_streams(
                    &data_ports,
                    server_ip,
                    stats.clone(),
                    cancel_rx.clone(),
                    &test_id,
                )
                .await?;
            }
            Protocol::Udp => {
                self.spawn_udp_streams(&data_ports, server_ip, stats.clone(), cancel_rx.clone())
                    .await?;
            }
            Protocol::Quic => {
                // QUIC uses its own connection model - should not reach here
                return Err(anyhow::anyhow!(
                    "QUIC protocol uses run_quic(), not run_test()"
                ));
            }
        }

        // Read interval updates and final result with timeout
        // For infinite duration, use 1 year timeout (effectively no timeout)
        let timeout_duration = if self.config.duration == Duration::ZERO {
            Duration::from_secs(365 * 24 * 3600) // 1 year
        } else {
            self.config.duration + Duration::from_secs(30)
        };
        let deadline = tokio::time::Instant::now() + timeout_duration;

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
                            let _ = cancel_tx.send(true);
                            return Err(e);
                        }
                        Err(_) => {
                            // Timeout
                            let _ = cancel_tx.send(true);
                            return Err(anyhow::anyhow!("Timeout waiting for server response"));
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
                        let _ = writer.write_all(format!("{}\n", cancel_msg.serialize()?).as_bytes()).await;
                        let _ = cancel_tx.send(true);
                        // Continue loop to receive Cancelled response
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
                                streams,
                            })
                            .await;
                    }
                }
                ControlMessage::Result(result) => {
                    let _ = cancel_tx.send(true);
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

    async fn spawn_tcp_streams(
        &self,
        data_ports: &[u16],
        server_addr: SocketAddr,
        stats: Arc<TestStats>,
        cancel: watch::Receiver<bool>,
        test_id: &str,
    ) -> anyhow::Result<()> {
        // Single-port mode: connect all streams to control port with DataHello
        let single_port_mode = data_ports.is_empty();
        let control_port = self.config.port;
        let test_id = test_id.to_string();

        #[allow(clippy::needless_range_loop)] // Intentional: single-port mode has empty data_ports
        for i in 0..self.config.streams as usize {
            let port = if single_port_mode {
                control_port
            } else {
                // Bounds check: server may return fewer ports than requested streams
                *data_ports.get(i).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Server returned {} data ports but {} streams were requested",
                        data_ports.len(),
                        self.config.streams
                    )
                })?
            };
            let addr = SocketAddr::new(server_addr.ip(), port);
            let stream_stats = stats.streams[i].clone();
            let cancel = cancel.clone();
            let direction = self.config.direction;
            let duration = self.config.duration;
            let bind_addr = self.config.bind_addr;
            let test_id = test_id.clone();
            let stream_index = i as u16;

            let mut config = TcpConfig::with_auto_detect(
                self.config.tcp_nodelay,
                self.config.window_size,
                self.config.bitrate,
            );
            config.congestion = self.config.tcp_congestion.clone();

            tokio::spawn(async move {
                match net::connect_tcp_with_bind(addr, bind_addr).await {
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

                        match direction {
                            Direction::Upload => {
                                // Client sends data
                                if let Err(e) =
                                    tcp::send_data(stream, stream_stats, duration, config, cancel)
                                        .await
                                {
                                    error!("Send error: {}", e);
                                }
                            }
                            Direction::Download => {
                                // Client receives data
                                if let Err(e) =
                                    tcp::receive_data(stream, stream_stats, cancel, config).await
                                {
                                    error!("Receive error: {}", e);
                                }
                            }
                            Direction::Bidir => {
                                // Configure socket BEFORE splitting (nodelay, window, buffers)
                                if let Err(e) = tcp::configure_stream(&stream, &config) {
                                    error!("Failed to configure TCP socket: {}", e);
                                }

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
                                    congestion: config.congestion.clone(),
                                };
                                let recv_config = config;

                                let send_handle = tokio::spawn(async move {
                                    if let Err(e) = tcp::send_data_half(
                                        write_half,
                                        send_stats,
                                        duration,
                                        send_config,
                                        send_cancel,
                                    )
                                    .await
                                    {
                                        error!("Bidir send error: {}", e);
                                    }
                                });

                                let recv_handle = tokio::spawn(async move {
                                    if let Err(e) = tcp::receive_data_half(
                                        read_half,
                                        recv_stats,
                                        recv_cancel,
                                        recv_config,
                                    )
                                    .await
                                    {
                                        error!("Bidir receive error: {}", e);
                                    }
                                });

                                let _ = tokio::join!(send_handle, recv_handle);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to connect to data port {}: {}", port, e);
                    }
                }
            });
        }

        // Give streams time to connect
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(())
    }

    async fn spawn_udp_streams(
        &self,
        data_ports: &[u16],
        server_addr: SocketAddr,
        stats: Arc<TestStats>,
        cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let bitrate = self.config.bitrate.unwrap_or(1_000_000_000); // 1 Gbps default

        // Calculate per-stream bitrate, clamping to at least 1 bps to prevent
        // integer division underflow (e.g., 100 bps / 8 streams = 12 bps, not 0)
        // Only bitrate=0 means unlimited (explicit -b 0)
        let stream_bitrate = if bitrate == 0 {
            0 // Unlimited mode (explicit -b 0)
        } else {
            (bitrate / self.config.streams as u64).max(1)
        };

        for (i, &port) in data_ports.iter().enumerate() {
            let server_port = SocketAddr::new(server_addr.ip(), port);
            let stream_stats = stats.streams[i].clone();
            let cancel = cancel.clone();
            let direction = self.config.direction;
            let duration = self.config.duration;
            let bind_addr = self.config.bind_addr;

            tokio::spawn(async move {
                // Create UDP socket matching the server's address family for cross-platform compatibility.
                // macOS dual-stack sockets behave differently than Linux, so we match the server's family.
                let socket = if let Some(local) = bind_addr {
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

                        if let Err(e) = udp::receive_udp(socket, stream_stats, cancel).await {
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

                        let send_handle = tokio::spawn(async move {
                            if let Err(e) = udp::send_udp_paced(
                                send_socket,
                                None, // Connected socket, no target needed
                                stream_bitrate,
                                duration,
                                send_stats,
                                send_cancel,
                            )
                            .await
                            {
                                error!("UDP bidir send error: {}", e);
                            }
                        });

                        let recv_handle = tokio::spawn(async move {
                            if let Err(e) =
                                udp::receive_udp(recv_socket, recv_stats, recv_cancel).await
                            {
                                error!("UDP bidir receive error: {}", e);
                            }
                        });

                        let _ = tokio::join!(send_handle, recv_handle);
                    }
                }
            });
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(())
    }

    /// Run a test using QUIC transport
    async fn run_quic(
        &self,
        progress_tx: Option<mpsc::Sender<TestProgress>>,
    ) -> anyhow::Result<TestResult> {
        use tokio::io::BufReader;

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

        // Spawn data streams based on direction
        for i in 0..self.config.streams {
            let stream_stats = stats.streams[i as usize].clone();
            let cancel = cancel_rx.clone();
            let duration = self.config.duration;
            let direction = self.config.direction;
            let conn = connection.clone();

            tokio::spawn(async move {
                match direction {
                    Direction::Upload => {
                        // Open unidirectional stream for sending
                        match conn.open_uni().await {
                            Ok(send) => {
                                if let Err(e) =
                                    quic::send_quic_data(send, stream_stats, duration, cancel).await
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

                                let send_handle = tokio::spawn(async move {
                                    if let Err(e) = quic::send_quic_data(
                                        send,
                                        send_stats,
                                        duration,
                                        send_cancel,
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
