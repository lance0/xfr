//! CSV output format

use crate::protocol::{TestResult, TimestampFormat};

/// Output test result as CSV
pub fn output_csv(result: &TestResult) -> String {
    let mut output = String::new();

    // Header. bytes_sent / bytes_received / throughput_send_mbps /
    // throughput_recv_mbps are populated only for bidirectional tests;
    // unidirectional tests leave those columns empty.
    output.push_str(
        "test_id,duration_secs,transfer_bytes,throughput_mbps,retransmits,jitter_ms,lost,lost_percent,bytes_sent,bytes_received,throughput_send_mbps,throughput_recv_mbps\n",
    );

    let fmt_u64 = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_default();
    let fmt_f64 = |v: Option<f64>| v.map(|n| format!("{:.2}", n)).unwrap_or_default();

    // Summary row
    output.push_str(&format!(
        "{},{:.2},{},{:.2},{},{:.2},{},{:.2},{},{},{},{}\n",
        result.id,
        result.duration_ms as f64 / 1000.0,
        result.bytes_total,
        result.throughput_mbps,
        result.tcp_info.as_ref().map(|t| t.retransmits).unwrap_or(0),
        result
            .udp_stats
            .as_ref()
            .map(|u| u.jitter_ms)
            .unwrap_or(0.0),
        result.udp_stats.as_ref().map(|u| u.lost).unwrap_or(0),
        result
            .udp_stats
            .as_ref()
            .map(|u| u.lost_percent)
            .unwrap_or(0.0),
        fmt_u64(result.bytes_sent),
        fmt_u64(result.bytes_received),
        fmt_f64(result.throughput_send_mbps),
        fmt_f64(result.throughput_recv_mbps),
    ));

    output
}

/// Output interval as CSV line
#[allow(clippy::too_many_arguments)]
pub fn output_interval_csv(
    timestamp: &str,
    elapsed_secs: f64,
    throughput_mbps: f64,
    bytes: u64,
    retransmits: Option<u64>,
    jitter_ms: Option<f64>,
    lost: Option<u64>,
    rtt_us: Option<u32>,
    cwnd: Option<u32>,
) -> String {
    format!(
        "{},{:.2},{},{:.2},{},{},{},{},{}\n",
        timestamp,
        elapsed_secs,
        bytes,
        throughput_mbps,
        retransmits.unwrap_or(0),
        jitter_ms.unwrap_or(0.0),
        lost.unwrap_or(0),
        rtt_us.map(|r| r.to_string()).unwrap_or_default(),
        cwnd.map(|c| c.to_string()).unwrap_or_default(),
    )
}

/// CSV header for interval output
pub fn csv_interval_header(_timestamp_format: &TimestampFormat) -> String {
    "timestamp,elapsed_secs,bytes,throughput_mbps,retransmits,jitter_ms,lost,rtt_us,cwnd\n"
        .to_string()
}
