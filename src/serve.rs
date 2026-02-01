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

use crate::protocol::{
    ControlMessage, Direction, PROTOCOL_VERSION, Protocol, StreamInterval, TestResult,
    versions_compatible,
};
use crate::stats::TestStats;
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
    _base_port: u16,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
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

            // Dynamically allocate data ports (bind to 0 for OS-assigned ports)
            let mut data_ports = Vec::new();
            let duration = Duration::from_secs(duration_secs as u64);

            // Pre-bind listeners/sockets to get actual ports
            let (tcp_listeners, udp_sockets) = match protocol {
                Protocol::Tcp => {
                    let mut listeners = Vec::new();
                    for _ in 0..streams {
                        let listener = TcpListener::bind("0.0.0.0:0").await?;
                        data_ports.push(listener.local_addr()?.port());
                        debug!("Data port {} allocated", data_ports.last().unwrap());
                        listeners.push(listener);
                    }
                    (listeners, Vec::new())
                }
                Protocol::Udp => {
                    let mut sockets = Vec::new();
                    for _ in 0..streams {
                        let socket = UdpSocket::bind("0.0.0.0:0").await?;
                        data_ports.push(socket.local_addr()?.port());
                        debug!("UDP port {} allocated", data_ports.last().unwrap());
                        sockets.push(Arc::new(socket));
                    }
                    (Vec::new(), sockets)
                }
            };

            // Send test ack with allocated ports
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

            // Signal handlers to stop
            if let Some(test) = active_tests.lock().await.get(&id) {
                let _ = test.cancel_tx.send(true);
            }

            // Wait for all data handlers to complete
            futures::future::join_all(handles).await;

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
                udp_stats: stats.udp_stats.lock().clone(),
            });

            writer
                .write_all(format!("{}\n", result.serialize()?).as_bytes())
                .await?;

            // Notify metrics that test completed
            #[cfg(feature = "prometheus")]
            crate::output::prometheus::on_test_complete(&stats);

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
            if let Ok((stream, _)) = listener.accept().await {
                let config = TcpConfig::default();

                // Capture TCP_INFO before transfer starts
                if let Some(info) = tcp::get_stream_tcp_info(&stream) {
                    test_stats.update_tcp_info(info);
                }

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
                            let _ = tcp::receive_data_half(
                                read_half,
                                recv_stats,
                                recv_cancel,
                                recv_config,
                            )
                            .await;
                        });

                        let _ = tokio::join!(send_handle, recv_handle);
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
                        test_stats.update_udp_stats(udp_stats);
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
                            test_stats_copy.update_udp_stats(udp_stats);
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
    use mdns_sd::{ServiceDaemon, ServiceInfo};

    let mdns = match ServiceDaemon::new() {
        Ok(m) => m,
        Err(e) => {
            warn!("Failed to create mDNS daemon: {}", e);
            return None;
        }
    };

    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "xfr-server".to_string());

    let service_type = "_xfr._tcp.local.";
    let instance_name = hostname.clone();

    match ServiceInfo::new(
        service_type,
        &instance_name,
        &format!("{}.", hostname),
        (),
        port,
        None,
    ) {
        Ok(service) => {
            if let Err(e) = mdns.register(service) {
                warn!("Failed to register mDNS service: {}", e);
                return None;
            }
            info!(
                "Registered mDNS service: {}.{}",
                instance_name, service_type
            );
            Some(mdns)
        }
        Err(e) => {
            warn!("Failed to create mDNS service info: {}", e);
            None
        }
    }
}
