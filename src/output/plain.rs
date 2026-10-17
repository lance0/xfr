//! Plain text output

use std::fmt::Write;

use crate::probe::{MtuProbeReport, SizeVerdict};
use crate::protocol::{TestResult, UdpDirectionProgress, UdpStats};
use crate::stats::{bytes_to_human, mbps_to_human};

const REPORT_SEPARATOR: &str = "────────────────────────────────────────────────────────────";

pub fn output_plain(result: &TestResult, mptcp: bool) -> String {
    // A probe run carries no meaningful throughput numbers; the
    // per-size table IS the result.
    if let Some(ref probe) = result.mtu_probe {
        return format_mtu_probe(probe);
    }
    let mut output = String::new();

    output.push_str(REPORT_SEPARATOR);
    output.push('\n');
    output.push_str("  xfr Test Results\n");
    output.push_str(REPORT_SEPARATOR);
    output.push('\n');
    output.push('\n');

    writeln!(
        output,
        "  Duration:    {:.2}s",
        result.duration_ms as f64 / 1000.0
    )
    .expect("writing to String cannot fail");

    // Bidirectional tests: show per-direction bytes/throughput alongside the total.
    // Unidirectional tests report only the combined number (which equals the
    // single-direction throughput anyway).
    if let (Some(sent), Some(recv), Some(ts_send), Some(ts_recv)) = (
        result.bytes_sent,
        result.bytes_received,
        result.throughput_send_mbps,
        result.throughput_recv_mbps,
    ) {
        writeln!(
            output,
            "  Transfer:    Send: {}    Recv: {}    (Total: {})",
            bytes_to_human(sent),
            bytes_to_human(recv),
            bytes_to_human(result.bytes_total)
        )
        .expect("writing to String cannot fail");
        writeln!(
            output,
            "  Throughput:  Send: {}  Recv: {}  (Total: {})",
            mbps_to_human(ts_send),
            mbps_to_human(ts_recv),
            mbps_to_human(result.throughput_mbps)
        )
        .expect("writing to String cannot fail");
    } else {
        writeln!(
            output,
            "  Transfer:    {}",
            bytes_to_human(result.bytes_total)
        )
        .expect("writing to String cannot fail");
        writeln!(
            output,
            "  Throughput:  {}",
            mbps_to_human(result.throughput_mbps)
        )
        .expect("writing to String cannot fail");
    }
    output.push('\n');

    if let Some(ref tcp_info) = result.tcp_info {
        if mptcp {
            output.push_str("  Sender TCP Info (initial subflow):\n");
        } else {
            output.push_str("  Sender TCP Info:\n");
        }
        writeln!(output, "    Retransmits: {}", tcp_info.retransmits)
            .expect("writing to String cannot fail");
        writeln!(
            output,
            "    RTT:         {:.2}ms",
            tcp_info.rtt_us as f64 / 1000.0
        )
        .expect("writing to String cannot fail");
        writeln!(
            output,
            "    RTT Var:     {:.2}ms",
            tcp_info.rtt_var_us as f64 / 1000.0
        )
        .expect("writing to String cannot fail");
        writeln!(output, "    Cwnd:        {} KB", tcp_info.cwnd / 1024)
            .expect("writing to String cannot fail");
        output.push('\n');
    }

    if result.udp_stats_send.is_some() || result.udp_stats_recv.is_some() {
        append_udp_stats(
            &mut output,
            "Upload UDP Stats",
            result.udp_stats_send.as_deref(),
        );
        append_udp_stats(
            &mut output,
            "Download UDP Stats",
            result.udp_stats_recv.as_deref(),
        );
    } else if let Some(ref udp_stats) = result.udp_stats {
        append_udp_stats(&mut output, "UDP Stats", Some(udp_stats));
    }

    for (side, errors) in [
        ("Server", &result.server_udp_send_errors),
        ("Client", &result.client_udp_send_errors),
    ] {
        if let Some(errors) = errors {
            writeln!(output, "  Warning: {}\n", errors.describe(side))
                .expect("writing to String cannot fail");
        }
    }
    if let Some(msg) = result.describe_zero_byte_streams() {
        writeln!(output, "  Warning: {msg}\n").expect("writing to String cannot fail");
    }

    if result.streams.len() > 1
        || result
            .streams
            .iter()
            .any(|s| s.bytes_sent.is_some() || s.bytes_received.is_some())
    {
        output.push_str("  Per-Stream Results:\n");
        for stream in &result.streams {
            write!(
                output,
                "    [{}] {} @ {}",
                stream.id,
                bytes_to_human(stream.bytes),
                mbps_to_human(stream.throughput_mbps)
            )
            .expect("writing to String cannot fail");
            if let Some(rtx) = stream.retransmits {
                write!(output, "  rtx: {}", rtx).expect("writing to String cannot fail");
            }
            output.push('\n');
            // The bidir split gets its own indented line: on one line it
            // ran to ~100 columns and wrapped on an 80-column terminal.
            if let (Some(sent), Some(recv), Some(send_rate), Some(recv_rate)) = (
                stream.bytes_sent,
                stream.bytes_received,
                stream.throughput_send_mbps,
                stream.throughput_recv_mbps,
            ) {
                writeln!(
                    output,
                    "        Upload: {} @ {}  Download: {} @ {}",
                    bytes_to_human(sent),
                    mbps_to_human(send_rate),
                    bytes_to_human(recv),
                    mbps_to_human(recv_rate)
                )
                .expect("writing to String cannot fail");
            }
        }
        output.push('\n');
    }

    output.push_str(REPORT_SEPARATOR);
    output.push('\n');

    output
}

fn append_udp_stats(output: &mut String, title: &str, stats: Option<&UdpStats>) {
    writeln!(output, "  {title}:").expect("writing to String cannot fail");
    if let Some(udp_stats) = stats {
        writeln!(output, "    Packets Sent:     {}", udp_stats.packets_sent)
            .expect("writing to String cannot fail");
        writeln!(
            output,
            "    Packets Received: {}",
            udp_stats.packets_received
        )
        .expect("writing to String cannot fail");
        writeln!(
            output,
            "    Lost:             {} ({:.2}%)",
            udp_stats.lost, udp_stats.lost_percent
        )
        .expect("writing to String cannot fail");
        writeln!(output, "    Jitter:           {:.2}ms", udp_stats.jitter_ms)
            .expect("writing to String cannot fail");
        if let Some(max) = udp_stats.jitter_max_ms {
            writeln!(output, "    Jitter Max:       {:.2}ms", max)
                .expect("writing to String cannot fail");
        }
        writeln!(output, "    Out of Order:     {}", udp_stats.out_of_order)
            .expect("writing to String cannot fail");
        if let Some(size) = udp_stats.packet_size {
            writeln!(output, "    Packet Size:      {} bytes", size)
                .expect("writing to String cannot fail");
        }
        output.push('\n');
    } else {
        output.push_str("    Not reported\n\n");
    }
}

/// Render a `--probe-mtu` report (issue #64): the per-size walk, then
/// the largest surviving payload and derived path MTU per direction.
fn format_mtu_probe(probe: &MtuProbeReport) -> String {
    let mut output = String::new();

    output.push_str(REPORT_SEPARATOR);
    output.push('\n');
    output.push_str("  xfr Path MTU Probe\n");
    output.push_str(REPORT_SEPARATOR);
    output.push_str("\n\n");

    writeln!(
        output,
        "  Address family: {} ({} bytes IP+UDP overhead)\n",
        if probe.ipv6 { "IPv6" } else { "IPv4" },
        crate::probe::ip_overhead(probe.ipv6)
    )
    .expect("writing to String cannot fail");

    output.push_str("  Payload      client → server    server → client\n");
    for size in &probe.sizes {
        writeln!(
            output,
            "  {:>7}      {:<18} {}",
            size.payload,
            format_verdict(size.forward, size.forward_ok, size.attempts),
            format_verdict(size.reverse, size.reverse_ok, size.attempts),
        )
        .expect("writing to String cannot fail");
    }
    output.push('\n');

    match (probe.forward_max_payload, probe.forward_path_mtu) {
        (Some(payload), Some(mtu)) => writeln!(
            output,
            "  Largest payload client → server: {} bytes  (path MTU ≈ {})",
            payload, mtu
        )
        .expect("writing to String cannot fail"),
        _ => output.push_str("  Largest payload client → server: none survived\n"),
    }
    match (probe.reverse_max_payload, probe.reverse_path_mtu) {
        (Some(payload), Some(mtu)) => writeln!(
            output,
            "  Largest payload server → client: {} bytes  (path MTU ≈ {})",
            payload, mtu
        )
        .expect("writing to String cannot fail"),
        _ => output.push_str("  Largest payload server → client: none observed\n"),
    }
    // The echo can only ever be as large as the probe that triggered
    // it, so a wider reverse path is invisible from this side.
    output.push_str("  (server → client is a lower bound, capped by the client → server path)\n");

    output.push_str(REPORT_SEPARATOR);
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
    // Right-align every column that has another column after it. The human
    // formatters return variable-width strings ("193 KB" vs "119.25 MB",
    // "200.0 Mbps" vs "1.20 Gbps"), so an unpadded line makes the numbers
    // jump sideways once a second while streaming. Widths cover the widest
    // value each formatter produces below 1000 Gbps / 1 TB.
    let mut output = format!(
        "[{}]  {:>11}  {:>10}",
        timestamp,
        mbps_to_human(throughput_mbps),
        bytes_to_human(bytes)
    );

    // UDP shows jitter/lost; TCP shows rtx/rtt. Use jitter presence to distinguish.
    if let Some(jitter) = jitter_ms {
        write!(output, "  jitter: {:>7.2}ms", jitter).expect("writing to String cannot fail");
        if let Some(l) = lost {
            write!(output, "  lost: {}", l).expect("writing to String cannot fail");
        }
    } else {
        if let Some(rtx) = retransmits {
            write!(output, "  rtx: {:>5}", rtx).expect("writing to String cannot fail");
        }
        if let Some(rtt) = rtt_us {
            write!(output, "  rtt: {:.2}ms", rtt as f64 / 1000.0)
                .expect("writing to String cannot fail");
        }
    }

    output.push('\n');
    output
}

/// Feedback-only stall line (issue #70 follow-up, scripted half of issue
/// #93): printed while full `Interval` reports have stalled but 2 Hz UDP
/// receiver feedback keeps arriving. Shows cumulative loss — the feedback
/// path knows packet counts, not bytes, so fabricating a throughput row
/// would be dishonest; this line says exactly what is known.
pub fn output_feedback_plain(
    timestamp: &str,
    packets_lost: u64,
    lost_percent: Option<f64>,
) -> String {
    match lost_percent {
        Some(pct) => format!(
            "[{}]  (reports stalled)  lost so far: {} ({:.1}%)\n",
            timestamp, packets_lost, pct
        ),
        None => format!(
            "[{}]  (reports stalled)  lost so far: {}\n",
            timestamp, packets_lost
        ),
    }
}

/// Bidir UDP quality as fixed-width columns, so streaming rows keep their
/// shape as values grow. `--` marks a receiver without a reading yet.
fn directional_quality(
    udp_send: Option<&UdpDirectionProgress>,
    udp_recv: Option<&UdpDirectionProgress>,
    upload_jitter_fallback: Option<f64>,
) -> String {
    let loss = |m: Option<&UdpDirectionProgress>| match m.and_then(|m| m.progress.lost_percent()) {
        Some(percent) => format!("{percent:>5.1}%"),
        None => format!("{:>6}", "--%"),
    };
    let jitter = |jitter: Option<f64>| match jitter {
        Some(jitter) => format!("{jitter:>5.2}ms"),
        None => format!("{:>7}", "--ms"),
    };
    format!(
        "  loss ↑{} ↓{}  jitter ↑{} ↓{}",
        loss(udp_send),
        loss(udp_recv),
        jitter(
            udp_send
                .and_then(|m| m.jitter_ms)
                .or(upload_jitter_fallback)
        ),
        jitter(udp_recv.and_then(|m| m.jitter_ms)),
    )
}

/// Bidir UDP rows replace the single upload-only `jitter`/`lost` columns
/// with per-direction readings rather than printing upload twice.
#[allow(clippy::too_many_arguments)]
pub fn output_interval_plain_with_udp_directions(
    timestamp: &str,
    elapsed_secs: f64,
    throughput_mbps: f64,
    bytes: u64,
    retransmits: Option<u64>,
    jitter_ms: Option<f64>,
    lost: Option<u64>,
    rtt_us: Option<u32>,
    udp_send: Option<&UdpDirectionProgress>,
    udp_recv: Option<&UdpDirectionProgress>,
) -> String {
    if udp_send.is_none() && udp_recv.is_none() {
        return output_interval_plain(
            timestamp,
            elapsed_secs,
            throughput_mbps,
            bytes,
            retransmits,
            jitter_ms,
            lost,
            rtt_us,
        );
    }
    let mut output = output_interval_plain(
        timestamp,
        elapsed_secs,
        throughput_mbps,
        bytes,
        None,
        None,
        None,
        None,
    );
    output.pop();
    output.push_str(&directional_quality(udp_send, udp_recv, jitter_ms));
    output.push('\n');
    output
}

pub fn output_directional_feedback_plain(
    timestamp: &str,
    _elapsed_secs: f64,
    udp_send: Option<&UdpDirectionProgress>,
    udp_recv: Option<&UdpDirectionProgress>,
) -> String {
    format!(
        "[{timestamp}]  (reports stalled){}\n",
        directional_quality(udp_send, udp_recv, None)
    )
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
                bytes_sent: None,
                bytes_received: None,
                throughput_send_mbps: None,
                throughput_recv_mbps: None,
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
            server_udp_send_errors: None,
            client_udp_send_errors: None,
            zero_byte_streams: None,
            udp_stats_send: None,
            udp_stats_recv: None,
        }
    }

    #[test]
    fn feedback_stall_line_shows_cumulative_loss() {
        let line = output_feedback_plain("00:00:07", 17_000, Some(20.04));
        assert_eq!(
            line,
            "[00:00:07]  (reports stalled)  lost so far: 17000 (20.0%)\n"
        );
        // No receiver feedback decoded yet: counts without a percent.
        let line = output_feedback_plain("00:00:07", 0, None);
        assert_eq!(line, "[00:00:07]  (reports stalled)  lost so far: 0\n");
    }

    #[test]
    fn test_output_plain_reports_udp_send_errors() {
        let mut result = make_result_with_tcp_info();
        assert!(!output_plain(&result, false).contains("UDP send error"));
        result.server_udp_send_errors = Some(crate::protocol::UdpSendErrors {
            count: 42,
            last_error: "Connection refused (os error 111)".to_string(),
        });
        assert!(output_plain(&result, false).contains(
            "  Warning: Server reported 42 UDP send errors (last: Connection refused (os error 111))\n"
        ));
    }

    #[test]
    fn directional_feedback_keeps_unknown_upload_distinct_from_clean_download() {
        let recv = UdpDirectionProgress {
            progress: crate::protocol::UdpIntervalProgress {
                packets_received: 10,
                packets_lost: 0,
            },
            jitter_ms: Some(0.0),
        };
        let output = output_directional_feedback_plain("2.0", 2.0, None, Some(&recv));
        assert_eq!(
            output,
            "[2.0]  (reports stalled)  loss ↑   --% ↓  0.0%  jitter ↑   --ms ↓ 0.00ms\n"
        );
        assert!(!output.contains("Mbps"));
        let interval = output_interval_plain_with_udp_directions(
            "2.0",
            2.0,
            8.0,
            1000,
            None,
            None,
            None,
            None,
            None,
            Some(&recv),
        );
        assert!(interval.ends_with("loss ↑   --% ↓  0.0%  jitter ↑   --ms ↓ 0.00ms\n"));
        assert!(!interval.contains("lost:"), "upload must not print twice");
        // Columns stay put as readings grow (and as unknowns resolve).
        let lossy = UdpDirectionProgress {
            progress: crate::protocol::UdpIntervalProgress {
                packets_received: 877,
                packets_lost: 123,
            },
            jitter_ms: Some(12.34),
        };
        let grown = output_interval_plain_with_udp_directions(
            "2.0",
            2.0,
            8.0,
            1000,
            None,
            None,
            None,
            None,
            Some(&lossy),
            Some(&lossy),
        );
        assert_eq!(
            grown.chars().count(),
            interval.chars().count(),
            "{grown}{interval}"
        );
    }

    #[test]
    fn final_plain_shows_both_udp_directions_and_one_stream_split() {
        let mut result = make_result_with_tcp_info();
        result.tcp_info = None;
        result.udp_stats_send = Some(Box::new(UdpStats {
            packets_sent: 100,
            packets_received: 80,
            lost: 20,
            lost_percent: 20.0,
            jitter_ms: 0.5,
            out_of_order: 0,
            jitter_max_ms: None,
            packet_size: None,
        }));
        result.udp_stats_recv = Some(Box::new(UdpStats {
            packets_sent: 100,
            packets_received: 100,
            lost: 0,
            lost_percent: 0.0,
            jitter_ms: 0.0,
            out_of_order: 0,
            jitter_max_ms: None,
            packet_size: None,
        }));
        result.streams[0].bytes_sent = Some(400);
        result.streams[0].bytes_received = Some(624);
        result.streams[0].throughput_send_mbps = Some(3.0);
        result.streams[0].throughput_recv_mbps = Some(5.0);
        let output = output_plain(&result, false);
        assert_eq!(output, include_str!("testdata/plain-udp-bidir.txt"));
        assert!(output.contains("Upload UDP Stats:\n"));
        assert!(output.contains("Lost:             20 (20.00%)"));
        assert!(output.contains("Download UDP Stats:\n"));
        assert!(output.contains("Lost:             0 (0.00%)"));
        assert!(output.contains("Per-Stream Results:"));
        assert!(output.contains("Upload: 400 B @ 3.0 Mbps  Download: 624 B @ 5.0 Mbps"));
        result.udp_stats_send = None;
        assert!(output_plain(&result, false).contains("Upload UDP Stats:\n    Not reported\n"));
    }

    #[test]
    fn mtu_probe_report_keeps_exact_text_format() {
        use crate::probe::SizeResult;
        let mut result = make_result_with_tcp_info();
        result.mtu_probe = Some(Box::new(MtuProbeReport {
            forward_max_payload: Some(1472),
            reverse_max_payload: None,
            forward_path_mtu: Some(1500),
            reverse_path_mtu: None,
            ipv6: false,
            sizes: vec![
                SizeResult {
                    payload: 1472,
                    forward_ok: 2,
                    reverse_ok: 0,
                    attempts: 3,
                    forward: SizeVerdict::Pass,
                    reverse: SizeVerdict::Fail,
                },
                SizeResult {
                    payload: 8972,
                    forward_ok: 0,
                    reverse_ok: 0,
                    attempts: 3,
                    forward: SizeVerdict::LocalLimit,
                    reverse: SizeVerdict::Untested,
                },
            ],
        }));
        assert_eq!(
            output_plain(&result, false),
            include_str!("testdata/plain-mtu.txt")
        );
    }

    #[test]
    fn bidir_per_stream_lines_fit_80_columns() {
        // Widest realistic values: a three-digit stream index, totals just
        // under 1 TB (rounds to "1024.00 GB"), rates rounding to
        // "1000.00 Gbps", and six-digit retransmit counts.
        let mut result = make_result_with_tcp_info();
        let stream = StreamResult {
            id: 127,
            bytes: (1 << 40) - 1,
            throughput_mbps: 999_999.0,
            retransmits: Some(999_999),
            jitter_ms: None,
            lost: None,
            bytes_sent: Some((1 << 40) - 1),
            bytes_received: Some((1 << 40) - 1),
            throughput_send_mbps: Some(999_999.0),
            throughput_recv_mbps: Some(999_999.0),
        };
        result.streams = vec![stream.clone(), stream];
        let output = output_plain(&result, false);
        assert!(output.contains("1000.00 Gbps"), "{output}");
        assert!(output.contains("        Upload: 1024.00 GB @ 1000.00 Gbps  Download:"));
        for line in output.lines() {
            assert!(
                line.chars().count() <= 80,
                "{} cols: {line}",
                line.chars().count()
            );
        }
    }

    #[test]
    fn test_output_plain_sender_tcp_label_tcp() {
        let output = output_plain(&make_result_with_tcp_info(), false);
        assert_eq!(output, include_str!("testdata/plain-tcp.txt"));
        assert!(!output.contains("initial subflow"));
    }

    #[test]
    fn test_output_plain_sender_tcp_label_mptcp() {
        let output = output_plain(&make_result_with_tcp_info(), true);
        assert_eq!(output, include_str!("testdata/plain-mptcp.txt"));
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
        // Columns are right-aligned so the numbers don't slide sideways
        // between samples; the counts keep their own fixed width.
        assert!(output.contains("rtx:     5"), "unexpected line: {output}");
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
        assert!(
            output.contains("jitter:    1.42ms"),
            "unexpected line: {output}"
        );
        assert!(output.contains("lost: 3"));
        assert!(!output.contains("rtx:"));
        assert!(!output.contains("rtt:"));
    }

    #[test]
    fn interval_columns_stay_put_across_magnitudes() {
        // Streaming output is read as a column, one line per second. A sample
        // formatted "900 B" / "200.0 Mbps" and one formatted "150.00 MB" /
        // "1.20 Gbps" must land their later columns in the same place, or the
        // numbers visibly slide sideways as the transfer ramps up.
        let small = output_interval_plain("1.001", 1.0, 200.0, 900, Some(0), None, None, Some(120));
        let large = output_interval_plain(
            "2.001",
            2.0,
            1_200.0,
            157_286_400,
            Some(0),
            None,
            None,
            Some(120),
        );

        let rtx_col = |line: &str| line.find("rtx:").expect("rtx column present");
        assert_eq!(
            rtx_col(&small),
            rtx_col(&large),
            "columns drifted:\n{small}\n{large}"
        );

        let rtt_col = |line: &str| line.find("rtt:").expect("rtt column present");
        assert_eq!(
            rtt_col(&small),
            rtt_col(&large),
            "columns drifted:\n{small}\n{large}"
        );
    }
}
