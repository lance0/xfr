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
///
/// The trailing bytes_sent / bytes_received / throughput_send_mbps /
/// throughput_recv_mbps columns are populated only for bidirectional
/// tests; unidirectional tests leave them empty. They are appended at
/// the end of the row so downstream parsers that index columns by
/// position keep working.
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
    bytes_sent: Option<u64>,
    bytes_received: Option<u64>,
    throughput_send_mbps: Option<f64>,
    throughput_recv_mbps: Option<f64>,
) -> String {
    format!(
        "{},{:.2},{},{:.2},{},{},{},{},{},{},{},{},{}\n",
        timestamp,
        elapsed_secs,
        bytes,
        throughput_mbps,
        retransmits.unwrap_or(0),
        jitter_ms.unwrap_or(0.0),
        lost.unwrap_or(0),
        rtt_us.map(|r| r.to_string()).unwrap_or_default(),
        cwnd.map(|c| c.to_string()).unwrap_or_default(),
        bytes_sent.map(|b| b.to_string()).unwrap_or_default(),
        bytes_received.map(|b| b.to_string()).unwrap_or_default(),
        throughput_send_mbps
            .map(|t| format!("{:.2}", t))
            .unwrap_or_default(),
        throughput_recv_mbps
            .map(|t| format!("{:.2}", t))
            .unwrap_or_default(),
    )
}

/// CSV header for interval output
pub fn csv_interval_header(_timestamp_format: &TimestampFormat) -> String {
    "timestamp,elapsed_secs,bytes,throughput_mbps,retransmits,jitter_ms,lost,rtt_us,cwnd,bytes_sent,bytes_received,throughput_send_mbps,throughput_recv_mbps\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::StreamResult;

    fn make_result() -> TestResult {
        TestResult {
            id: "test-1".to_string(),
            bytes_total: 1024,
            duration_ms: 1000,
            throughput_mbps: 8.0,
            streams: vec![StreamResult {
                id: 0,
                bytes: 1024,
                throughput_mbps: 8.0,
                retransmits: Some(1),
                jitter_ms: None,
                lost: None,
            }],
            tcp_info: None,
            udp_stats: None,
            bytes_sent: None,
            bytes_received: None,
            throughput_send_mbps: None,
            throughput_recv_mbps: None,
            mtu_probe: None,
        }
    }

    #[test]
    fn test_interval_header_appends_split_columns() {
        // Existing columns must keep their positions; the per-direction
        // split columns are appended at the end.
        let header = csv_interval_header(&TimestampFormat::Relative);
        assert_eq!(
            header,
            "timestamp,elapsed_secs,bytes,throughput_mbps,retransmits,jitter_ms,lost,rtt_us,cwnd,bytes_sent,bytes_received,throughput_send_mbps,throughput_recv_mbps\n"
        );
    }

    #[test]
    fn test_bidir_interval_row_has_split_values() {
        let row = output_interval_csv(
            "1.000",
            1.0,
            96.0,
            12_000_000,
            Some(2),
            None,
            None,
            Some(1200),
            Some(65535),
            Some(5_000_000),
            Some(7_000_000),
            Some(40.0),
            Some(56.0),
        );
        assert_eq!(
            row,
            "1.000,1.00,12000000,96.00,2,0,0,1200,65535,5000000,7000000,40.00,56.00\n"
        );
    }

    #[test]
    fn test_unidir_interval_row_leaves_split_empty() {
        let row = output_interval_csv(
            "1.000", 1.0, 8.0, 1024, None, None, None, None, None, None, None, None, None,
        );
        assert_eq!(row, "1.000,1.00,1024,8.00,0,0,0,,,,,,\n");
        // 13 columns, last four empty.
        assert_eq!(row.trim_end().split(',').count(), 13);
    }

    #[test]
    fn test_bidir_summary_row_has_split_values() {
        let mut result = make_result();
        result.bytes_total = 12_000_000;
        result.throughput_mbps = 96.0;
        result.bytes_sent = Some(5_000_000);
        result.bytes_received = Some(7_000_000);
        result.throughput_send_mbps = Some(40.0);
        result.throughput_recv_mbps = Some(56.0);

        let output = output_csv(&result);
        let mut lines = output.lines();
        let header = lines.next().unwrap();
        assert_eq!(
            header,
            "test_id,duration_secs,transfer_bytes,throughput_mbps,retransmits,jitter_ms,lost,lost_percent,bytes_sent,bytes_received,throughput_send_mbps,throughput_recv_mbps"
        );
        let row = lines.next().unwrap();
        assert_eq!(
            row,
            "test-1,1.00,12000000,96.00,0,0.00,0,0.00,5000000,7000000,40.00,56.00"
        );
    }

    #[test]
    fn test_unidir_summary_row_leaves_split_empty() {
        let output = output_csv(&make_result());
        let row = output.lines().nth(1).unwrap();
        assert_eq!(row, "test-1,1.00,1024,8.00,0,0.00,0,0.00,,,,");
        assert_eq!(row.split(',').count(), 12);
    }
}
