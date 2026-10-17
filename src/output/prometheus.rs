//! Prometheus metrics endpoint

#[cfg(feature = "prometheus")]
use std::net::SocketAddr;
#[cfg(feature = "prometheus")]
use std::sync::Arc;
#[cfg(feature = "prometheus")]
use std::time::Duration;

#[cfg(feature = "prometheus")]
use http_body_util::Full;
#[cfg(feature = "prometheus")]
use hyper::body::Bytes;
#[cfg(feature = "prometheus")]
use hyper::service::service_fn;
#[cfg(feature = "prometheus")]
use hyper::{Request, Response};
#[cfg(feature = "prometheus")]
use hyper_util::rt::{TokioIo, TokioTimer};
#[cfg(feature = "prometheus")]
use once_cell::sync::Lazy;
#[cfg(feature = "prometheus")]
use prometheus::{
    Counter, CounterVec, Encoder, Gauge, GaugeVec, Histogram, HistogramOpts, IntGauge, Opts,
    TextEncoder,
};
#[cfg(feature = "prometheus")]
use tokio::net::TcpListener;
#[cfg(feature = "prometheus")]
use tokio::sync::Semaphore;
#[cfg(feature = "prometheus")]
use tracing::{debug, info, warn};

#[cfg(feature = "prometheus")]
use crate::stats::TestStats;

#[cfg(feature = "prometheus")]
pub struct XfrMetrics {
    // Aggregate metrics
    pub bytes_total: Counter,
    pub throughput_mbps: Gauge,
    pub tests_total: Counter,
    pub test_duration_seconds: Histogram,

    // Per-stream metrics
    pub stream_bytes_total: CounterVec,
    pub stream_throughput_mbps: GaugeVec,
    pub stream_retransmits: CounterVec,

    // TCP-specific metrics
    pub tcp_rtt_microseconds: GaugeVec,
    pub tcp_retransmits_total: CounterVec,

    // Test state
    pub active_tests: IntGauge,
}

#[cfg(feature = "prometheus")]
impl XfrMetrics {
    fn new() -> Self {
        let bytes_total = Counter::with_opts(Opts::new(
            "xfr_bytes_total",
            "Total bytes transferred across all tests",
        ))
        .unwrap();

        let throughput_mbps = Gauge::with_opts(Opts::new(
            "xfr_throughput_mbps",
            "Current aggregate throughput in Mbps",
        ))
        .unwrap();

        let tests_total = Counter::with_opts(Opts::new(
            "xfr_tests_total",
            "Total number of completed tests",
        ))
        .unwrap();

        let test_duration_seconds = Histogram::with_opts(
            HistogramOpts::new(
                "xfr_test_duration_seconds",
                "Distribution of test durations in seconds",
            )
            .buckets(vec![1.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0]),
        )
        .unwrap();

        let stream_bytes_total = CounterVec::new(
            Opts::new("xfr_stream_bytes_total", "Bytes transferred per stream"),
            &["test_id", "stream_id"],
        )
        .unwrap();

        let stream_throughput_mbps = GaugeVec::new(
            Opts::new(
                "xfr_stream_throughput_mbps",
                "Current throughput per stream in Mbps",
            ),
            &["test_id", "stream_id"],
        )
        .unwrap();

        let stream_retransmits = CounterVec::new(
            Opts::new("xfr_stream_retransmits_total", "TCP retransmits per stream"),
            &["test_id", "stream_id"],
        )
        .unwrap();

        let tcp_rtt_microseconds = GaugeVec::new(
            Opts::new("xfr_tcp_rtt_microseconds", "TCP round-trip time"),
            &["test_id"],
        )
        .unwrap();

        let tcp_retransmits_total = CounterVec::new(
            Opts::new("xfr_tcp_retransmits_total", "Total TCP retransmits"),
            &["test_id"],
        )
        .unwrap();

        let active_tests = IntGauge::with_opts(Opts::new(
            "xfr_active_tests",
            "Number of currently running tests",
        ))
        .unwrap();

        Self {
            bytes_total,
            throughput_mbps,
            tests_total,
            test_duration_seconds,
            stream_bytes_total,
            stream_throughput_mbps,
            stream_retransmits,
            tcp_rtt_microseconds,
            tcp_retransmits_total,
            active_tests,
        }
    }
}

#[cfg(feature = "prometheus")]
static METRICS: Lazy<XfrMetrics> = Lazy::new(XfrMetrics::new);

#[cfg(feature = "prometheus")]
pub struct MetricsServer {
    port: u16,
}

#[cfg(feature = "prometheus")]
impl MetricsServer {
    pub fn new(port: u16) -> Self {
        Self { port }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let addr: SocketAddr = format!("0.0.0.0:{}", self.port).parse()?;
        let listener = TcpListener::bind(addr).await?;
        info!("Prometheus metrics available at http://{}/metrics", addr);

        let listener = Arc::new(listener);
        accept_loop(move || {
            let listener = listener.clone();
            async move { listener.accept().await.map(|(stream, _)| stream) }
        })
        .await
    }
}

/// Maximum metrics connections served at once. Further connections wait in
/// the kernel accept backlog until a slot frees up.
#[cfg(feature = "prometheus")]
const MAX_CONNECTIONS: usize = 64;

/// Pause after an accept error (e.g. EMFILE) so a persistent error does not
/// spin the loop.
#[cfg(feature = "prometheus")]
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);

/// Accept and serve connections forever. Accept errors are logged and retried
/// after a short backoff instead of ending the server.
#[cfg(feature = "prometheus")]
async fn accept_loop<S, F, Fut>(mut accept: F) -> !
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<S>>,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut failing = false;
    loop {
        let permit = slots
            .clone()
            .acquire_owned()
            .await
            .expect("metrics connection semaphore is never closed");
        let stream = match accept().await {
            Ok(stream) => {
                failing = false;
                stream
            }
            Err(err) => {
                // Warn once per run of consecutive errors; repeats go to debug.
                if failing {
                    debug!("Prometheus metrics accept error: {err}");
                } else {
                    warn!("Prometheus metrics accept error, retrying: {err}");
                }
                failing = true;
                tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                continue;
            }
        };
        tokio::spawn(async move {
            if let Err(err) = serve_connection(stream).await {
                debug!("Prometheus metrics connection error: {err:?}");
            }
            drop(permit);
        });
    }
}

#[cfg(feature = "prometheus")]
async fn serve_connection<T>(stream: T) -> anyhow::Result<()>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let service = service_fn(handle_request);
    tokio::time::timeout(
        Duration::from_secs(10),
        hyper::server::conn::http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(Duration::from_secs(5))
            .keep_alive(false)
            .max_buf_size(16 * 1024)
            .serve_connection(TokioIo::new(stream), service),
    )
    .await??;
    Ok(())
}

#[cfg(feature = "prometheus")]
async fn handle_request(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    match req.uri().path() {
        "/metrics" => {
            let encoder = TextEncoder::new();
            let metric_families = prometheus::gather();
            let mut buffer = Vec::new();
            encoder.encode(&metric_families, &mut buffer).unwrap();

            Ok(Response::builder()
                .status(200)
                .header("Content-Type", encoder.format_type())
                .body(Full::new(Bytes::from(buffer)))
                .unwrap())
        }
        "/health" => Ok(Response::builder()
            .status(200)
            .body(Full::new(Bytes::from("OK")))
            .unwrap()),
        _ => Ok(Response::builder()
            .status(404)
            .body(Full::new(Bytes::from("Not Found")))
            .unwrap()),
    }
}

/// Register all metrics with the default prometheus registry.
/// Call this once at startup before spawning the metrics server.
#[cfg(feature = "prometheus")]
pub fn register_metrics() {
    let m = &*METRICS;

    // Register all metrics (ignore errors if already registered)
    let _ = prometheus::register(Box::new(m.bytes_total.clone()));
    let _ = prometheus::register(Box::new(m.throughput_mbps.clone()));
    let _ = prometheus::register(Box::new(m.tests_total.clone()));
    let _ = prometheus::register(Box::new(m.test_duration_seconds.clone()));
    let _ = prometheus::register(Box::new(m.stream_bytes_total.clone()));
    let _ = prometheus::register(Box::new(m.stream_throughput_mbps.clone()));
    let _ = prometheus::register(Box::new(m.stream_retransmits.clone()));
    let _ = prometheus::register(Box::new(m.tcp_rtt_microseconds.clone()));
    let _ = prometheus::register(Box::new(m.tcp_retransmits_total.clone()));
    let _ = prometheus::register(Box::new(m.active_tests.clone()));
}

/// Called when a new test starts
#[cfg(feature = "prometheus")]
pub fn on_test_start() {
    METRICS.active_tests.inc();
}

/// Called when a test aborts or errors before completion.
/// Only decrements the active test gauge and updates live stats;
/// does NOT increment `tests_total` or counters.
#[cfg(feature = "prometheus")]
pub fn on_test_aborted(stats: &TestStats) {
    let m = &*METRICS;
    m.active_tests.dec();
    update_metrics(stats);
}

/// Called when a test completes. Updates all relevant metrics.
#[cfg(feature = "prometheus")]
pub fn on_test_complete(stats: &TestStats) {
    let m = &*METRICS;

    m.active_tests.dec();
    m.tests_total.inc();

    let duration_secs = stats.elapsed_ms() as f64 / 1000.0;
    m.test_duration_seconds.observe(duration_secs);

    // Update gauges one final time
    update_metrics(stats);

    // Update counters (only at completion to avoid overcounting)
    update_counters(stats);
}

/// Update metrics with current test stats for live monitoring.
/// Only updates gauges (instantaneous values), not counters (cumulative values).
/// Counters are only updated at test completion via on_test_complete().
#[cfg(feature = "prometheus")]
pub fn update_metrics(stats: &TestStats) {
    let m = &*METRICS;
    let test_id = &stats.test_id;

    // Update throughput gauge (instantaneous value)
    let total_bytes = stats.total_bytes();
    let duration_ms = stats.elapsed_ms();
    if duration_ms > 0 {
        let throughput = (total_bytes as f64 * 8.0) / (duration_ms as f64 / 1000.0) / 1_000_000.0;
        m.throughput_mbps.set(throughput);
    }

    // Per-stream gauges (instantaneous values)
    for stream in &stats.streams {
        let stream_id = stream.stream_id.to_string();
        let labels = &[test_id.as_str(), stream_id.as_str()];

        let stream_throughput = stream.throughput_mbps();
        m.stream_throughput_mbps
            .with_label_values(labels)
            .set(stream_throughput);
    }

    // TCP RTT gauge (instantaneous value)
    if let Some(ref tcp_info) = stats.get_tcp_info() {
        m.tcp_rtt_microseconds
            .with_label_values(&[test_id])
            .set(tcp_info.rtt_us as f64);
    }
}

/// Update counters with final test stats. Called only once per test at completion.
#[cfg(feature = "prometheus")]
fn update_counters(stats: &TestStats) {
    let m = &*METRICS;
    let test_id = &stats.test_id;

    // Aggregate byte counter
    let total_bytes = stats.total_bytes();
    m.bytes_total.inc_by(total_bytes as f64);

    // Per-stream counters
    for stream in &stats.streams {
        let stream_id = stream.stream_id.to_string();
        let labels = &[test_id.as_str(), stream_id.as_str()];

        let bytes = stream.total_bytes();
        m.stream_bytes_total
            .with_label_values(labels)
            .inc_by(bytes as f64);

        let retransmits = stream.retransmits();
        m.stream_retransmits
            .with_label_values(labels)
            .inc_by(retransmits as f64);
    }

    // TCP retransmits counter
    if let Some(ref tcp_info) = stats.get_tcp_info() {
        m.tcp_retransmits_total
            .with_label_values(&[test_id])
            .inc_by(tcp_info.retransmits as f64);
    }
}

#[cfg(all(test, feature = "prometheus"))]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn fragmented_requests_respond_and_close() {
        register_metrics();
        for (path, body) in [("/health", "OK"), ("/metrics", "xfr_bytes_total")] {
            let (mut client, server) = tokio::io::duplex(1);
            let task = tokio::spawn(serve_connection(server));
            client
                .write_all(format!("GET {path} HTTP/1.1\r\nHo").as_bytes())
                .await
                .unwrap();
            tokio::task::yield_now().await;
            client.write_all(b"st: localhost\r\n\r\n").await.unwrap();
            let mut response = String::new();
            tokio::time::timeout(Duration::from_secs(1), client.read_to_string(&mut response))
                .await
                .unwrap()
                .unwrap();
            assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
            assert!(response.contains("connection: close\r\n"));
            assert!(response.contains(body));
            task.await.unwrap().unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn header_deadline_is_not_reset_by_trickling_bytes() {
        let (mut client, server) = tokio::io::duplex(4096);
        let task = tokio::spawn(serve_connection(server));
        client.write_all(b"GET /health HTTP/1.1\r\n").await.unwrap();
        tokio::task::yield_now().await;
        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(1)).await;
            client.write_all(b"X").await.unwrap();
            tokio::task::yield_now().await;
            assert!(!task.is_finished());
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(task.is_finished());
        let error = task.await.unwrap().unwrap_err();
        assert!(error.downcast_ref::<hyper::Error>().unwrap().is_timeout());
    }

    #[tokio::test(start_paused = true)]
    async fn connection_deadline_bounds_stalled_response_writes() {
        let (mut client, server) = tokio::io::duplex(64);
        let task = tokio::spawn(serve_connection(server));
        client
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(9)).await;
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(task.is_finished());
        let error = task.await.unwrap().unwrap_err();
        assert!(
            error
                .downcast_ref::<tokio::time::error::Elapsed>()
                .is_some()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn accept_error_does_not_end_loop() {
        let (mut client, server) = tokio::io::duplex(4096);
        // Popped from the end: one accept error, then a real connection.
        let mut results = vec![Ok(server), Err(std::io::Error::other("EMFILE"))];
        let loop_task = tokio::spawn(accept_loop(move || {
            let next = results.pop();
            async move {
                match next {
                    Some(result) => result,
                    None => std::future::pending().await,
                }
            }
        }));
        client
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(1), client.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(!loop_task.is_finished());
        loop_task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn connection_cap_defers_accept_until_a_slot_frees() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (mut clients, mut servers): (Vec<_>, Vec<_>) =
            (0..=MAX_CONNECTIONS).map(|_| tokio::io::duplex(64)).unzip();
        let accepts = Arc::new(AtomicUsize::new(0));
        let counter = accepts.clone();
        let loop_task = tokio::spawn(accept_loop(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let next = servers.pop();
            async move {
                match next {
                    Some(stream) => Ok(stream),
                    None => std::future::pending().await,
                }
            }
        }));

        // Every slot holds a stalled connection, well inside the 5 s header
        // deadline, so the loop must not accept again.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(accepts.load(Ordering::SeqCst), MAX_CONNECTIONS);

        // Closing an accepted connection frees its slot.
        drop(clients.pop());
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(accepts.load(Ordering::SeqCst), MAX_CONNECTIONS + 1);
        loop_task.abort();
    }
}
