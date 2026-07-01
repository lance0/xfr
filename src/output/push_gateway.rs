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
    pub fn new(url: String) -> anyhow::Result<Self> {
        Self::new_with_timeout(url, std::time::Duration::from_secs(30))
    }

    fn new_with_timeout(url: String, timeout: std::time::Duration) -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build push gateway HTTP client: {e}"))?;

        Ok(Self { url, client })
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

        /// Write # HELP/# TYPE headers once per metric family, then any samples.
        fn write_family_header(output: &mut String, name: &str, help: &str, metric_type: &str) {
            output.push_str(&format!("# HELP {} {}\n", name, help));
            output.push_str(&format!("# TYPE {} {}\n", name, metric_type));
        }

        /// Write a single sample (gauge or counter) for an aggregate metric.
        fn write_sample(output: &mut String, name: &str, value: f64) {
            output.push_str(&format!("{} {}\n", name, value));
        }

        // Add labels for the test
        output.push_str(&format!("# Test ID: {}\n", test_id));

        // Aggregate metrics
        write_family_header(
            &mut output,
            "xfr_bytes_total",
            "Total bytes transferred",
            "counter",
        );
        write_sample(&mut output, "xfr_bytes_total", bytes_total as f64);

        write_family_header(
            &mut output,
            "xfr_throughput_mbps",
            "Average throughput in Mbps",
            "gauge",
        );
        write_sample(&mut output, "xfr_throughput_mbps", throughput_mbps);

        write_family_header(
            &mut output,
            "xfr_duration_seconds",
            "Test duration in seconds",
            "gauge",
        );
        write_sample(&mut output, "xfr_duration_seconds", duration_secs);

        // Per-stream metrics: headers emitted once, samples once per stream.
        if !stats.streams.is_empty() {
            write_family_header(
                &mut output,
                "xfr_stream_bytes_total",
                "Per-stream total bytes transferred",
                "counter",
            );
            write_family_header(
                &mut output,
                "xfr_stream_throughput_mbps",
                "Per-stream average throughput in Mbps",
                "gauge",
            );
            write_family_header(
                &mut output,
                "xfr_stream_retransmits_total",
                "Per-stream TCP retransmits",
                "counter",
            );

            for stream in &stats.streams {
                let stream_id = stream.stream_id;
                let stream_bytes = stream.total_bytes() as f64;
                let stream_throughput = stream.throughput_mbps();
                let stream_retransmits = stream.retransmits() as f64;

                output.push_str(&format!(
                    "xfr_stream_bytes_total{{test_id=\"{}\",stream_id=\"{}\"}} {}\n",
                    test_id, stream_id, stream_bytes
                ));
                output.push_str(&format!(
                    "xfr_stream_throughput_mbps{{test_id=\"{}\",stream_id=\"{}\"}} {}\n",
                    test_id, stream_id, stream_throughput
                ));
                output.push_str(&format!(
                    "xfr_stream_retransmits_total{{test_id=\"{}\",stream_id=\"{}\"}} {}\n",
                    test_id, stream_id, stream_retransmits
                ));
            }
        }

        // Aggregate TCP info if available
        if let Some(tcp_info) = stats.final_local_tcp_info() {
            write_family_header(
                &mut output,
                "xfr_tcp_rtt_microseconds",
                "TCP round-trip time in microseconds",
                "gauge",
            );
            write_sample(
                &mut output,
                "xfr_tcp_rtt_microseconds",
                tcp_info.rtt_us as f64,
            );

            write_family_header(
                &mut output,
                "xfr_tcp_retransmits_total",
                "TCP retransmits",
                "counter",
            );
            write_sample(
                &mut output,
                "xfr_tcp_retransmits_total",
                tcp_info.retransmits as f64,
            );

            write_family_header(
                &mut output,
                "xfr_tcp_cwnd_bytes",
                "TCP congestion window in bytes",
                "gauge",
            );
            write_sample(&mut output, "xfr_tcp_cwnd_bytes", tcp_info.cwnd as f64);
        }

        // Aggregate UDP stats if available
        let udp_stats_vec = stats.udp_stats.lock();
        if !udp_stats_vec.is_empty() {
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

            write_family_header(
                &mut output,
                "xfr_udp_packets_sent",
                "UDP packets sent",
                "counter",
            );
            write_sample(&mut output, "xfr_udp_packets_sent", total_sent as f64);

            write_family_header(
                &mut output,
                "xfr_udp_packets_received",
                "UDP packets received",
                "counter",
            );
            write_sample(
                &mut output,
                "xfr_udp_packets_received",
                total_received as f64,
            );

            write_family_header(
                &mut output,
                "xfr_udp_packets_lost",
                "UDP packets lost",
                "counter",
            );
            write_sample(&mut output, "xfr_udp_packets_lost", total_lost as f64);

            write_family_header(
                &mut output,
                "xfr_udp_jitter_ms",
                "UDP jitter in milliseconds",
                "gauge",
            );
            write_sample(&mut output, "xfr_udp_jitter_ms", max_jitter);

            write_family_header(
                &mut output,
                "xfr_udp_lost_percent",
                "UDP packet loss percentage",
                "gauge",
            );
            write_sample(&mut output, "xfr_udp_lost_percent", lost_percent);
        }

        output
    }
}

/// Push metrics to the gateway if configured
pub async fn maybe_push_metrics(push_gateway_url: &Option<String>, stats: &TestStats) {
    if let Some(url) = push_gateway_url {
        match PushGatewayClient::new(url.clone()) {
            Ok(client) => client.push_test_metrics(stats).await,
            Err(e) => {
                error!("Failed to initialize push gateway client: {}", e);
            }
        }
    } else {
        debug!("No push gateway configured, skipping metrics push");
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tokio::net::TcpListener;

    use crate::output::push_gateway::PushGatewayClient;
    use crate::protocol::{TcpInfoSnapshot, UdpStats};
    use crate::stats::TestStats;

    #[tokio::test]
    async fn test_push_with_retry_honors_timeout() {
        // Bind a socket that accepts but never responds.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move {
            if let Ok((socket, _)) = listener.accept().await {
                // Hold the socket open but never respond, so reqwest waits
                // for the configured timeout instead of failing fast.
                let _hold = socket;
                std::future::pending::<()>().await;
            }
        });

        let url = format!("http://{}/metrics/job/test", addr);
        let client = PushGatewayClient::new_with_timeout(url.clone(), Duration::from_millis(100))
            .expect("client should build with a valid timeout");

        let start = Instant::now();
        let result = client.push_with_retry(&url, "test_body").await;
        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "expected a timeout error, got {:?}",
            result
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "timeout should fire quickly, but took {:?}",
            elapsed
        );
    }

    #[test]
    fn test_format_metrics_headers_once_per_family() {
        let stats = TestStats::new("test".to_string(), 2);
        stats.streams[0].add_bytes_sent(1_000_000);
        stats.streams[1].add_bytes_sent(2_000_000);
        stats.add_tcp_info(TcpInfoSnapshot {
            retransmits: 5,
            rtt_us: 1200,
            rtt_var_us: 300,
            cwnd: 64 * 1024,
            bytes_acked: None,
        });
        stats.add_udp_stats(UdpStats {
            packets_sent: 100,
            packets_received: 95,
            lost: 5,
            lost_percent: 5.0,
            jitter_ms: 1.5,
            jitter_max_ms: Some(3.0),
            out_of_order: 0,
            packet_size: Some(1400),
        });

        let client =
            PushGatewayClient::new("http://example.com".to_string()).expect("client should build");
        let metrics = client.format_metrics(&stats);

        assert_eq!(
            metrics
                .matches("# TYPE xfr_stream_bytes_total counter")
                .count(),
            1,
            "# TYPE should be emitted once per metric family"
        );
        assert_eq!(
            metrics
                .matches("# TYPE xfr_stream_throughput_mbps gauge")
                .count(),
            1
        );
        assert_eq!(
            metrics
                .matches("# TYPE xfr_stream_retransmits_total counter")
                .count(),
            1
        );

        // Assert that each per-stream family has one sample per stream.
        // `matches("xfr_stream_bytes_total{"...)` would be more precise; here we count
        // occurrences of the metric name and assert at least 2 samples.
        let bytes_count = metrics.matches("xfr_stream_bytes_total{").count();
        assert_eq!(bytes_count, 2, "expected one sample per stream");
        let throughput_count = metrics.matches("xfr_stream_throughput_mbps{").count();
        assert_eq!(throughput_count, 2);
        let retransmits_count = metrics.matches("xfr_stream_retransmits_total{").count();
        assert_eq!(retransmits_count, 2);

        // Headers for aggregate metric families should also be present once.
        assert!(metrics.contains("# HELP xfr_bytes_total Total bytes transferred"));
        assert_eq!(metrics.matches("# TYPE xfr_bytes_total counter").count(), 1);
        assert_eq!(
            metrics
                .matches("# TYPE xfr_udp_packets_sent counter")
                .count(),
            1
        );
    }
}
