//! Prometheus Push Gateway client
//!
//! Pushes test metrics to a Prometheus Push Gateway at test completion.

use reqwest::Client;
use tracing::{debug, error, info, warn};

use crate::stats::TestStats;

/// Push Gateway client for sending test metrics
pub struct PushGatewayClient {
    url: String,
    client: Client,
}

impl PushGatewayClient {
    /// Create a new Push Gateway client
    pub fn new(url: String) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self { url, client }
    }

    /// Push test metrics to the gateway
    ///
    /// # Arguments
    /// * `stats` - Test statistics to push
    /// * `test_id` - Unique identifier for this test (used as job label)
    pub async fn push_test_metrics(&self, stats: &TestStats) {
        let job_name = format!("xfr_test_{}", stats.test_id);
        let url = format!("{}/metrics/job/{}", self.url, job_name);

        let metrics = self.format_metrics(stats);

        // Retry logic: try up to 3 times with exponential backoff
        for attempt in 1..=3 {
            match self.push_with_retry(&url, &metrics).await {
                Ok(_) => {
                    info!("Pushed metrics to {} (attempt {})", url, attempt);
                    return;
                }
                Err(e) => {
                    if attempt < 3 {
                        warn!(
                            "Failed to push metrics (attempt {}): {}. Retrying...",
                            attempt, e
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(100 * attempt as u64))
                            .await;
                    } else {
                        error!("Failed to push metrics after 3 attempts: {}", e);
                    }
                }
            }
        }
    }

    /// Push metrics with a single HTTP POST request
    async fn push_with_retry(&self, url: &str, metrics: &str) -> anyhow::Result<()> {
        let response = self
            .client
            .post(url)
            .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
            .body(metrics.to_string())
            .send()
            .await?;

        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            let body = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Push gateway returned {}: {}",
                status,
                body
            ))
        }
    }

    /// Format test stats as Prometheus text format
    fn format_metrics(&self, stats: &TestStats) -> String {
        let mut output = String::new();
        let test_id = &stats.test_id;
        let elapsed_ms = stats.elapsed_ms();
        let duration_secs = elapsed_ms as f64 / 1000.0;
        let bytes_total = stats.total_bytes();
        let throughput_mbps = if elapsed_ms > 0 {
            (bytes_total as f64 * 8.0) / duration_secs / 1_000_000.0
        } else {
            0.0
        };

        // Helper to write metric
        let write_metric = |output: &mut String, name: &str, value: f64| {
            output.push_str(&format!("# TYPE {} gauge\n{} {}\n", name, name, value));
        };

        // Helper to write counter
        let write_counter = |output: &mut String, name: &str, value: f64| {
            output.push_str(&format!("# TYPE {} counter\n{} {}\n", name, name, value));
        };

        // Add labels for the test
        output.push_str(&format!("# Test ID: {}\n", test_id));

        // Aggregate metrics
        write_counter(&mut output, "xfr_bytes_total", bytes_total as f64);
        write_metric(&mut output, "xfr_throughput_mbps", throughput_mbps);
        write_metric(&mut output, "xfr_duration_seconds", duration_secs);

        // Per-stream metrics
        for stream in &stats.streams {
            let stream_id = stream.stream_id;
            let stream_bytes = stream.total_bytes() as f64;
            let stream_throughput = stream.throughput_mbps();
            let stream_retransmits = stream.retransmits() as f64;

            output.push_str(&format!(
                "# TYPE xfr_stream_bytes_total counter\nxfr_stream_bytes_total{{test_id=\"{}\",stream_id=\"{}\"}} {}\n",
                test_id, stream_id, stream_bytes
            ));
            output.push_str(&format!(
                "# TYPE xfr_stream_throughput_mbps gauge\nxfr_stream_throughput_mbps{{test_id=\"{}\",stream_id=\"{}\"}} {}\n",
                test_id, stream_id, stream_throughput
            ));
            output.push_str(&format!(
                "# TYPE xfr_stream_retransmits_total counter\nxfr_stream_retransmits_total{{test_id=\"{}\",stream_id=\"{}\"}} {}\n",
                test_id, stream_id, stream_retransmits
            ));
        }

        // Aggregate TCP info if available
        let tcp_infos = stats.tcp_info.lock();
        if !tcp_infos.is_empty() {
            // Use the last snapshot as the most recent state
            if let Some(tcp_info) = tcp_infos.last() {
                write_metric(
                    &mut output,
                    "xfr_tcp_rtt_microseconds",
                    tcp_info.rtt_us as f64,
                );
                write_counter(
                    &mut output,
                    "xfr_tcp_retransmits_total",
                    tcp_info.retransmits as f64,
                );
                write_metric(&mut output, "xfr_tcp_cwnd_bytes", tcp_info.cwnd as f64);
            }
        }

        // Aggregate UDP stats if available
        let udp_stats_vec = stats.udp_stats.lock();
        if !udp_stats_vec.is_empty() {
            // Aggregate all UDP stats
            let mut total_sent = 0u64;
            let mut total_received = 0u64;
            let mut total_lost = 0u64;
            let mut max_jitter = 0.0f64;
            for udp in udp_stats_vec.iter() {
                total_sent += udp.packets_sent;
                total_received += udp.packets_received;
                total_lost += udp.lost;
                max_jitter = max_jitter.max(udp.jitter_ms);
            }
            let lost_percent = if total_sent > 0 {
                (total_lost as f64 / total_sent as f64) * 100.0
            } else {
                0.0
            };
            write_counter(&mut output, "xfr_udp_packets_sent", total_sent as f64);
            write_counter(
                &mut output,
                "xfr_udp_packets_received",
                total_received as f64,
            );
            write_counter(&mut output, "xfr_udp_packets_lost", total_lost as f64);
            write_metric(&mut output, "xfr_udp_jitter_ms", max_jitter);
            write_metric(&mut output, "xfr_udp_lost_percent", lost_percent);
        }

        output
    }
}

/// Push metrics to the gateway if configured
pub async fn maybe_push_metrics(push_gateway_url: &Option<String>, stats: &TestStats) {
    if let Some(url) = push_gateway_url {
        let client = PushGatewayClient::new(url.clone());
        client.push_test_metrics(stats).await;
    } else {
        debug!("No push gateway configured, skipping metrics push");
    }
}
