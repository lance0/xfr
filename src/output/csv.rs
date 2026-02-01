//! CSV output format

use crate::protocol::TestResult;

/// Output test result as CSV
pub fn output_csv(result: &TestResult) -> String {
    let mut output = String::new();

    // Header
    output.push_str("test_id,duration_secs,transfer_bytes,throughput_mbps,retransmits,jitter_ms,lost,lost_percent\n");

    // Summary row
    output.push_str(&format!(
        "{},{:.2},{},{:.2},{},{:.2},{},{:.2}\n",
        result.id,
        result.duration_ms as f64 / 1000.0,
        result.bytes_total,
        result.throughput_mbps,
        result.tcp_info.as_ref().map(|t| t.retransmits).unwrap_or(0),
        result.udp_stats.as_ref().map(|u| u.jitter_ms).unwrap_or(0.0),
        result.udp_stats.as_ref().map(|u| u.lost).unwrap_or(0),
        result.udp_stats.as_ref().map(|u| u.lost_percent).unwrap_or(0.0),
    ));

    output
}

/// Output interval as CSV line
pub fn output_interval_csv(
    elapsed_secs: f64,
    throughput_mbps: f64,
    bytes: u64,
    retransmits: Option<u64>,
    jitter_ms: Option<f64>,
    lost: Option<u64>,
) -> String {
    format!(
        "{:.2},{},{:.2},{},{},{}\n",
        elapsed_secs,
        bytes,
        throughput_mbps,
        retransmits.unwrap_or(0),
        jitter_ms.unwrap_or(0.0),
        lost.unwrap_or(0),
    )
}

/// CSV header for interval output
pub fn csv_interval_header() -> &'static str {
    "elapsed_secs,bytes,throughput_mbps,retransmits,jitter_ms,lost\n"
}
