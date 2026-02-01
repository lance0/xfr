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
use prometheus::{Encoder, TextEncoder};
#[cfg(feature = "prometheus")]
use tokio::net::TcpListener;
#[cfg(feature = "prometheus")]
use tracing::info;

#[cfg(feature = "prometheus")]
use crate::stats::TestStats;

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

#[cfg(feature = "prometheus")]
pub fn register_metrics() {
    use prometheus::{register_counter, register_gauge, register_histogram};

    // These will fail silently if already registered
    let _ = register_counter!("xfr_bytes_total", "Total bytes transferred");
    let _ = register_gauge!("xfr_throughput_mbps", "Current throughput in Mbps");
    let _ = register_counter!("xfr_tests_total", "Total number of tests run");
    let _ = register_histogram!("xfr_test_duration_seconds", "Test duration in seconds");
}

#[cfg(feature = "prometheus")]
pub fn update_metrics(stats: &TestStats) {
    let bytes_total = prometheus::Counter::with_opts(prometheus::Opts::new(
        "xfr_bytes_total",
        "Total bytes transferred",
    ))
    .unwrap();

    // Try to register, ignore if already registered
    let _ = prometheus::register(Box::new(bytes_total.clone()));
    bytes_total.inc_by(stats.total_bytes() as f64);
}
