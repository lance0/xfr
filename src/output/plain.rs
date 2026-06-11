//! Plain text output

use crate::probe::{MtuProbeReport, SizeVerdict};
use crate::protocol::TestResult;
use crate::stats::{bytes_to_human, mbps_to_human};

pub fn output_plain(result: &TestResult, mptcp: bool) -> String {
    // A probe run carries no meaningful throughput numbers; the
    // per-size table IS the result.
    if let Some(ref probe) = result.mtu_probe {
        return format_mtu_probe(probe);
    }
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

    // Bidirectional tests: show per-direction bytes/throughput alongside the total.
    // Unidirectional tests report only the combined number (which equals the
    // single-direction throughput anyway).
    if let (Some(sent), Some(recv), Some(ts_send), Some(ts_recv)) = (
        result.bytes_sent,
        result.bytes_received,
        result.throughput_send_mbps,
        result.throughput_recv_mbps,
    ) {
        output.push_str(&format!(
            "  Transfer:    Send: {}    Recv: {}    (Total: {})\n",
            bytes_to_human(sent),
            bytes_to_human(recv),
            bytes_to_human(result.bytes_total)
        ));
        output.push_str(&format!(
            "  Throughput:  Send: {}  Recv: {}  (Total: {})\n",
            mbps_to_human(ts_send),
            mbps_to_human(ts_recv),
            mbps_to_human(result.throughput_mbps)
        ));
    } else {
        output.push_str(&format!(
            "  Transfer:    {}\n",
            bytes_to_human(result.bytes_total)
        ));
        output.push_str(&format!(
            "  Throughput:  {}\n",
            mbps_to_human(result.throughput_mbps)
        ));
    }
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
        if let Some(max) = udp_stats.jitter_max_ms {
            output.push_str(&format!("    Jitter Max:       {:.2}ms\n", max));
        }
        output.push_str(&format!(
            "    Out of Order:     {}\n",
            udp_stats.out_of_order
        ));
        if let Some(size) = udp_stats.packet_size {
            output.push_str(&format!("    Packet Size:      {} bytes\n", size));
        }
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

/// Render a `--probe-mtu` report (issue #64): the per-size walk, then
/// the largest surviving payload and derived path MTU per direction.
fn format_mtu_probe(probe: &MtuProbeReport) -> String {
    let mut output = String::new();

    output.push_str("─".repeat(60).as_str());
    output.push('\n');
    output.push_str("  xfr Path MTU Probe\n");
    output.push_str("─".repeat(60).as_str());
    output.push_str("\n\n");

    output.push_str(&format!(
        "  Address family: {} ({} bytes IP+UDP overhead)\n\n",
        if probe.ipv6 { "IPv6" } else { "IPv4" },
        crate::probe::ip_overhead(probe.ipv6)
    ));

    output.push_str("  Payload      client → server    server → client\n");
    for size in &probe.sizes {
        output.push_str(&format!(
            "  {:>7}      {:<18} {}\n",
            size.payload,
            format_verdict(size.forward, size.forward_ok, size.attempts),
            format_verdict(size.reverse, size.reverse_ok, size.attempts),
        ));
    }
    output.push('\n');

    match (probe.forward_max_payload, probe.forward_path_mtu) {
        (Some(payload), Some(mtu)) => output.push_str(&format!(
            "  Largest payload client → server: {} bytes  (path MTU ≈ {})\n",
            payload, mtu
        )),
        _ => output.push_str("  Largest payload client → server: none survived\n"),
    }
    match (probe.reverse_max_payload, probe.reverse_path_mtu) {
        (Some(payload), Some(mtu)) => output.push_str(&format!(
            "  Largest payload server → client: {} bytes  (path MTU ≈ {})\n",
            payload, mtu
        )),
        _ => output.push_str("  Largest payload server → client: none observed\n"),
    }
    // The echo can only ever be as large as the probe that triggered
    // it, so a wider reverse path is invisible from this side.
    output.push_str("  (server → client is a lower bound, capped by the client → server path)\n");

    output.push_str("─".repeat(60).as_str());
    output.push('\n');

    output
}

fn format_verdict(verdict: SizeVerdict, ok: u8, attempts: u8) -> String {
    match verdict {
        SizeVerdict::Pass => format!("pass ({}/{})", ok, attempts),
        SizeVerdict::Fail => format!("BLOCKED (0/{})", attempts),
        SizeVerdict::LocalLimit => "local MTU limit".to_string(),
        SizeVerdict::Untested => "untested".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn output_interval_plain(
    timestamp: &str,
    _elapsed_secs: f64,
    throughput_mbps: f64,
    bytes: u64,
    retransmits: Option<u64>,
    jitter_ms: Option<f64>,
    lost: Option<u64>,
    rtt_us: Option<u32>,
) -> String {
    let mut output = format!(
        "[{}]  {}  {}",
        timestamp,
        mbps_to_human(throughput_mbps),
        bytes_to_human(bytes)
    );

    // UDP shows jitter/lost; TCP shows rtx/rtt. Use jitter presence to distinguish.
    if let Some(jitter) = jitter_ms {
        output.push_str(&format!("  jitter: {:.2}ms", jitter));
        if let Some(l) = lost {
            output.push_str(&format!("  lost: {}", l));
        }
    } else {
        if let Some(rtx) = retransmits {
            output.push_str(&format!("  rtx: {}", rtx));
        }
        if let Some(rtt) = rtt_us {
            output.push_str(&format!("  rtt: {:.2}ms", rtt as f64 / 1000.0));
        }
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
                bytes_acked: None,
            }),
            udp_stats: None,
            bytes_sent: None,
            bytes_received: None,
            throughput_send_mbps: None,
            throughput_recv_mbps: None,
            mtu_probe: None,
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

    #[test]
    fn test_output_plain_bidir_shows_split() {
        // Bidirectional result populates the split fields — output should render
        // Send/Recv/Total lines so asymmetric throughput is visible.
        let mut result = make_result_with_tcp_info();
        result.bytes_sent = Some(5_000_000_000);
        result.bytes_received = Some(7_000_000_000);
        result.throughput_send_mbps = Some(40_000.0);
        result.throughput_recv_mbps = Some(56_000.0);
        result.bytes_total = 12_000_000_000;
        result.throughput_mbps = 96_000.0;

        let output = output_plain(&result, false);
        assert!(output.contains("Send:"), "expected split 'Send:' line");
        assert!(output.contains("Recv:"), "expected split 'Recv:' line");
        assert!(output.contains("(Total:"), "expected combined total");
    }

    #[test]
    fn test_output_plain_unidir_no_split() {
        // Unidirectional result leaves Options None — output stays single-line.
        let output = output_plain(&make_result_with_tcp_info(), false);
        assert!(
            !output.contains("Send:") && !output.contains("Recv:"),
            "unidir output must not show split lines"
        );
    }

    #[test]
    fn test_interval_plain_tcp_shows_rtx_rtt() {
        let output = output_interval_plain(
            "1.001",
            1.0,
            48000.0,
            6_000_000_000,
            Some(5),
            None,
            None,
            Some(50),
        );
        assert!(output.contains("rtx: 5"));
        assert!(output.contains("rtt: 0.05ms"));
        assert!(!output.contains("jitter:"));
        assert!(!output.contains("lost:"));
    }

    #[test]
    fn test_interval_plain_udp_shows_jitter_lost() {
        let output = output_interval_plain(
            "1.001",
            1.0,
            1000.0,
            125_000_000,
            Some(0),
            Some(1.42),
            Some(3),
            None,
        );
        assert!(output.contains("jitter: 1.42ms"));
        assert!(output.contains("lost: 3"));
        assert!(!output.contains("rtx:"));
        assert!(!output.contains("rtt:"));
    }
}
