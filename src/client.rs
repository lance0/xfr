//! Client mode implementation
//!
//! Connects to a server and runs bandwidth tests.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::protocol::{
    ControlMessage, Direction, Protocol, StreamInterval, TestResult, PROTOCOL_VERSION,
};
use crate::stats::{StreamStats, TestStats};
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
}

impl Client {
    pub fn new(config: ClientConfig) -> Self {
        Self { config }
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

        self.run_test(stream, progress_tx).await
    }

    async fn run_test(
        &self,
        stream: TcpStream,
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
                if !version.starts_with(PROTOCOL_VERSION.split('.').next().unwrap_or("1")) {
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

        // Connect data streams
        let server_addr: SocketAddr = format!("{}:{}", self.config.host, self.config.port)
            .parse()
            .unwrap();

        match self.config.protocol {
            Protocol::Tcp => {
                self.spawn_tcp_streams(&data_ports, server_addr, stats.clone(), cancel_rx.clone())
                    .await?;
            }
            Protocol::Udp => {
                self.spawn_udp_streams(&data_ports, server_addr, stats.clone(), cancel_rx.clone())
                    .await?;
            }
        }

        // Read interval updates and final result
        loop {
            line.clear();
            reader.read_line(&mut line).await?;

            if line.is_empty() {
                break;
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
        _stats: Arc<TestStats>,
        cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        for (i, &port) in data_ports.iter().enumerate() {
            let addr = SocketAddr::new(server_addr.ip(), port);
            let stream_stats = Arc::new(StreamStats::new(i as u8));
            let cancel = cancel.clone();
            let direction = self.config.direction;
            let duration = self.config.duration;

            let config = TcpConfig {
                nodelay: self.config.tcp_nodelay,
                window_size: self.config.window_size,
                ..Default::default()
            };

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
                                // Client sends (receive handled elsewhere)
                                if let Err(e) =
                                    tcp::send_data(stream, stream_stats, duration, config, cancel)
                                        .await
                                {
                                    error!("Send error: {}", e);
                                }
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
        _stats: Arc<TestStats>,
        cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let bitrate = self.config.bitrate.unwrap_or(1_000_000_000); // 1 Gbps default

        for (i, &port) in data_ports.iter().enumerate() {
            let server_port = SocketAddr::new(server_addr.ip(), port);
            let stream_stats = Arc::new(StreamStats::new(i as u8));
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
                }
            });
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(())
    }

    pub async fn cancel(&self) -> anyhow::Result<()> {
        // TODO: Implement cancel via control channel
        Ok(())
    }
}
