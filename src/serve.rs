//! Server mode implementation
//!
//! Listens for incoming connections and handles bandwidth tests.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Mutex, mpsc, watch};
use tracing::{debug, error, info, warn};

use crate::protocol::{
    ControlMessage, Direction, PROTOCOL_VERSION, Protocol, StreamInterval, TestResult,
};
use crate::stats::{StreamStats, TestStats};
use crate::tcp::{self, TcpConfig};
use crate::udp;

pub struct ServerConfig {
    pub port: u16,
    pub one_off: bool,
    #[cfg(feature = "prometheus")]
    pub prometheus_port: Option<u16>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: crate::protocol::DEFAULT_PORT,
            one_off: false,
            #[cfg(feature = "prometheus")]
            prometheus_port: None,
        }
    }
}

struct ActiveTest {
    #[allow(dead_code)]
    stats: Arc<TestStats>,
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
        let addr = format!("0.0.0.0:{}", self.config.port);
        let listener = TcpListener::bind(&addr).await?;
        info!("xfr server listening on {}", addr);

        loop {
            let (stream, peer_addr) = listener.accept().await?;
            info!("Client connected: {}", peer_addr);

            let active_tests = self.active_tests.clone();
            let base_port = self.config.port;

            let handle = tokio::spawn(async move {
                if let Err(e) = handle_client(stream, peer_addr, active_tests, base_port).await {
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

async fn handle_client(
    stream: TcpStream,
    _peer_addr: SocketAddr,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    base_port: u16,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    // Read client hello
    reader.read_line(&mut line).await?;
    let msg: ControlMessage = ControlMessage::deserialize(line.trim())?;

    match msg {
        ControlMessage::Hello { version, .. } => {
            if !version.starts_with(PROTOCOL_VERSION.split('.').next().unwrap_or("1")) {
                let error = ControlMessage::error(format!(
                    "Incompatible protocol version: {} (server: {})",
                    version, PROTOCOL_VERSION
                ));
                writer
                    .write_all(format!("{}\n", error.serialize()?).as_bytes())
                    .await?;
                return Err(anyhow::anyhow!("Protocol version mismatch"));
            }

            // Send server hello
            let hello = ControlMessage::server_hello();
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

    // Read test request
    line.clear();
    reader.read_line(&mut line).await?;
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
            info!(
                "Test requested: {} {} streams, {} mode, {}s",
                protocol, streams, direction, duration_secs
            );

            // Allocate data ports
            let data_ports: Vec<u16> = (0..streams).map(|i| base_port + 1 + i as u16).collect();

            // Send test ack
            let ack = ControlMessage::TestAck {
                id: id.clone(),
                data_ports: data_ports.clone(),
            };
            writer
                .write_all(format!("{}\n", ack.serialize()?).as_bytes())
                .await?;

            // Create test stats
            let stats = Arc::new(TestStats::new(id.clone(), streams));
            let (cancel_tx, cancel_rx) = watch::channel(false);

            // Store active test
            {
                let mut tests = active_tests.lock().await;
                tests.insert(
                    id.clone(),
                    ActiveTest {
                        stats: stats.clone(),
                        cancel_tx,
                        data_ports: data_ports.clone(),
                    },
                );
            }

            // Spawn data stream handlers
            let duration = Duration::from_secs(duration_secs as u64);
            let (interval_tx, mut interval_rx) = mpsc::channel(100);

            match protocol {
                Protocol::Tcp => {
                    spawn_tcp_handlers(
                        &data_ports,
                        stats.clone(),
                        direction,
                        duration,
                        cancel_rx.clone(),
                        interval_tx,
                    )
                    .await?;
                }
                Protocol::Udp => {
                    spawn_udp_handlers(
                        &data_ports,
                        stats.clone(),
                        direction,
                        duration,
                        bitrate.unwrap_or(1_000_000_000), // 1 Gbps default
                        cancel_rx.clone(),
                        interval_tx,
                    )
                    .await?;
                }
            }

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
                            id: id.clone(),
                            elapsed_ms: stats.elapsed_ms(),
                            streams: stream_intervals,
                            aggregate,
                        };

                        if writer.write_all(format!("{}\n", interval_msg.serialize()?).as_bytes()).await.is_err() {
                            warn!("Failed to send interval, client may have disconnected");
                            break;
                        }
                    }
                    Some(_) = interval_rx.recv() => {
                        // Data handler completed
                    }
                }

                // Check for cancel message
                line.clear();
                let read_result =
                    tokio::time::timeout(Duration::from_millis(10), reader.read_line(&mut line))
                        .await;

                if let Ok(Ok(n)) = read_result {
                    if n > 0 {
                        if let Ok(ControlMessage::Cancel {
                            id: cancel_id,
                            reason,
                        }) = ControlMessage::deserialize(line.trim())
                        {
                            if cancel_id == id {
                                info!("Test {} cancelled: {}", id, reason);
                                if let Some(test) = active_tests.lock().await.get(&id) {
                                    let _ = test.cancel_tx.send(true);
                                }
                                let cancelled = ControlMessage::Cancelled { id: id.clone() };
                                writer
                                    .write_all(format!("{}\n", cancelled.serialize()?).as_bytes())
                                    .await?;
                                break;
                            }
                        }
                    }
                }
            }

            // Wait for data handlers to finish
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Send final result
            let duration_ms = stats.elapsed_ms();
            let bytes_total = stats.total_bytes();
            let throughput_mbps =
                (bytes_total as f64 * 8.0) / (duration_ms as f64 / 1000.0) / 1_000_000.0;

            let stream_results: Vec<_> = stats
                .streams
                .iter()
                .map(|s| s.to_result(duration_ms))
                .collect();

            let result = ControlMessage::Result(TestResult {
                id: id.clone(),
                bytes_total,
                duration_ms,
                throughput_mbps,
                streams: stream_results,
                tcp_info: stats.tcp_info.lock().clone(),
                udp_stats: None,
            });

            writer
                .write_all(format!("{}\n", result.serialize()?).as_bytes())
                .await?;

            // Cleanup
            active_tests.lock().await.remove(&id);
            info!(
                "Test {} complete: {:.2} Mbps, {} bytes",
                id, throughput_mbps, bytes_total
            );
        }
        _ => {
            let error = ControlMessage::error("Expected test_start message");
            writer
                .write_all(format!("{}\n", error.serialize()?).as_bytes())
                .await?;
        }
    }

    Ok(())
}

async fn spawn_tcp_handlers(
    data_ports: &[u16],
    stats: Arc<TestStats>,
    direction: Direction,
    duration: Duration,
    cancel: watch::Receiver<bool>,
    _interval_tx: mpsc::Sender<()>,
) -> anyhow::Result<()> {
    // Pre-bind all listeners first
    let mut listeners = Vec::new();
    for &port in data_ports {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
        debug!("Data port {} listening", port);
        listeners.push(listener);
    }

    for (i, listener) in listeners.into_iter().enumerate() {
        let cancel = cancel.clone();
        let stats = stats.clone();

        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let config = TcpConfig::default();
                // Use a new StreamStats that we can track
                let stream_stats = Arc::new(StreamStats::new(i as u8));

                match direction {
                    Direction::Upload => {
                        // Server receives data
                        let _ =
                            tcp::receive_data(stream, stream_stats.clone(), cancel, config).await;
                    }
                    Direction::Download => {
                        // Server sends data
                        let _ =
                            tcp::send_data(stream, stream_stats.clone(), duration, config, cancel)
                                .await;
                    }
                    Direction::Bidir => {
                        // Both directions - needs split
                        let _ =
                            tcp::receive_data(stream, stream_stats.clone(), cancel, config).await;
                    }
                }

                // Copy the bytes to the main stats after transfer completes
                let bytes = stream_stats.total_bytes();
                if let Some(main_stream) = stats.streams.get(i) {
                    main_stream.add_bytes_received(bytes);
                }
            }
        });
    }

    Ok(())
}

async fn spawn_udp_handlers(
    data_ports: &[u16],
    stats: Arc<TestStats>,
    direction: Direction,
    duration: Duration,
    bitrate: u64,
    cancel: watch::Receiver<bool>,
    _interval_tx: mpsc::Sender<()>,
) -> anyhow::Result<()> {
    for (i, &port) in data_ports.iter().enumerate() {
        let stream_stats = Arc::new(StreamStats::new(i as u8));
        let cancel = cancel.clone();
        let stats = stats.clone();

        let socket = Arc::new(UdpSocket::bind(format!("0.0.0.0:{}", port)).await?);
        debug!("UDP port {} bound", port);

        tokio::spawn(async move {
            match direction {
                Direction::Upload => {
                    // Server receives UDP
                    let _ = udp::receive_udp(socket, stream_stats.clone(), cancel).await;
                }
                Direction::Download => {
                    // Server sends UDP
                    let _ = udp::send_udp_paced(
                        socket,
                        bitrate,
                        duration,
                        stream_stats.clone(),
                        cancel,
                    )
                    .await;
                }
                Direction::Bidir => {
                    let _ = udp::receive_udp(socket, stream_stats.clone(), cancel).await;
                }
            }

            // Copy the bytes to the main stats
            let bytes = stream_stats.total_bytes();
            if let Some(main_stream) = stats.streams.get(i) {
                main_stream.add_bytes_received(bytes);
            }
        });
    }

    Ok(())
}
