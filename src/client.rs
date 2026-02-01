//! Client mode implementation
//!
//! Connects to a server and runs bandwidth tests.

use parking_lot::Mutex;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::protocol::{
    ControlMessage, Direction, PROTOCOL_VERSION, Protocol, StreamInterval, TestResult,
    versions_compatible,
};
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
    pub window_size: Option<usize>,
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
            window_size: None,
        }
    }
}

pub struct TestProgress {
    pub elapsed_ms: u64,
    pub total_bytes: u64,
    pub throughput_mbps: f64,
    pub streams: Vec<StreamInterval>,
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
        let addr = format!("{}:{}", self.config.host, self.config.port);
        info!("Connecting to {}...", addr);

        let stream = TcpStream::connect(&addr).await?;
        let peer_addr = stream.peer_addr()?;
        info!("Connected to {}", peer_addr);

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

        // Read server hello
        reader.read_line(&mut line).await?;
        let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

        match msg {
            ControlMessage::Hello {
                version,
                capabilities,
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
        };
        writer
            .write_all(format!("{}\n", test_start.serialize()?).as_bytes())
            .await?;

        // Read test ack
        line.clear();
        reader.read_line(&mut line).await?;
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
                self.spawn_tcp_streams(&data_ports, server_ip, stats.clone(), cancel_rx.clone())
                    .await?;
            }
            Protocol::Udp => {
                self.spawn_udp_streams(&data_ports, server_ip, stats.clone(), cancel_rx.clone())
                    .await?;
            }
        }

        // Read interval updates and final result with timeout
        let deadline = tokio::time::Instant::now() + self.config.duration + Duration::from_secs(30);

        loop {
            line.clear();

            // Check for external cancel request while waiting for server messages
            tokio::select! {
                read_result = tokio::time::timeout_at(deadline, reader.read_line(&mut line)) => {
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
                            return Err(e.into());
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
    ) -> anyhow::Result<()> {
        for (i, &port) in data_ports.iter().enumerate() {
            let addr = SocketAddr::new(server_addr.ip(), port);
            let stream_stats = stats.streams[i].clone();
            let cancel = cancel.clone();
            let direction = self.config.direction;
            let duration = self.config.duration;

            let config = TcpConfig::with_auto_detect(
                self.config.tcp_nodelay,
                self.config.window_size,
                self.config.bitrate,
            );

            tokio::spawn(async move {
                match TcpStream::connect(addr).await {
                    Ok(stream) => {
                        debug!("Connected to data port {}", port);

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

        for (i, &port) in data_ports.iter().enumerate() {
            let server_port = SocketAddr::new(server_addr.ip(), port);
            let stream_stats = stats.streams[i].clone();
            let cancel = cancel.clone();
            let direction = self.config.direction;
            let duration = self.config.duration;
            let stream_bitrate = bitrate / self.config.streams as u64;

            tokio::spawn(async move {
                let socket = match UdpSocket::bind("0.0.0.0:0").await {
                    Ok(s) => Arc::new(s),
                    Err(e) => {
                        error!("Failed to bind UDP socket: {}", e);
                        return;
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
                        if let Err(e) = udp::receive_udp(socket, stream_stats, cancel).await {
                            error!("UDP receive error: {}", e);
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

                        let send_handle = tokio::spawn(async move {
                            if let Err(e) = udp::send_udp_paced(
                                send_socket,
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
