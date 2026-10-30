//! Prometheus Push Gateway client
//!
//! Pushes test metrics to a Prometheus Push Gateway at test completion.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::net::IpAddr;
use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Client;
use tracing::{debug, error, warn};

use crate::protocol::{Direction, Protocol, TcpInfoSnapshot, TestResult, UdpStats};
use crate::stats::TestStats;

/// Validate operator labels without permitting overrides of exporter-owned dimensions.
pub fn validate_push_labels(labels: &BTreeMap<String, String>) -> anyhow::Result<()> {
    for name in labels.keys() {
        anyhow::ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .enumerate()
                    .all(|(i, c)| c.is_ascii_alphabetic()
                        || c == b'_'
                        || (i > 0 && c.is_ascii_digit()))
                && !name.starts_with("__")
                && !name.starts_with("U__")
                && ![
                    "job",
                    "role",
                    "server",
                    "client",
                    "protocol",
                    "test_direction",
                    "direction",
                    "view",
                    "test_id",
                    "stream_id"
                ]
                .contains(&name.as_str()),
            "invalid or reserved push label name {name:?}"
        );
    }
    Ok(())
}

/// Merge CLI labels over config values; duplicate CLI keys are errors.
pub fn merge_push_labels(
    mut labels: BTreeMap<String, String>,
    cli: &[String],
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut overrides = BTreeMap::new();
    for label in cli {
        let (name, value) = label
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("push label must be key=value: {label:?}"))?;
        anyhow::ensure!(
            overrides
                .insert(name.to_string(), value.to_string())
                .is_none(),
            "duplicate push label {name:?}"
        );
    }
    labels.extend(overrides);
    validate_push_labels(&labels)?;
    Ok(labels)
}

/// Push Gateway client for sending test metrics
#[derive(Clone)]
pub struct PushGatewayClient {
    url: reqwest::Url,
    client: Client,
    grouping_labels: Vec<(String, String)>,
    test: Option<(Protocol, Direction)>,
    labels: BTreeMap<String, String>,
    job: Option<String>,
}

impl PushGatewayClient {
    /// Create a new Push Gateway client
    pub fn new(url: String) -> anyhow::Result<Self> {
        Self::new_with_timeout(url, std::time::Duration::from_secs(30))
    }

    fn new_with_timeout(url: String, timeout: std::time::Duration) -> anyhow::Result<Self> {
        let url = reqwest::Url::parse(&url)?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "push gateway URL must use HTTP or HTTPS and include a host"
        );
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build push gateway HTTP client: {e}"))?;

        Ok(Self {
            url,
            client,
            grouping_labels: Vec::new(),
            test: None,
            labels: BTreeMap::new(),
            job: None,
        })
    }

    /// Supply run context for client-relative directional UDP snapshots.
    /// Context-free callers retain the legacy metric payload.
    /// Use finalized client results with `push_test_result`, or server-local
    /// counters with `push_test_metrics`.
    pub fn with_test(mut self, protocol: Protocol, direction: Direction) -> Self {
        self.test = Some((protocol, direction));
        self
    }

    /// Attach portable operator labels to either export role.
    pub fn with_labels(mut self, labels: BTreeMap<String, String>) -> anyhow::Result<Self> {
        validate_push_labels(&labels)?;
        self.labels = labels;
        Ok(self)
    }

    /// Select bounded aggregate client snapshots under a stable job.
    pub fn with_job(mut self, job: String) -> anyhow::Result<Self> {
        anyhow::ensure!(!job.trim().is_empty(), "push job must not be empty");
        self.job = Some(job);
        Ok(self)
    }

    /// Validate bounded-client context before starting a measurement.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.job.is_some() {
            anyhow::ensure!(
                self.labels
                    .get("instance")
                    .is_some_and(|v| !v.trim().is_empty()),
                "stable push job requires a nonempty instance label"
            );
            anyhow::ensure!(
                self.test.is_some()
                    && self
                        .grouping_labels
                        .iter()
                        .any(|(k, v)| k == "role" && v == "client")
                    && self.grouping_labels.iter().any(|(k, _)| k == "server"),
                "stable push job requires client target and test context"
            );
        }
        Ok(())
    }

    pub(crate) fn with_endpoint(mut self, name: &str, value: &str, role: &str) -> Self {
        self.grouping_labels = vec![("role".into(), role.into()), (name.into(), value.into())];
        self
    }

    /// Identify the client push by the target host and listening port.
    pub fn with_server(self, host: &str, port: u16) -> Self {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let server = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        self.with_endpoint("server", &server, "client")
    }

    /// Push test metrics to the gateway
    /// Directional metrics interpret these as server-local send/receive counters.
    ///
    /// # Arguments
    /// * `stats` - Test statistics to push
    pub async fn push_test_metrics(&self, stats: &TestStats) {
        if self.job.is_some() {
            error!("Stable push jobs require finalized client results");
            return;
        }
        let metrics = self.format_metrics(stats);
        if let Err(error) = self.push_metrics(&stats.test_id, &metrics).await {
            error!("Failed to push metrics: {error:#}");
        }
    }

    /// Push the finalized result, including any client-side receiver corrections.
    /// Delivery has a five-second total deadline, including retries.
    pub async fn push_test_result(&self, result: &TestResult) -> anyhow::Result<()> {
        self.validate()?;
        let metrics = if self.job.is_some() {
            self.format_snapshot(result)
        } else {
            self.format_result(result)
        };
        tokio::time::timeout(
            Duration::from_secs(5),
            self.push_metrics(&result.id, &metrics),
        )
        .await
        .map_err(|_| anyhow::anyhow!("metrics delivery timed out after 5 seconds"))?
    }

    fn format_result(&self, result: &TestResult) -> String {
        let mut metrics = Self::format_samples(
            &result.id,
            result.duration_ms,
            result.bytes_total,
            result.throughput_mbps,
            result
                .streams
                .iter()
                .map(|s| (s.id, s.bytes, s.throughput_mbps, s.retransmits.unwrap_or(0))),
            result.tcp_info.as_ref(),
            result.udp_stats.iter(),
        );
        if let Some((Protocol::Udp, direction)) = self.test {
            let samples = Self::result_directions(result, direction);
            append_udp_directions(&mut metrics, &samples);
        }
        metrics
    }

    fn result_directions(result: &TestResult, direction: Direction) -> Vec<DirectionalUdp<'_>> {
        match direction {
            Direction::Upload | Direction::Download => vec![DirectionalUdp {
                direction: if direction == Direction::Upload {
                    "upload"
                } else {
                    "download"
                },
                view: "receiver",
                bytes: Some(result.bytes_total),
                throughput_mbps: Some(result.throughput_mbps),
                quality: result.udp_stats.as_ref(),
            }],
            Direction::Bidir => vec![
                DirectionalUdp {
                    direction: "upload",
                    view: "receiver",
                    bytes: result.bytes_sent,
                    throughput_mbps: result.throughput_send_mbps,
                    quality: result.udp_stats_send.as_deref(),
                },
                DirectionalUdp {
                    direction: "download",
                    view: "receiver",
                    bytes: result.bytes_received,
                    throughput_mbps: result.throughput_recv_mbps,
                    quality: result.udp_stats_recv.as_deref(),
                },
            ],
        }
    }

    fn format_snapshot(&self, result: &TestResult) -> String {
        let mut output = String::new();
        snapshot_gauge(&mut output, "bytes", "", result.bytes_total);
        snapshot_gauge(&mut output, "throughput_mbps", "", result.throughput_mbps);
        snapshot_gauge(
            &mut output,
            "duration_seconds",
            "",
            result.duration_ms as f64 / 1000.0,
        );
        snapshot_gauge(&mut output, "streams", "", result.streams.len());
        snapshot_gauge(
            &mut output,
            "tcp_info_available",
            "",
            u8::from(result.tcp_info.is_some()),
        );
        if let Some(tcp) = &result.tcp_info {
            snapshot_gauge(&mut output, "tcp_rtt_microseconds", "", tcp.rtt_us);
            snapshot_gauge(&mut output, "tcp_retransmits", "", tcp.retransmits);
            snapshot_gauge(&mut output, "tcp_cwnd_bytes", "", tcp.cwnd);
        }
        if let Some((Protocol::Udp, direction)) = self.test {
            let mut samples = Self::result_directions(result, direction);
            let flags: Vec<[bool; 3]> = samples
                .iter()
                .map(|sample| {
                    [
                        sample.bytes.is_some() && sample.throughput_mbps.is_some(),
                        sample
                            .quality
                            .is_some_and(|q| q.packets_received > 0 || q.lost > 0),
                        sample.quality.is_some_and(|q| q.packets_received > 0),
                    ]
                })
                .collect();
            // One family at a time: a family's samples must follow its header.
            for (i, (metric, view)) in [
                ("udp_direction_available", ",view=\"receiver\""),
                ("udp_direction_quality_available", ""),
                ("udp_direction_jitter_available", ""),
            ]
            .into_iter()
            .enumerate()
            {
                for (sample, flags) in samples.iter().zip(&flags) {
                    let labels = format!("{{direction=\"{}\"{view}}}", sample.direction);
                    snapshot_gauge(&mut output, metric, &labels, u8::from(flags[i]));
                }
            }
            for (sample, [available, ..]) in samples.iter_mut().zip(flags) {
                if !available {
                    sample.bytes = None;
                    sample.throughput_mbps = None;
                }
            }
            append_udp_directions_named(&mut output, &samples, "xfr_last_test_udp_direction");
        }
        output
    }

    fn push_url(&self, test_id: &str) -> anyhow::Result<reqwest::Url> {
        let mut url = self.url.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|()| anyhow::anyhow!("invalid push gateway URL"))?;
            path.pop_if_empty().push("metrics");
            let mut labels = self.grouping_labels.clone();
            if self.job.is_some() {
                let (protocol, direction) = self.test.expect("validated snapshot context");
                labels.push(("protocol".into(), protocol.to_string().to_ascii_lowercase()));
                labels.push((
                    "test_direction".into(),
                    match direction {
                        Direction::Upload => "upload",
                        Direction::Download => "download",
                        Direction::Bidir => "bidir",
                    }
                    .into(),
                ));
            }
            labels.extend(self.labels.iter().map(|(k, v)| (k.clone(), v.clone())));
            let job = self
                .job
                .clone()
                .unwrap_or_else(|| format!("xfr_test_{test_id}"));
            for (name, value) in std::iter::once(("job", job))
                .chain(labels.iter().map(|(k, v)| (k.as_str(), v.clone())))
            {
                if value.is_empty() || value.contains('/') || matches!(value.as_str(), "." | "..") {
                    path.push(&format!("{name}@base64"));
                    path.push(&if value.is_empty() {
                        "=".into()
                    } else {
                        URL_SAFE_NO_PAD.encode(value)
                    });
                } else {
                    path.push(name).push(&value);
                }
            }
        }
        Ok(url)
    }

    async fn push_metrics(&self, test_id: &str, metrics: &str) -> anyhow::Result<()> {
        let url = self.push_url(test_id)?;

        // Retry up to three times, with the caller bounding the full delivery.
        for attempt in 1..=3 {
            match self.push_with_retry(url.as_str(), metrics).await {
                Ok(_) => {
                    debug!("Pushed metrics (attempt {})", attempt);
                    return Ok(());
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
                        return Err(e.context("metrics delivery failed after 3 attempts"));
                    }
                }
            }
        }
        unreachable!("the final attempt always returns")
    }

    /// Send one request; the caller owns retries.
    async fn push_with_retry(&self, url: &str, metrics: &str) -> anyhow::Result<()> {
        let response = self
            .client
            .request(
                if self.job.is_some() {
                    reqwest::Method::PUT
                } else {
                    reqwest::Method::POST
                },
                url,
            )
            .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
            .body(metrics.to_string())
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;

        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Push gateway returned {}", status))
        }
    }

    /// Format test stats as Prometheus text format
    fn format_metrics(&self, stats: &TestStats) -> String {
        let elapsed_ms = stats.elapsed_ms();
        let duration_secs = elapsed_ms as f64 / 1000.0;
        let bytes_total = stats.total_bytes();
        let throughput_mbps = if elapsed_ms > 0 {
            (bytes_total as f64 * 8.0) / duration_secs / 1_000_000.0
        } else {
            0.0
        };

        let mut metrics = Self::format_samples(
            &stats.test_id,
            elapsed_ms,
            bytes_total,
            throughput_mbps,
            stats.streams.iter().map(|s| {
                (
                    s.stream_id,
                    s.total_bytes(),
                    s.throughput_mbps(),
                    s.retransmits(),
                )
            }),
            stats.final_local_tcp_info().as_ref(),
            stats.udp_stats.lock().iter(),
        );
        if let Some((Protocol::Udp, direction)) = self.test {
            let quality = stats.aggregate_udp_receiver_stats();
            let throughput = |bytes: u64| {
                if elapsed_ms > 0 {
                    bytes as f64 * 8.0 / duration_secs / 1_000_000.0
                } else {
                    0.0
                }
            };
            let mut samples = Vec::new();
            if direction != Direction::Download {
                let bytes = stats.total_bytes_received();
                samples.push(DirectionalUdp {
                    direction: "upload",
                    view: "receiver",
                    bytes: Some(bytes),
                    throughput_mbps: Some(throughput(bytes)),
                    quality: quality.as_ref(),
                });
            }
            if direction != Direction::Upload {
                let bytes = stats.total_bytes_sent();
                samples.push(DirectionalUdp {
                    direction: "download",
                    view: "sender",
                    bytes: Some(bytes),
                    throughput_mbps: Some(throughput(bytes)),
                    quality: None,
                });
            }
            append_udp_directions(&mut metrics, &samples);
        }
        metrics
    }

    fn format_samples<'a>(
        test_id: &str,
        elapsed_ms: u64,
        bytes_total: u64,
        throughput_mbps: f64,
        streams: impl ExactSizeIterator<Item = (u8, u64, f64, u64)>,
        tcp_info: Option<&TcpInfoSnapshot>,
        udp_stats: impl Iterator<Item = &'a UdpStats>,
    ) -> String {
        let mut output = String::new();
        let test_id = test_id
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        let duration_secs = elapsed_ms as f64 / 1000.0;

        /// Write # HELP/# TYPE headers once per metric family, then any samples.
        fn write_family_header(output: &mut String, name: &str, help: &str, metric_type: &str) {
            writeln!(output, "# HELP {} {}", name, help).expect("writing to String cannot fail");
            writeln!(output, "# TYPE {} {}", name, metric_type)
                .expect("writing to String cannot fail");
        }

        /// Write a single sample (gauge or counter) for an aggregate metric.
        fn write_sample(output: &mut String, name: &str, value: f64) {
            writeln!(output, "{} {}", name, value).expect("writing to String cannot fail");
        }

        // Add labels for the test
        writeln!(output, "# Test ID: {}", test_id).expect("writing to String cannot fail");

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
        if streams.len() > 0 {
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

            for (stream_id, bytes, stream_throughput, retransmits) in streams {
                let stream_bytes = bytes as f64;
                let stream_retransmits = retransmits as f64;

                writeln!(
                    output,
                    "xfr_stream_bytes_total{{test_id=\"{}\",stream_id=\"{}\"}} {}",
                    test_id, stream_id, stream_bytes
                )
                .expect("writing to String cannot fail");
                writeln!(
                    output,
                    "xfr_stream_throughput_mbps{{test_id=\"{}\",stream_id=\"{}\"}} {}",
                    test_id, stream_id, stream_throughput
                )
                .expect("writing to String cannot fail");
                writeln!(
                    output,
                    "xfr_stream_retransmits_total{{test_id=\"{}\",stream_id=\"{}\"}} {}",
                    test_id, stream_id, stream_retransmits
                )
                .expect("writing to String cannot fail");
            }
        }

        // Aggregate TCP info if available
        if let Some(tcp_info) = tcp_info {
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
        let mut udp_stats = udp_stats.peekable();
        if udp_stats.peek().is_some() {
            let mut total_sent = 0u64;
            let mut total_received = 0u64;
            let mut total_lost = 0u64;
            let mut max_jitter = 0.0f64;
            for udp in udp_stats {
                total_sent = total_sent.saturating_add(udp.packets_sent);
                total_received = total_received.saturating_add(udp.packets_received);
                total_lost = total_lost.saturating_add(udp.lost);
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

struct DirectionalUdp<'a> {
    direction: &'static str,
    view: &'static str,
    bytes: Option<u64>,
    throughput_mbps: Option<f64>,
    quality: Option<&'a UdpStats>,
}

fn snapshot_gauge(output: &mut String, metric: &str, labels: &str, value: impl std::fmt::Display) {
    let header = format!("# TYPE xfr_last_test_{metric} gauge\n");
    if !output.contains(&header) {
        writeln!(
            output,
            "# HELP xfr_last_test_{metric} Most recently pushed test snapshot {metric}\n{}",
            header.trim_end()
        )
        .expect("writing to String cannot fail");
    }
    writeln!(output, "xfr_last_test_{metric}{labels} {value}")
        .expect("writing to String cannot fail");
}

fn append_udp_directions(output: &mut String, samples: &[DirectionalUdp<'_>]) {
    append_udp_directions_named(output, samples, "xfr_udp_direction");
}

fn append_udp_directions_named(output: &mut String, samples: &[DirectionalUdp<'_>], prefix: &str) {
    for (metric, help) in [
        ("bytes", "UDP bytes for this direction and measurement view"),
        (
            "throughput_mbps",
            "UDP throughput in Mbps for this direction and measurement view",
        ),
        (
            "packets_expected",
            "UDP packets expected from receiver sequence observations",
        ),
        ("packets_received", "UDP packets received"),
        ("packets_lost", "UDP packets lost according to the receiver"),
        (
            "lost_percent",
            "UDP packet loss percentage measured by the receiver",
        ),
        (
            "jitter_ms",
            "UDP jitter in milliseconds measured by the receiver",
        ),
    ] {
        let mut header = false;
        for sample in samples {
            let quality = sample
                .quality
                .filter(|stats| stats.packets_received > 0 || stats.lost > 0);
            let value = match metric {
                "bytes" => sample.bytes.map(|v| v.to_string()),
                "throughput_mbps" => sample.throughput_mbps.map(|v| v.to_string()),
                "packets_expected" => quality.map(|s| s.packets_sent.to_string()),
                "packets_received" => quality.map(|s| s.packets_received.to_string()),
                "packets_lost" => quality.map(|s| s.lost.to_string()),
                "lost_percent" => quality.map(|s| s.lost_percent.to_string()),
                "jitter_ms" => quality
                    .filter(|s| s.packets_received > 0)
                    .map(|s| s.jitter_ms.to_string()),
                _ => unreachable!("fixed metric list"),
            };
            let Some(value) = value else { continue };
            let name = format!("{prefix}_{metric}");
            if !header {
                writeln!(output, "# HELP {name} {help}\n# TYPE {name} gauge")
                    .expect("writing to String cannot fail");
                header = true;
            }
            if matches!(metric, "bytes" | "throughput_mbps") {
                writeln!(
                    output,
                    "{name}{{direction=\"{}\",view=\"{}\"}} {value}",
                    sample.direction, sample.view
                )
                .expect("writing to String cannot fail");
            } else {
                writeln!(
                    output,
                    "{name}{{direction=\"{}\"}} {value}",
                    sample.direction
                )
                .expect("writing to String cannot fail");
            }
        }
    }
}

/// Push metrics to the gateway if configured
pub async fn maybe_push_metrics(push_gateway_url: &Option<String>, stats: &TestStats) {
    maybe_push_metrics_for_peer(push_gateway_url, stats, None, None, &BTreeMap::new()).await;
}

pub(crate) async fn maybe_push_metrics_for_peer(
    push_gateway_url: &Option<String>,
    stats: &TestStats,
    peer: Option<IpAddr>,
    test: Option<(Protocol, Direction)>,
    labels: &BTreeMap<String, String>,
) {
    if let Some(url) = push_gateway_url {
        match PushGatewayClient::new(url.clone()) {
            Ok(client) => {
                let client = if let Some(peer) = peer {
                    client.with_endpoint(
                        "client",
                        &crate::net::normalize_ip(peer).to_string(),
                        "server",
                    )
                } else {
                    client
                };
                let client = if let Some((protocol, direction)) = test {
                    client.with_test(protocol, direction)
                } else {
                    client
                };
                match client.with_labels(labels.clone()) {
                    Ok(client) => client.push_test_metrics(stats).await,
                    Err(error) => error!("Invalid push labels: {error}"),
                }
            }
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

    #[test]
    fn operator_labels_validate_merge_and_preserve_values() {
        use super::merge_push_labels;
        use std::collections::BTreeMap;
        let base = BTreeMap::from([
            ("instance".into(), "config".into()),
            ("site".into(), "lab".into()),
        ]);
        let labels = merge_push_labels(
            base,
            &[
                "instance=cli=a".into(),
                "path=é/\"\\\n".into(),
                "empty=".into(),
            ],
        )
        .unwrap();
        assert_eq!(labels["instance"], "cli=a");
        assert_eq!(labels["site"], "lab");
        assert_eq!(labels["empty"], "");
        for key in [
            "",
            "1bad",
            "bad-name",
            "é",
            "__name__",
            "U__foo",
            "job",
            "role",
            "server",
            "client",
            "protocol",
            "test_direction",
            "direction",
            "view",
            "test_id",
            "stream_id",
        ] {
            assert!(
                merge_push_labels(BTreeMap::new(), &[format!("{key}=x")]).is_err(),
                "{key}"
            );
        }
        assert!(merge_push_labels(BTreeMap::new(), &["site=x".into(), "site=y".into()]).is_err());
        assert!(merge_push_labels(BTreeMap::new(), &["missing".into()]).is_err());
    }

    #[test]
    fn stable_snapshots_have_no_ids_streams_or_unknown_measurements() {
        use crate::protocol::{Direction, Protocol};
        use std::collections::BTreeMap;
        let client = PushGatewayClient::new("http://localhost".into())
            .unwrap()
            .with_labels(BTreeMap::from([("instance".into(), "home".into())]))
            .unwrap()
            .with_server("mesh-server", 5201)
            .with_test(Protocol::Udp, Direction::Bidir)
            .with_job("mesh".into())
            .unwrap();
        client.validate().unwrap();
        let mut result = directional_result();
        let full = client.format_snapshot(&result);
        assert!(
            full.contains("xfr_last_test_udp_direction_packets_lost{direction=\"download\"} 8")
        );
        // Each family's samples directly follow its HELP/TYPE header.
        let mut seen = Vec::new();
        for line in full.lines().filter(|line| !line.starts_with('#')) {
            let family = line.split(['{', ' ']).next().unwrap();
            if seen.last() != Some(&family) {
                assert!(!seen.contains(&family), "{family} split:\n{full}");
                seen.push(family);
            }
        }
        assert!(!full.contains("test_id"));
        assert!(!full.contains("stream_id"));
        assert!(!full.contains(&result.id));
        result.bytes_sent = None;
        result.throughput_send_mbps = None;
        result.udp_stats_send = None;
        result.udp_stats_recv = None;
        let missing = client.format_snapshot(&result);
        assert!(missing.contains(
            "xfr_last_test_udp_direction_available{direction=\"upload\",view=\"receiver\"} 0"
        ));
        assert!(
            missing.contains(
                "xfr_last_test_udp_direction_quality_available{direction=\"download\"} 0"
            )
        );
        assert!(!missing.contains("# TYPE xfr_last_test_udp_direction_packets_lost"));
        assert_eq!(
            missing
                .matches("# TYPE xfr_last_test_udp_direction_available gauge")
                .count(),
            1
        );
        let url = client.push_url("different-id").unwrap();
        assert!(url.path().contains("/job/mesh/"));
        assert!(url.path().contains("/test_direction/bidir"));
        assert!(!url.path().contains("different-id"));
        assert!(
            PushGatewayClient::new("http://localhost".into())
                .unwrap()
                .with_job("mesh".into())
                .unwrap()
                .validate()
                .is_err()
        );
    }

    fn directional_result() -> crate::protocol::TestResult {
        serde_json::from_value(serde_json::json!({
            "id": "direction-fixture", "duration_ms": 1000, "bytes_total": 900,
            "throughput_mbps": 0.0072, "streams": [],
            "bytes_sent": 300, "bytes_received": 600,
            "throughput_send_mbps": 0.0024, "throughput_recv_mbps": 0.0048,
            "udp_stats": {"packets_sent": 10, "packets_received": 9, "lost": 1,
                "lost_percent": 10.0, "jitter_ms": 1.0, "out_of_order": 0},
            "udp_stats_send": {"packets_sent": 10, "packets_received": 9, "lost": 1,
                "lost_percent": 10.0, "jitter_ms": 1.0, "out_of_order": 0},
            "udp_stats_recv": {"packets_sent": 20, "packets_received": 12, "lost": 8,
                "lost_percent": 40.0, "jitter_ms": 3.0, "out_of_order": 0}
        }))
        .unwrap()
    }

    #[test]
    fn directional_context_distinguishes_upload_reverse_and_unknown() {
        use crate::protocol::{Direction, Protocol};
        let client = PushGatewayClient::new("http://localhost".into()).unwrap();
        let result = directional_result();
        assert!(!client.format_result(&result).contains("xfr_udp_direction_"));
        assert!(
            !client
                .clone()
                .with_test(Protocol::Tcp, Direction::Bidir)
                .format_result(&result)
                .contains("xfr_udp_direction_")
        );
        for (direction, name, absent) in [
            (Direction::Upload, "upload", "download"),
            (Direction::Download, "download", "upload"),
        ] {
            let text = client
                .clone()
                .with_test(Protocol::Udp, direction)
                .format_result(&result);
            assert!(text.contains(&format!(
                "xfr_udp_direction_bytes{{direction=\"{name}\",view=\"receiver\"}} 900\n"
            )));
            assert!(text.contains(&format!(
                "xfr_udp_direction_packets_expected{{direction=\"{name}\"}} 10\n"
            )));
            assert!(!text.contains(&format!("direction=\"{absent}\"")));
        }
    }

    #[test]
    fn directional_missing_halves_and_unobserved_quality_are_unknown() {
        use crate::protocol::{Direction, Protocol};
        let client = PushGatewayClient::new("http://localhost".into())
            .unwrap()
            .with_test(Protocol::Udp, Direction::Bidir);
        let mut result = directional_result();
        result.bytes_sent = None;
        result.throughput_send_mbps = None;
        result.udp_stats_send = None;
        result.bytes_received = Some(0);
        result.throughput_recv_mbps = Some(0.0);
        let quality = result.udp_stats_recv.as_mut().unwrap();
        quality.packets_sent = 0;
        quality.packets_received = 0;
        quality.lost = 0;
        let text = client.format_result(&result);
        assert!(!text.contains("direction=\"upload\""));
        assert!(
            text.contains("xfr_udp_direction_bytes{direction=\"download\",view=\"receiver\"} 0\n")
        );
        assert!(!text.contains("# TYPE xfr_udp_direction_packets_"));
        assert!(!text.contains("# TYPE xfr_udp_direction_jitter_ms"));
        result.udp_stats_recv.as_mut().unwrap().lost = 1;
        let text = client.format_result(&result);
        assert!(text.contains("xfr_udp_direction_packets_lost{direction=\"download\"} 1\n"));
        assert!(!text.contains("# TYPE xfr_udp_direction_jitter_ms"));
    }

    #[test]
    fn server_reverse_has_sender_bytes_without_receiver_quality() {
        use crate::protocol::{Direction, Protocol};
        let stats = TestStats::new("server-direction".into(), 1);
        stats.streams[0].add_bytes_sent(500);
        stats.streams[0].add_bytes_received(200);
        stats.add_udp_stats(directional_result().udp_stats.unwrap());
        let client = PushGatewayClient::new("http://localhost".into()).unwrap();
        let reverse = client
            .clone()
            .with_test(Protocol::Udp, Direction::Download)
            .format_metrics(&stats);
        assert!(
            reverse
                .contains("xfr_udp_direction_bytes{direction=\"download\",view=\"sender\"} 500\n")
        );
        assert!(!reverse.contains("# TYPE xfr_udp_direction_packets_"));
        let bidir = client
            .with_test(Protocol::Udp, Direction::Bidir)
            .format_metrics(&stats);
        assert!(
            bidir.contains("xfr_udp_direction_bytes{direction=\"upload\",view=\"receiver\"} 200\n")
        );
        assert!(bidir.contains("xfr_udp_direction_packets_lost{direction=\"upload\"} 1\n"));
        assert!(!bidir.contains("xfr_udp_direction_packets_lost{direction=\"download\"}"));
    }

    #[test]
    fn grouping_round_trips_literal_dot_values() {
        use crate::protocol::{Direction, Protocol};
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        use std::collections::BTreeMap;
        for job in [".", ".."] {
            for site in [".", ".."] {
                let client = PushGatewayClient::new(
                    "http://localhost/proxy/api/v1/import/prometheus/?extra_label=zone%3Dmesh"
                        .into(),
                )
                .unwrap()
                .with_server("mesh-server", 5201)
                .with_test(Protocol::Tcp, Direction::Upload)
                .with_labels(BTreeMap::from([
                    ("instance".into(), "home".into()),
                    ("site".into(), site.into()),
                ]))
                .unwrap()
                .with_job(job.into())
                .unwrap();
                client.validate().unwrap();
                let url = client.push_url("unused-uuid").unwrap();
                assert!(
                    url.path()
                        .starts_with("/proxy/api/v1/import/prometheus/metrics/")
                );
                assert_eq!(url.query(), Some("extra_label=zone%3Dmesh"));
                let segments: Vec<_> = url.path_segments().unwrap().collect();
                let groups = &segments[segments
                    .iter()
                    .position(|segment| *segment == "metrics")
                    .unwrap()
                    + 1..];
                assert!(groups.len().is_multiple_of(2), "{url}");
                for (name, expected) in [("job", job), ("site", site)] {
                    let encoded_name = format!("{name}@base64");
                    let pair = groups
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .find(|pair| pair[0] == name || pair[0] == encoded_name)
                        .unwrap();
                    let actual = if pair[0] == encoded_name {
                        String::from_utf8(URL_SAFE_NO_PAD.decode(pair[1]).unwrap()).unwrap()
                    } else {
                        pair[1].to_string()
                    };
                    assert_eq!(actual, expected, "{name} lost in {url}");
                }
            }
        }
    }

    #[test]
    fn grouping_preserves_gateway_path_query_and_ipv6_endpoint() {
        let client = PushGatewayClient::new(
            "http://localhost:8429/api/v1/import/prometheus/?extra_label=site%3Dmesh".into(),
        )
        .unwrap()
        .with_server("fe80::1%eth0", 5201);
        let url = client.push_url("fixture").unwrap();
        assert!(url.path().starts_with(
            "/api/v1/import/prometheus/metrics/job/xfr_test_fixture/role/client/server/"
        ));
        assert!(url.path().ends_with("[fe80::1%25eth0]:5201"));
        assert_eq!(url.query(), Some("extra_label=site%3Dmesh"));
    }

    #[test]
    fn grouping_encodes_slashes_and_empty_values() {
        let client = PushGatewayClient::new("http://localhost".into())
            .unwrap()
            .with_endpoint("server", "", "client");
        assert_eq!(
            client.push_url("a/b").unwrap().path(),
            "/metrics/job@base64/eGZyX3Rlc3RfYS9i/role/client/server@base64/="
        );
        assert!(PushGatewayClient::new("file:///tmp/push".into()).is_err());
    }

    #[test]
    fn test_ids_cannot_inject_prometheus_samples() {
        let stats = TestStats::new("quote\"slash\\\nnewline".into(), 1);
        let client = PushGatewayClient::new("http://localhost".into()).unwrap();
        let text = client.format_metrics(&stats);
        assert!(text.contains("test_id=\"quote\\\"slash\\\\\\nnewline\""));
        assert_eq!(
            text.lines().filter(|line| !line.starts_with('#')).count(),
            6
        );
    }

    #[test]
    fn metrics_payload_keeps_exact_text_format() {
        let mut stats = TestStats::new("fixture".into(), 2);
        // Keep elapsed time at zero so the full payload is deterministic.
        let start = Instant::now() + Duration::from_secs(3600);
        stats.start_time = start;
        for stream in &mut stats.streams {
            std::sync::Arc::get_mut(stream).unwrap().start_time = start;
        }
        stats.streams[0].add_bytes_sent(1_000_000);
        stats.streams[1].add_bytes_sent(2_000_000);
        stats.streams[0].set_final_tcp_info(TcpInfoSnapshot {
            retransmits: 5,
            rtt_us: 1200,
            rtt_var_us: 300,
            cwnd: 65536,
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
        let client = PushGatewayClient::new("http://localhost".into()).unwrap();
        assert_eq!(
            client.format_metrics(&stats),
            r#"# Test ID: fixture
# HELP xfr_bytes_total Total bytes transferred
# TYPE xfr_bytes_total counter
xfr_bytes_total 3000000
# HELP xfr_throughput_mbps Average throughput in Mbps
# TYPE xfr_throughput_mbps gauge
xfr_throughput_mbps 0
# HELP xfr_duration_seconds Test duration in seconds
# TYPE xfr_duration_seconds gauge
xfr_duration_seconds 0
# HELP xfr_stream_bytes_total Per-stream total bytes transferred
# TYPE xfr_stream_bytes_total counter
# HELP xfr_stream_throughput_mbps Per-stream average throughput in Mbps
# TYPE xfr_stream_throughput_mbps gauge
# HELP xfr_stream_retransmits_total Per-stream TCP retransmits
# TYPE xfr_stream_retransmits_total counter
xfr_stream_bytes_total{test_id="fixture",stream_id="0"} 1000000
xfr_stream_throughput_mbps{test_id="fixture",stream_id="0"} 0
xfr_stream_retransmits_total{test_id="fixture",stream_id="0"} 0
xfr_stream_bytes_total{test_id="fixture",stream_id="1"} 2000000
xfr_stream_throughput_mbps{test_id="fixture",stream_id="1"} 0
xfr_stream_retransmits_total{test_id="fixture",stream_id="1"} 0
# HELP xfr_tcp_rtt_microseconds TCP round-trip time in microseconds
# TYPE xfr_tcp_rtt_microseconds gauge
xfr_tcp_rtt_microseconds 1200
# HELP xfr_tcp_retransmits_total TCP retransmits
# TYPE xfr_tcp_retransmits_total counter
xfr_tcp_retransmits_total 5
# HELP xfr_tcp_cwnd_bytes TCP congestion window in bytes
# TYPE xfr_tcp_cwnd_bytes gauge
xfr_tcp_cwnd_bytes 65536
# HELP xfr_udp_packets_sent UDP packets sent
# TYPE xfr_udp_packets_sent counter
xfr_udp_packets_sent 100
# HELP xfr_udp_packets_received UDP packets received
# TYPE xfr_udp_packets_received counter
xfr_udp_packets_received 95
# HELP xfr_udp_packets_lost UDP packets lost
# TYPE xfr_udp_packets_lost counter
xfr_udp_packets_lost 5
# HELP xfr_udp_jitter_ms UDP jitter in milliseconds
# TYPE xfr_udp_jitter_ms gauge
xfr_udp_jitter_ms 1.5
# HELP xfr_udp_lost_percent UDP packet loss percentage
# TYPE xfr_udp_lost_percent gauge
xfr_udp_lost_percent 5
"#
        );
    }

    #[test]
    fn extreme_udp_sequence_gaps_cannot_overflow_metrics() {
        let stats = TestStats::new("extreme".to_string(), 2);
        for _ in 0..2 {
            stats.add_udp_stats(UdpStats {
                packets_sent: u64::MAX,
                packets_received: 1,
                lost: u64::MAX - 1,
                lost_percent: 100.0,
                jitter_ms: 0.0,
                jitter_max_ms: None,
                out_of_order: 0,
                packet_size: Some(16),
            });
        }
        let client = PushGatewayClient::new("http://example.com".to_string()).unwrap();
        let metrics = client.format_metrics(&stats);
        assert!(metrics.contains(&format!("xfr_udp_packets_lost {}", u64::MAX as f64)));
        assert!(metrics.contains("xfr_udp_packets_received 2"));
    }

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
