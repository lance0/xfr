//! TUI application state machine

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::client::TestProgress;
use crate::protocol::{Direction, Protocol, TestResult, TimestampFormat};

use super::settings::SettingsState;
use super::theme::Theme;

const SPARKLINE_HISTORY: usize = 60;
const LOG_HISTORY: usize = 100;
// Rolling window for aggregate jitter display. Progress events fire at 1Hz,
// so 10 samples ≈ 10s smoothing window — long enough to damp per-second
// noise, short enough to still track real network changes.
const JITTER_HISTORY_SAMPLES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Connecting,
    Running,
    Paused,
    Completed,
    Error,
}

pub struct StreamData {
    pub id: u8,
    pub bytes: u64,
    pub throughput_mbps: f64,
    pub retransmits: u64,
    pub jitter_ms: Option<f64>,
}

#[derive(Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub message: String,
}

pub struct App {
    pub state: AppState,
    pub host: String,
    pub port: u16,
    pub protocol: Protocol,
    pub direction: Direction,
    pub streams_count: u8,
    pub duration: Duration,
    pub bitrate: Option<u64>,

    pub elapsed: Duration,
    pub total_bytes: u64,
    pub current_throughput_mbps: f64,
    pub throughput_history: VecDeque<f64>,
    // Rolling history of aggregate jitter samples so the TUI can show a
    // smoothed reading instead of the noisy per-second snapshot.
    pub jitter_history: VecDeque<f64>,
    pub streams: Vec<StreamData>,

    // Bidirectional split stats (populated from AggregateInterval/TestResult
    // when the server reports per-direction totals). Zero for unidirectional tests.
    pub bidir_bytes_sent: u64,
    pub bidir_bytes_received: u64,
    pub throughput_send_mbps: f64,
    pub throughput_recv_mbps: f64,

    pub total_retransmits: u64,
    pub rtt_us: u32,
    pub cwnd: u32,

    // UDP stats
    pub udp_jitter_ms: f64,
    pub udp_lost_percent: f64,
    pub udp_packets_sent: u64,
    pub udp_packets_lost: u64,

    pub result: Option<TestResult>,
    pub error: Option<String>,

    pub start_time: Option<Instant>,
    pub show_help: bool,
    pub show_streams: bool,
    pub timestamp_format: TimestampFormat,
    pub theme: Theme,
    pub theme_index: usize,

    // Settings modal state
    pub settings: SettingsState,

    // History log
    pub history: VecDeque<LogEntry>,
    pub average_throughput_mbps: f64,
    throughput_sum: f64,
    throughput_count: u64,

    // Event tracking for history
    peak_throughput_mbps: f64,
    prev_retransmits: u64,
    prev_udp_lost: u64,

    // Update notification
    pub update_available: Option<String>,

    /// Server-reported version (e.g. "xfr/0.9.8") captured from the `Hello`
    /// handshake. None until the handshake completes; surfaced in the UI so
    /// cross-version test pairings are visible at a glance.
    pub server_version: Option<String>,

    /// Wall-clock instant at which the current pause started. `Some` while
    /// `state == Paused`, `None` otherwise. On resume we advance `start_time`
    /// forward by the paused duration so `tick()`'s `start_time.elapsed()`
    /// naturally excludes time spent paused.
    pause_started_at: Option<Instant>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host: String,
        port: u16,
        protocol: Protocol,
        direction: Direction,
        streams: u8,
        duration: Duration,
        bitrate: Option<u64>,
        timestamp_format: TimestampFormat,
        theme: Theme,
    ) -> Self {
        Self {
            state: AppState::Connecting,
            host,
            port,
            protocol,
            direction,
            streams_count: streams,
            duration,
            bitrate,

            elapsed: Duration::ZERO,
            total_bytes: 0,
            current_throughput_mbps: 0.0,
            throughput_history: VecDeque::with_capacity(SPARKLINE_HISTORY),
            jitter_history: VecDeque::with_capacity(JITTER_HISTORY_SAMPLES),
            bidir_bytes_sent: 0,
            bidir_bytes_received: 0,
            throughput_send_mbps: 0.0,
            throughput_recv_mbps: 0.0,
            streams: (0..streams)
                .map(|id| StreamData {
                    id,
                    bytes: 0,
                    throughput_mbps: 0.0,
                    retransmits: 0,
                    jitter_ms: None,
                })
                .collect(),

            total_retransmits: 0,
            rtt_us: 0,
            cwnd: 0,

            udp_jitter_ms: 0.0,
            udp_lost_percent: 0.0,
            udp_packets_sent: 0,
            udp_packets_lost: 0,

            result: None,
            error: None,

            start_time: None,
            show_help: false,
            show_streams: false,
            timestamp_format,
            theme,
            theme_index: 0,

            settings: SettingsState::new(0, streams, protocol, duration, direction),

            history: VecDeque::with_capacity(LOG_HISTORY),
            average_throughput_mbps: 0.0,
            throughput_sum: 0.0,
            throughput_count: 0,

            peak_throughput_mbps: 0.0,
            prev_retransmits: 0,
            prev_udp_lost: 0,

            update_available: None,
            server_version: None,
            pause_started_at: None,
        }
    }

    /// Add a log entry with current timestamp
    pub fn log(&mut self, message: impl Into<String>) {
        let timestamp = if let Some(start) = self.start_time {
            let elapsed = start.elapsed();
            format!(
                "{:02}:{:02}:{:02}",
                elapsed.as_secs() / 3600,
                (elapsed.as_secs() % 3600) / 60,
                elapsed.as_secs() % 60
            )
        } else {
            "00:00:00".to_string()
        };

        self.history.push_front(LogEntry {
            timestamp,
            message: message.into(),
        });

        if self.history.len() > LOG_HISTORY {
            self.history.pop_back();
        }
    }

    /// Create app with theme by name, setting theme_index appropriately
    #[allow(clippy::too_many_arguments)]
    pub fn with_theme_name(
        host: String,
        port: u16,
        protocol: Protocol,
        direction: Direction,
        streams: u8,
        duration: Duration,
        bitrate: Option<u64>,
        timestamp_format: TimestampFormat,
        theme_name: &str,
    ) -> Self {
        let theme_list = Theme::list();
        let theme_index = theme_list
            .iter()
            .position(|&t| t == theme_name)
            .unwrap_or(0);
        let theme = Theme::by_name(theme_name);

        let mut app = Self::new(
            host,
            port,
            protocol,
            direction,
            streams,
            duration,
            bitrate,
            timestamp_format,
            theme,
        );
        app.theme_index = theme_index;
        app.settings.theme_index = theme_index;
        app
    }

    /// Cycle to the next theme
    pub fn cycle_theme(&mut self) {
        let theme_list = Theme::list();
        self.theme_index = (self.theme_index + 1) % theme_list.len();
        self.theme = Theme::by_name(theme_list[self.theme_index]);
        self.settings.theme_index = self.theme_index;
    }

    /// Set theme by index (from settings modal)
    pub fn set_theme_index(&mut self, index: usize) {
        let theme_list = Theme::list();
        if index < theme_list.len() {
            self.theme_index = index;
            self.theme = Theme::by_name(theme_list[index]);
            self.settings.theme_index = index;
        }
    }

    /// Get current theme name
    pub fn theme_name(&self) -> &str {
        self.theme.name()
    }

    pub fn on_connected(&mut self) {
        self.state = AppState::Running;
        self.start_time = Some(Instant::now());
        self.log("Connected to server.");
    }

    /// Refresh `elapsed` from the local wall clock. Called from the TUI loop
    /// once per iteration so the counter stays live even when server
    /// `Interval` progress messages are delayed (e.g. packet-drop bursts on
    /// the control channel). `elapsed` is wall-clock-authoritative during
    /// Running — `on_progress` does NOT update it (doing so was a visual
    /// no-op since the next tick immediately overwrote the server's value).
    /// `on_result` pins `self.duration` once completed.
    pub fn tick(&mut self) {
        if self.state == AppState::Running
            && let Some(start) = self.start_time
        {
            self.elapsed = start.elapsed();
        }
    }

    /// Record the server's advertised version (from the `Hello` handshake).
    /// Stored for UI display only. The string is sanitized before storage —
    /// see [`sanitize_server_version`] — since it crosses a network trust
    /// boundary and lands in a terminal. A hostile or compromised server
    /// could otherwise smuggle terminal escape sequences or an oversized
    /// payload into our display.
    pub fn set_server_version(&mut self, version: String) {
        self.server_version = Some(sanitize_server_version(&version));
    }

    pub fn on_progress(&mut self, progress: TestProgress) {
        // `elapsed` is intentionally NOT written here. `tick()` owns it during
        // Running from the local wall clock (pause-aware), so the TUI stays
        // live between progress messages. Writing from `progress.elapsed_ms`
        // on each message caused the next tick() to immediately overwrite it
        // anyway, producing a one-frame flash of the server-authoritative
        // value that users couldn't perceive. `on_result()` pins the final
        // once the test completes.
        self.total_bytes = progress.total_bytes;
        self.current_throughput_mbps = progress.throughput_mbps;

        // Bidirectional split: when the server reports per-direction counts
        // (all four Some), surface them to the UI so the live ↑/↓ panel works
        // during the test rather than only after it finishes.
        if let (Some(sent), Some(recv), Some(ts), Some(tr)) = (
            progress.bytes_sent,
            progress.bytes_received,
            progress.throughput_send_mbps,
            progress.throughput_recv_mbps,
        ) {
            self.bidir_bytes_sent += sent;
            self.bidir_bytes_received += recv;
            self.throughput_send_mbps = ts;
            self.throughput_recv_mbps = tr;
        }

        // Update sparkline history
        self.throughput_history.push_back(progress.throughput_mbps);
        if self.throughput_history.len() > SPARKLINE_HISTORY {
            self.throughput_history.pop_front();
        }

        // Update stream data
        // Intervals are sent every second, so throughput = bytes * 8 / 1_000_000
        let mut total_jitter = 0.0;
        let mut total_lost = 0u64;
        let mut jitter_count = 0;

        for interval in &progress.streams {
            if let Some(stream) = self.streams.get_mut(interval.id as usize) {
                stream.bytes = interval.bytes;
                // Use 1-second interval for throughput calculation (intervals are 1s apart)
                stream.throughput_mbps = (interval.bytes as f64 * 8.0) / 1_000_000.0;
                stream.retransmits = interval.retransmits.unwrap_or(0);
                stream.jitter_ms = interval.jitter_ms;
            }

            // Accumulate UDP stats from intervals
            if let Some(jitter) = interval.jitter_ms {
                total_jitter += jitter;
                jitter_count += 1;
            }
            if let Some(lost) = interval.lost {
                total_lost += lost;
            }
        }

        // Use local TCP_INFO retransmits when available (sender-side), otherwise sum from server
        if let Some(total) = progress.total_retransmits {
            self.total_retransmits = total;
        } else {
            self.total_retransmits = self.streams.iter().map(|s| s.retransmits).sum();
        }

        // Update live TCP_INFO from interval data
        if let Some(rtt) = progress.rtt_us {
            self.rtt_us = rtt;
        }
        if let Some(cwnd) = progress.cwnd {
            self.cwnd = cwnd;
        }

        // Update UDP stats (average jitter across streams) and push to the
        // rolling window so the stats panel can display a smoothed value.
        if jitter_count > 0 {
            self.udp_jitter_ms = total_jitter / jitter_count as f64;
            self.jitter_history.push_back(self.udp_jitter_ms);
            if self.jitter_history.len() > JITTER_HISTORY_SAMPLES {
                self.jitter_history.pop_front();
            }
        }
        self.udp_packets_lost = total_lost;

        // Track average throughput
        if progress.throughput_mbps > 0.0 {
            self.throughput_sum += progress.throughput_mbps;
            self.throughput_count += 1;
            self.average_throughput_mbps = self.throughput_sum / self.throughput_count as f64;
        }

        // Log significant events
        self.detect_events(progress.throughput_mbps);
    }

    /// Detect and log significant events for history
    fn detect_events(&mut self, throughput_mbps: f64) {
        // Peak throughput (only log if 10%+ above previous peak, after warmup)
        if self.throughput_count > 2 && throughput_mbps > self.peak_throughput_mbps * 1.1 {
            self.peak_throughput_mbps = throughput_mbps;
            self.log(format!("Peak: {:.0} Mbps", throughput_mbps));
        } else if throughput_mbps > self.peak_throughput_mbps {
            self.peak_throughput_mbps = throughput_mbps;
        }

        // Retransmit spike (TCP)
        if self.total_retransmits > self.prev_retransmits {
            let delta = self.total_retransmits - self.prev_retransmits;
            if delta >= 10 {
                self.log(format!("Retransmits: +{}", delta));
            }
        }
        self.prev_retransmits = self.total_retransmits;

        // UDP packet loss
        if self.udp_packets_lost > self.prev_udp_lost {
            let delta = self.udp_packets_lost - self.prev_udp_lost;
            if delta >= 5 {
                self.log(format!("UDP loss: +{} packets", delta));
            }
        }
        self.prev_udp_lost = self.udp_packets_lost;
    }

    pub fn on_result(&mut self, result: TestResult) {
        self.state = AppState::Completed;
        self.elapsed = self.duration; // Show full duration on completion

        // Sum retransmits from streams (captured after transfer, accurate for download mode)
        self.total_retransmits = result.streams.iter().filter_map(|s| s.retransmits).sum();

        // Use tcp_info for RTT and cwnd (connection-level stats)
        if let Some(tcp_info) = &result.tcp_info {
            self.rtt_us = tcp_info.rtt_us;
            self.cwnd = tcp_info.cwnd;
        }
        if let Some(udp_stats) = &result.udp_stats {
            self.udp_jitter_ms = udp_stats.jitter_ms;
            self.udp_lost_percent = udp_stats.lost_percent;
            self.udp_packets_sent = udp_stats.packets_sent;
            self.udp_packets_lost = udp_stats.lost;
        }
        // Bidirectional split stats: populated only by servers that know to
        // report them. Older servers leave these None, in which case we keep
        // the zero defaults and the UI falls back to the combined throughput.
        if let (Some(sent), Some(recv), Some(ts), Some(tr)) = (
            result.bytes_sent,
            result.bytes_received,
            result.throughput_send_mbps,
            result.throughput_recv_mbps,
        ) {
            self.bidir_bytes_sent = sent;
            self.bidir_bytes_received = recv;
            self.throughput_send_mbps = ts;
            self.throughput_recv_mbps = tr;
        }
        self.result = Some(result);
        self.log(format!(
            "Test completed. Avg: {:.0} Mbps.",
            self.average_throughput_mbps
        ));
    }

    pub fn on_error(&mut self, error: String) {
        self.state = AppState::Error;
        self.log(format!("Error: {}", &error));
        self.error = Some(error);
    }

    pub fn toggle_pause(&mut self) {
        match self.state {
            AppState::Running => {
                self.state = AppState::Paused;
                self.pause_started_at = Some(Instant::now());
                self.log("Test paused");
            }
            AppState::Paused => {
                // Shift start_time forward by the paused duration so the
                // elapsed counter recomputed by `tick()` excludes the pause.
                // Server's own `elapsed_ms` already excludes pause time, so
                // this keeps the two sources in agreement and avoids a
                // visible forward-then-backward jump at resume.
                if let (Some(paused_at), Some(start)) =
                    (self.pause_started_at.take(), self.start_time.as_mut())
                {
                    *start += paused_at.elapsed();
                }
                self.state = AppState::Running;
                self.log("Test resumed");
            }
            _ => {}
        }
    }

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    pub fn toggle_streams(&mut self) {
        // Only toggle if multiple streams exist
        if self.streams_count > 1 {
            self.show_streams = !self.show_streams;
        }
    }

    /// Check if test has infinite duration
    pub fn is_infinite(&self) -> bool {
        self.duration == Duration::ZERO
    }

    pub fn progress_percent(&self) -> f64 {
        if self.is_infinite() {
            0.0
        } else {
            (self.elapsed.as_secs_f64() / self.duration.as_secs_f64() * 100.0).min(100.0)
        }
    }

    pub fn time_remaining(&self) -> Duration {
        if self.is_infinite() {
            Duration::ZERO
        } else {
            self.duration.saturating_sub(self.elapsed)
        }
    }

    pub fn max_throughput(&self) -> f64 {
        self.throughput_history
            .iter()
            .cloned()
            .fold(0.0f64, f64::max)
    }

    /// Mean of the last ~10s of jitter samples. Returns 0.0 before any
    /// samples have arrived (very early in the test or non-UDP runs).
    pub fn avg_jitter_ms(&self) -> f64 {
        if self.jitter_history.is_empty() {
            0.0
        } else {
            self.jitter_history.iter().sum::<f64>() / self.jitter_history.len() as f64
        }
    }

    /// Jitter values for the UDP stats panel.
    ///
    /// - `primary` is the latest per-interval aggregate while running, or
    ///   the server's authoritative final once the test has completed.
    /// - `smoothed` is the 10-second rolling mean, shown alongside `primary`
    ///   only during the running state so users can see both the
    ///   instantaneous reading and the smoothed view. None when completed
    ///   (the final value is the authoritative one — a smoothed comparison
    ///   would just be noise at that point).
    ///
    /// Surfacing both resolves the cognitive friction where the rolling mean
    /// could stay above a sample's minimum (issue #48 follow-up).
    pub fn jitter_display(&self) -> JitterDisplay {
        if self.state == AppState::Completed {
            JitterDisplay {
                primary: self.udp_jitter_ms,
                smoothed: None,
            }
        } else {
            JitterDisplay {
                primary: self.udp_jitter_ms,
                smoothed: Some(self.avg_jitter_ms()),
            }
        }
    }
}

/// Display-safe normalization of a server-advertised version string.
///
/// The input comes from a remote `Hello` message and is rendered verbatim
/// into the user's terminal, so it needs to be treated as untrusted:
/// - non-printable / control bytes are stripped (prevents ANSI escape
///   injection that could clear the screen, move the cursor, spoof
///   content, or run the terminal's exotic OSC commands)
/// - the result is clamped to a short length (prevents a malicious peer
///   from monopolizing the Configuration panel row or blowing up render
///   cost)
/// - an empty result falls back to `(unknown)` so the UI still makes sense
pub fn sanitize_server_version(raw: &str) -> String {
    const MAX_LEN: usize = 32;
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LEN)
        .collect();
    if cleaned.is_empty() {
        "(unknown)".to_string()
    } else {
        cleaned
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JitterDisplay {
    /// The value to color-code and show first. Latest per-interval aggregate
    /// while running; server's authoritative final when completed.
    pub primary: f64,
    /// 10-second rolling mean — shown in parentheses alongside `primary`
    /// while running, `None` when the test is complete.
    pub smoothed: Option<f64>,
}

impl Default for App {
    fn default() -> Self {
        Self::new(
            "localhost".to_string(),
            5201,
            Protocol::Tcp,
            Direction::Upload,
            1,
            Duration::from_secs(10),
            None,
            TimestampFormat::default(),
            Theme::default(),
        )
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // App has too many fields to spread-construct in tests
mod tests {
    use super::*;

    #[test]
    fn jitter_display_returns_both_instant_and_smoothed_while_running() {
        let mut app = App::default();
        app.state = AppState::Running;
        // Simulate 3 per-second jitter samples: 2, 4, 6 ms → rolling mean 4.0.
        // Latest per-interval aggregate is 6.0 ms (what the server just sent).
        app.udp_jitter_ms = 6.0;
        app.jitter_history.extend([2.0, 4.0, 6.0]);

        let jd = app.jitter_display();
        assert!(
            (jd.primary - 6.0).abs() < f64::EPSILON,
            "primary should be latest per-interval aggregate 6.0, got {}",
            jd.primary
        );
        assert!(
            jd.smoothed.is_some(),
            "running state must include the smoothed mean alongside primary"
        );
        let avg = jd.smoothed.unwrap();
        assert!(
            (avg - 4.0).abs() < f64::EPSILON,
            "smoothed mean should be 4.0, got {avg}"
        );
    }

    #[test]
    fn jitter_display_uses_authoritative_final_on_complete() {
        let mut app = App::default();
        // Run-level final jitter from the server (via on_result) is different
        // from whatever the last 10s of progress samples averaged to — the
        // completed screen must show the authoritative final, and suppress
        // the smoothed companion so the display stays unambiguous.
        app.state = AppState::Completed;
        app.udp_jitter_ms = 1.23;
        app.jitter_history.extend([9.9, 9.9, 9.9]); // stale tail samples

        let jd = app.jitter_display();
        assert!(
            (jd.primary - 1.23).abs() < f64::EPSILON,
            "expected authoritative 1.23, got {}",
            jd.primary
        );
        assert_eq!(
            jd.smoothed, None,
            "completed state must not show a smoothed companion"
        );
    }

    #[test]
    fn avg_jitter_ms_is_zero_with_no_samples() {
        let app = App::default();
        assert_eq!(app.avg_jitter_ms(), 0.0);
    }

    #[test]
    fn tick_refreshes_elapsed_while_running() {
        // When Running with start_time set, tick() should update elapsed from
        // the wall clock so the UI stays live between progress messages. This
        // is the core fix for issue #62: progress-message starvation during
        // packet-drop bursts left `elapsed` stale.
        let mut app = App::default();
        app.state = AppState::Running;
        app.start_time = Some(Instant::now() - Duration::from_secs(3));
        app.elapsed = Duration::ZERO; // stale — what a drop burst leaves behind

        app.tick();

        assert!(
            app.elapsed >= Duration::from_secs(3),
            "expected elapsed ≥ 3s from wall clock, got {:?}",
            app.elapsed
        );
    }

    #[test]
    fn tick_is_noop_outside_running() {
        // Only Running should advance elapsed from wall clock. Other states
        // either don't have a meaningful elapsed (Connecting/Error) or pin
        // their own value (Completed sets duration, Paused should freeze).
        for state in [
            AppState::Connecting,
            AppState::Paused,
            AppState::Completed,
            AppState::Error,
        ] {
            let mut app = App::default();
            app.state = state;
            app.start_time = Some(Instant::now() - Duration::from_secs(5));
            app.elapsed = Duration::from_secs(42); // sentinel

            app.tick();

            assert_eq!(
                app.elapsed,
                Duration::from_secs(42),
                "tick() must not mutate elapsed in state {state:?}"
            );
        }
    }

    #[test]
    fn set_server_version_populates_field() {
        let mut app = App::default();
        assert!(app.server_version.is_none());
        app.set_server_version("xfr/0.9.8".to_string());
        assert_eq!(app.server_version.as_deref(), Some("xfr/0.9.8"));
    }

    #[test]
    fn sanitize_server_version_strips_control_bytes() {
        // ANSI escape sequences a hostile server could send to e.g. clear the
        // screen or move the cursor. All control bytes (ESC, CR, LF, NUL,
        // etc.) must be stripped before the string lands in the terminal.
        let dirty = "xfr/\x1b[2J\x1b[Hmalicious\r\n\x00";
        assert_eq!(sanitize_server_version(dirty), "xfr/[2J[Hmalicious");
    }

    #[test]
    fn sanitize_server_version_caps_length() {
        // A peer advertising a 10 KB version string must not monopolize the
        // Configuration panel row or blow up render cost.
        let long = "x".repeat(10_000);
        let cleaned = sanitize_server_version(&long);
        assert!(cleaned.chars().count() <= 32);
        assert!(cleaned.starts_with("xxxxx"));
    }

    #[test]
    fn sanitize_server_version_falls_back_for_empty_or_all_control() {
        assert_eq!(sanitize_server_version(""), "(unknown)");
        // All control characters → empty after filter → fallback.
        assert_eq!(sanitize_server_version("\x1b\x00\r\n"), "(unknown)");
    }

    #[test]
    fn set_server_version_sanitizes_input() {
        // set_server_version routes through sanitize_server_version, so a
        // hostile Hello.server value never reaches the renderer as-is.
        let mut app = App::default();
        app.set_server_version("xfr/0.9.9\x1b[31m".to_string());
        assert_eq!(app.server_version.as_deref(), Some("xfr/0.9.9[31m"));
    }

    #[test]
    fn on_progress_does_not_overwrite_elapsed() {
        // Regression: before this fix, on_progress wrote self.elapsed from
        // progress.elapsed_ms, which the next tick() immediately overwrote
        // from the wall clock — so the server's authoritative value never
        // actually rendered. We removed the write entirely; elapsed is
        // wall-clock authoritative during Running. This test pins that
        // contract so we don't accidentally re-introduce the write.
        let mut app = App::default();
        app.state = AppState::Running;
        app.start_time = Some(Instant::now() - Duration::from_secs(5));
        app.elapsed = Duration::from_secs(5); // wall-clock derived

        app.on_progress(crate::client::TestProgress {
            elapsed_ms: 9_999, // a server value far off from wall clock
            total_bytes: 0,
            throughput_mbps: 0.0,
            streams: vec![],
            rtt_us: None,
            cwnd: None,
            total_retransmits: None,
            bytes_sent: None,
            bytes_received: None,
            throughput_send_mbps: None,
            throughput_recv_mbps: None,
        });

        assert_eq!(
            app.elapsed,
            Duration::from_secs(5),
            "on_progress must not mutate elapsed; tick() owns it"
        );
    }

    #[test]
    fn tick_elapsed_excludes_paused_time() {
        // Timeline (simulated):
        //   T-5s: test started       (start_time = now - 5s)
        //   T-3s: user hit `p`       (pause_started_at backdated to 3s ago)
        //   T-0s: user hit `p` again (resume, now)
        // Wall-clock from start = 5s; of which 3s was paused, 2s actively
        // running. Resume must shift start_time forward by the pause duration
        // so tick() shows ~2s, not 5s.
        let mut app = App::default();
        app.state = AppState::Running;
        app.start_time = Some(Instant::now() - Duration::from_secs(5));

        app.toggle_pause();
        assert_eq!(app.state, AppState::Paused);
        // Backdate pause_started_at to 3s ago to simulate a 3s pause window.
        app.pause_started_at = Some(Instant::now() - Duration::from_secs(3));

        app.toggle_pause();
        assert_eq!(app.state, AppState::Running);
        app.tick();

        let elapsed = app.elapsed;
        assert!(
            elapsed >= Duration::from_secs(2) && elapsed < Duration::from_secs(3),
            "elapsed should be ~2s (wall-clock 5s minus 3s pause), got {:?}",
            elapsed
        );
    }
}
