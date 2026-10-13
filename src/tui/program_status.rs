//! Program Status Protocol (OSC 7501) reports, so terminals that implement
//! it can show test status in tab headers and session pickers. Terminals
//! must ignore unknown OSCs, so reports go out without feature detection.
//! Spec: <https://www.superlogical.com/rex/docs/build/program-status> (rev 0.4).

use std::io::Write;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::app::{App, AppState};
use crate::protocol::{Direction, Protocol};
use crate::stats::mbps_to_human;

/// Decoded `msg` limit. A terminal discards an oversized report whole.
const MSG_MAX_BYTES: usize = 2048;
/// Same-state updates (rate, progress) go out at most this often.
const MIN_UPDATE_INTERVAL: Duration = Duration::from_secs(1);

/// One OSC 7501 report for the root record. Each report replaces the
/// record, so `app` is repeated every time.
pub fn report(state: &str, progress: Option<u8>, msg: Option<&str>) -> String {
    let mut out = format!("\x1b]7501;state={state}:app=xfr");
    if let Some(progress) = progress {
        out += &format!(":progress={}", progress.min(100));
    }
    if let Some(msg) = msg {
        // A control character anywhere in the decoded text (newline, ESC,
        // BEL, C1) makes the terminal refuse the report, so flatten to one line.
        let mut msg = msg
            .replace(char::is_control, " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut end = msg.len().min(MSG_MAX_BYTES);
        while !msg.is_char_boundary(end) {
            end -= 1;
        }
        msg.truncate(end);
        out += ":msg=";
        out += &STANDARD.encode(msg);
    }
    out + "\x1b\\"
}

/// `(state, progress, msg)` for the client TUI.
pub fn client_status(app: &App) -> (&'static str, Option<u8>, String) {
    // `on_connected` flips straight to Running, so "connecting" is a
    // running test that has heard neither the ServerHello nor an interval.
    let connecting = app.state == AppState::Connecting
        || (app.state == AppState::Running
            && app.server_version.is_none()
            && app.throughput_history.is_empty());
    if connecting {
        return (
            "working",
            None,
            format!("Connecting to {}:{}", app.host, app.port),
        );
    }
    match app.state {
        AppState::Connecting | AppState::Running => {
            let bounded = app.byte_budget.is_some_and(|b| b > 0) || !app.is_infinite();
            let progress = bounded.then(|| app.progress_percent() as u8);
            ("working", progress, rate(app, app.current_throughput_mbps))
        }
        AppState::Paused => ("idle", None, "Paused".to_string()),
        AppState::Completed => {
            // Same quality figure the stats panel shows: loss for UDP,
            // retransmits otherwise.
            let mut msg = rate(app, app.average_throughput_mbps);
            if app.protocol != Protocol::Udp {
                msg += &format!(", {} retransmits", app.total_retransmits);
            } else if let Some(loss) = app.udp_lost_percent {
                msg += &format!(", {loss:.1}% loss");
            }
            ("done", None, msg)
        }
        AppState::Error => ("error", None, app.error.clone().unwrap_or_default()),
    }
}

/// `TCP ↓ 941.0 Mbps`, with the panel's `↑ send / ↓ recv` split for bidir.
fn rate(app: &App, mbps: f64) -> String {
    let proto = app.protocol;
    match app.direction {
        Direction::Upload => format!("{proto} ↑ {}", mbps_to_human(mbps)),
        Direction::Download => format!("{proto} ↓ {}", mbps_to_human(mbps)),
        Direction::Bidir if app.throughput_send_mbps > 0.0 || app.throughput_recv_mbps > 0.0 => {
            format!(
                "{proto} ↑ {} / ↓ {}",
                mbps_to_human(app.throughput_send_mbps),
                mbps_to_human(app.throughput_recv_mbps)
            )
        }
        Direction::Bidir => format!("{proto} ↑↓ {}", mbps_to_human(mbps)),
    }
}

/// Sends reports only when they change. Disabled when stdout is not a
/// terminal: the bytes would land in whatever stdout was redirected to.
pub struct StatusReporter {
    enabled: bool,
    /// Last report sent: its state, bytes, and when.
    last: Option<(&'static str, String, Instant)>,
}

impl StatusReporter {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            last: None,
        }
    }

    /// State changes go out immediately; same-state changes at most once per
    /// [`MIN_UPDATE_INTERVAL`]. Call after `terminal.draw` returns, never
    /// mid-frame. Write errors are ignored: the next draw surfaces them.
    pub fn update(
        &mut self,
        out: &mut impl Write,
        state: &'static str,
        progress: Option<u8>,
        msg: Option<&str>,
    ) {
        if !self.enabled {
            return;
        }
        let report = report(state, progress, msg);
        if let Some((last_state, last, sent_at)) = &self.last
            && (*last == report
                || (*last_state == state && sent_at.elapsed() < MIN_UPDATE_INTERVAL))
        {
            return;
        }
        let _ = out.write_all(report.as_bytes()).and_then(|()| out.flush());
        self.last = Some((state, report, Instant::now()));
    }

    /// On exit, a `done` or `error` record is the result and should outlive
    /// the process. Anything else (a paused `idle`, a cancelled run's
    /// `working`) is removed.
    pub fn finish(&mut self, out: &mut impl Write) {
        if self.enabled
            && self
                .last
                .as_ref()
                .is_some_and(|(state, ..)| !matches!(*state, "done" | "error"))
        {
            let _ = out
                .write_all(report("clear", None, None).as_bytes())
                .and_then(|()| out.flush());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{TestResult, TimestampFormat};

    fn decoded_msg(report: &str) -> String {
        let b64 = report
            .split(":msg=")
            .nth(1)
            .unwrap()
            .trim_end_matches("\x1b\\");
        String::from_utf8(STANDARD.decode(b64).unwrap()).unwrap()
    }

    fn app(direction: Direction, duration: Duration) -> App {
        App::with_theme_name(
            "10.0.0.1".into(),
            5201,
            Protocol::Tcp,
            direction,
            1,
            duration,
            None,
            TimestampFormat::default(),
            "default",
        )
    }

    fn running(mut app: App) -> App {
        app.on_connected();
        app.set_server_version("0.10.1".into());
        app
    }

    #[test]
    fn report_exact_bytes() {
        assert_eq!(
            report("working", Some(42), Some("TCP ↓ 941.0 Mbps")),
            "\x1b]7501;state=working:app=xfr:progress=42:msg=VENQIOKGkyA5NDEuMCBNYnBz\x1b\\"
        );
        assert_eq!(
            report("clear", None, None),
            "\x1b]7501;state=clear:app=xfr\x1b\\"
        );
    }

    #[test]
    fn report_clamps_progress() {
        assert!(report("working", Some(250), None).contains(":progress=100\x1b\\"));
        assert!(report("working", Some(0), None).contains(":progress=0\x1b\\"));
    }

    #[test]
    fn report_strips_control_chars() {
        let r = report("error", None, Some("a\nb\x1b[31mc\x07d\u{9b}e\r\n"));
        assert_eq!(decoded_msg(&r), "a b [31mc d e");
        assert!(!decoded_msg(&r).chars().any(char::is_control));
    }

    #[test]
    fn report_truncates_msg_on_char_boundary() {
        // 3-byte chars: 2048 is not a multiple of 3, so the cut must back off.
        let msg = "€".repeat(1000);
        let decoded = decoded_msg(&report("done", None, Some(&msg)));
        assert_eq!(decoded.len(), 2046);
        assert_eq!(decoded, "€".repeat(682));
        let exact = "x".repeat(MSG_MAX_BYTES + 10);
        assert_eq!(
            decoded_msg(&report("done", None, Some(&exact))).len(),
            MSG_MAX_BYTES
        );
    }

    #[test]
    fn connecting_has_no_progress() {
        let mut a = app(Direction::Download, Duration::from_secs(10));
        a.on_connected();
        assert_eq!(
            client_status(&a),
            ("working", None, "Connecting to 10.0.0.1:5201".into())
        );
    }

    #[test]
    fn running_reports_progress_and_rate() {
        let mut a = running(app(Direction::Download, Duration::from_secs(10)));
        a.elapsed = Duration::from_millis(4_990);
        a.current_throughput_mbps = 941.0;
        assert_eq!(
            client_status(&a),
            ("working", Some(49), "TCP ↓ 941.0 Mbps".into())
        );
        a.direction = Direction::Upload;
        assert_eq!(client_status(&a).2, "TCP ↑ 941.0 Mbps");
    }

    #[test]
    fn byte_budget_progress() {
        let mut a = running(app(Direction::Upload, Duration::ZERO));
        a.byte_budget = Some(1000);
        a.total_bytes = 250;
        assert_eq!(client_status(&a).1, Some(25));
    }

    #[test]
    fn unbounded_run_omits_progress() {
        let a = running(app(Direction::Upload, Duration::ZERO));
        assert_eq!(client_status(&a).1, None);
        let (state, progress, msg) = client_status(&a);
        assert!(!report(state, progress, Some(&msg)).contains("progress"));
    }

    #[test]
    fn paused_completed_and_error_states() {
        let mut a = running(app(Direction::Download, Duration::from_secs(10)));
        a.toggle_pause();
        assert_eq!(client_status(&a), ("idle", None, "Paused".into()));

        a.toggle_pause();
        a.on_result(TestResult {
            id: String::new(),
            bytes_total: 1,
            duration_ms: 10_000,
            throughput_mbps: 941.0,
            streams: vec![],
            tcp_info: None,
            udp_stats: None,
            bytes_sent: None,
            bytes_received: None,
            throughput_send_mbps: None,
            throughput_recv_mbps: None,
            mtu_probe: None,
        });
        assert_eq!(
            client_status(&a),
            ("done", None, "TCP ↓ 941.0 Mbps, 0 retransmits".into())
        );

        a.protocol = Protocol::Udp;
        a.udp_lost_percent = Some(0.02);
        assert_eq!(client_status(&a).2, "UDP ↓ 941.0 Mbps, 0.0% loss");

        a.on_error("Connection refused".into());
        assert_eq!(
            client_status(&a),
            ("error", None, "Connection refused".into())
        );
    }

    fn quit_output(states: &[&'static str]) -> String {
        let mut reporter = StatusReporter::new(true);
        let mut out = Vec::new();
        for state in states {
            reporter.update(&mut out, state, None, None);
        }
        reporter.finish(&mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn quit_clears_unless_result_is_showing() {
        assert!(quit_output(&["working", "idle"]).ends_with("state=clear:app=xfr\x1b\\"));
        assert!(quit_output(&["working"]).ends_with("state=clear:app=xfr\x1b\\"));
        assert!(!quit_output(&["working", "done"]).contains("clear"));
        assert!(!quit_output(&["working", "error"]).contains("clear"));
        assert_eq!(quit_output(&[]), "");
    }

    #[test]
    fn reporter_skips_repeats_and_throttles_same_state() {
        let mut reporter = StatusReporter::new(true);
        let mut out = Vec::new();
        reporter.update(&mut out, "working", Some(1), None);
        reporter.update(&mut out, "working", Some(1), None);
        reporter.update(&mut out, "working", Some(2), None);
        assert_eq!(
            String::from_utf8_lossy(&out).matches("\x1b]7501").count(),
            1
        );
        reporter.update(&mut out, "done", None, None);
        assert_eq!(
            String::from_utf8_lossy(&out).matches("\x1b]7501").count(),
            2
        );
    }

    #[test]
    fn disabled_reporter_writes_nothing() {
        let mut reporter = StatusReporter::new(false);
        let mut out = Vec::new();
        reporter.update(&mut out, "working", None, Some("x"));
        reporter.finish(&mut out);
        assert!(out.is_empty());
    }
}
