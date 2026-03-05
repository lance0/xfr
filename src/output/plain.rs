//! Plain text output

use crate::protocol::TestResult;
use crate::stats::{bytes_to_human, mbps_to_human};

pub fn output_plain(result: &TestResult, mptcp: bool) -> String {
    let mut output = String::new();

    output.push_str("─".repeat(60).as_str());
    output.push('\n');
    output.push_str("  xfr Test Results\n");
    output.push_str("─".repeat(60).as_str());
    output.push('\n');
    output.push('\n');

    output.push_str(&format!(
        "  Duration:    {:.2}s\n",
        result.duration_ms as f64 / 1000.0
    ));
    output.push_str(&format!(
        "  Transfer:    {}\n",
        bytes_to_human(result.bytes_total)
    ));
    output.push_str(&format!(
        "  Throughput:  {}\n",
        mbps_to_human(result.throughput_mbps)
    ));
    output.push('\n');

    if let Some(ref tcp_info) = result.tcp_info {
        if mptcp {
            output.push_str("  Sender TCP Info (initial subflow):\n");
        } else {
            output.push_str("  Sender TCP Info:\n");
        }
        output.push_str(&format!("    Retransmits: {}\n", tcp_info.retransmits));
        output.push_str(&format!(
            "    RTT:         {:.2}ms\n",
            tcp_info.rtt_us as f64 / 1000.0
        ));
        output.push_str(&format!(
            "    RTT Var:     {:.2}ms\n",
            tcp_info.rtt_var_us as f64 / 1000.0
        ));
        output.push_str(&format!("    Cwnd:        {} KB\n", tcp_info.cwnd / 1024));
        output.push('\n');
    }

    if let Some(ref udp_stats) = result.udp_stats {
        output.push_str("  UDP Stats:\n");
        output.push_str(&format!(
            "    Packets Sent:     {}\n",
            udp_stats.packets_sent
        ));
        output.push_str(&format!(
            "    Packets Received: {}\n",
            udp_stats.packets_received
        ));
        output.push_str(&format!(
            "    Lost:             {} ({:.2}%)\n",
            udp_stats.lost, udp_stats.lost_percent
        ));
        output.push_str(&format!(
            "    Jitter:           {:.2}ms\n",
            udp_stats.jitter_ms
        ));
        output.push_str(&format!(
            "    Out of Order:     {}\n",
            udp_stats.out_of_order
        ));
        output.push('\n');
    }

    if result.streams.len() > 1 {
        output.push_str("  Per-Stream Results:\n");
        for stream in &result.streams {
            output.push_str(&format!(
                "    [{}] {} @ {}",
                stream.id,
                bytes_to_human(stream.bytes),
                mbps_to_human(stream.throughput_mbps)
            ));
            if let Some(rtx) = stream.retransmits {
                output.push_str(&format!("  rtx: {}", rtx));
            }
            output.push('\n');
        }
        output.push('\n');
    }

    output.push_str("─".repeat(60).as_str());
    output.push('\n');

    output
}

pub fn output_interval_plain(
    timestamp: &str,
    _elapsed_secs: f64,
    throughput_mbps: f64,
    bytes: u64,
    retransmits: Option<u64>,
    rtt_us: Option<u32>,
) -> String {
    let mut output = format!(
        "[{}]  {}  {}",
        timestamp,
        mbps_to_human(throughput_mbps),
        bytes_to_human(bytes)
    );

    if let Some(rtx) = retransmits {
        output.push_str(&format!("  rtx: {}", rtx));
    }

    if let Some(rtt) = rtt_us {
        output.push_str(&format!("  rtt: {:.2}ms", rtt as f64 / 1000.0));
    }

    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{StreamResult, TcpInfoSnapshot};

    fn make_result_with_tcp_info() -> TestResult {
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
            tcp_info: Some(TcpInfoSnapshot {
                retransmits: 1,
                rtt_us: 1000,
                rtt_var_us: 100,
                cwnd: 64 * 1024,
            }),
            udp_stats: None,
        }
    }

    #[test]
    fn test_output_plain_sender_tcp_label_tcp() {
        let output = output_plain(&make_result_with_tcp_info(), false);
        assert!(output.contains("Sender TCP Info:\n"));
        assert!(!output.contains("initial subflow"));
    }

    #[test]
    fn test_output_plain_sender_tcp_label_mptcp() {
        let output = output_plain(&make_result_with_tcp_info(), true);
        assert!(output.contains("Sender TCP Info (initial subflow):\n"));
    }
}
