//! Prometheus metrics endpoint

#[cfg(feature = "prometheus")]
use std::net::SocketAddr;

#[cfg(feature = "prometheus")]
use http_body_util::Full;
#[cfg(feature = "prometheus")]
use hyper::body::Bytes;
#[cfg(feature = "prometheus")]
use hyper::service::service_fn;
#[cfg(feature = "prometheus")]
use hyper::{Request, Response};
#[cfg(feature = "prometheus")]
use hyper_util::rt::TokioIo;
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
use tracing::info;

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

        loop {
            let (stream, _) = listener.accept().await?;
            let io = TokioIo::new(stream);

            tokio::spawn(async move {
                let service = service_fn(|req| async move { handle_request(req).await });

                if let Err(err) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await
                {
                    eprintln!("Error serving connection: {:?}", err);
                }
            });
        }
    }
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

/// Called when a test completes. Updates all relevant metrics.
#[cfg(feature = "prometheus")]
pub fn on_test_complete(stats: &TestStats) {
    let m = &*METRICS;

    m.active_tests.dec();
    m.tests_total.inc();

    let duration_secs = stats.elapsed_ms() as f64 / 1000.0;
    m.test_duration_seconds.observe(duration_secs);

    update_metrics(stats);
}

/// Update metrics with current test stats. Can be called during a test for live updates.
#[cfg(feature = "prometheus")]
pub fn update_metrics(stats: &TestStats) {
    let m = &*METRICS;
    let test_id = &stats.test_id;

    // Aggregate stats
    let total_bytes = stats.total_bytes();
    m.bytes_total.inc_by(total_bytes as f64);

    let duration_ms = stats.elapsed_ms();
    if duration_ms > 0 {
        let throughput = (total_bytes as f64 * 8.0) / (duration_ms as f64 / 1000.0) / 1_000_000.0;
        m.throughput_mbps.set(throughput);
    }

    // Per-stream stats
    for stream in &stats.streams {
        let stream_id = stream.stream_id.to_string();
        let labels = &[test_id.as_str(), stream_id.as_str()];

        let bytes = stream.total_bytes();
        m.stream_bytes_total
            .with_label_values(labels)
            .inc_by(bytes as f64);

        let stream_throughput = stream.throughput_mbps();
        m.stream_throughput_mbps
            .with_label_values(labels)
            .set(stream_throughput);

        let retransmits = stream.retransmits();
        m.stream_retransmits
            .with_label_values(labels)
            .inc_by(retransmits as f64);
    }

    // TCP info (use aggregated/first stream info)
    if let Some(ref tcp_info) = stats.get_tcp_info() {
        m.tcp_rtt_microseconds
            .with_label_values(&[test_id])
            .set(tcp_info.rtt_us as f64);

        m.tcp_retransmits_total
            .with_label_values(&[test_id])
            .inc_by(tcp_info.retransmits as f64);
    }
}
