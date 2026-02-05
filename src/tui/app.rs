//! TUI application state machine

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::client::TestProgress;
use crate::protocol::{Direction, Protocol, TestResult, TimestampFormat};

use super::settings::SettingsState;
use super::theme::Theme;

const SPARKLINE_HISTORY: usize = 60;
const LOG_HISTORY: usize = 100;

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
    pub streams: Vec<StreamData>,

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
            streams: (0..streams)
                .map(|id| StreamData {
                    id,
                    bytes: 0,
                    throughput_mbps: 0.0,
                    retransmits: 0,
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

    pub fn on_progress(&mut self, progress: TestProgress) {
        self.elapsed = Duration::from_millis(progress.elapsed_ms);
        self.total_bytes = progress.total_bytes;
        self.current_throughput_mbps = progress.throughput_mbps;

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

        // Sum retransmits
        self.total_retransmits = self.streams.iter().map(|s| s.retransmits).sum();

        // Update UDP stats (average jitter across streams)
        if jitter_count > 0 {
            self.udp_jitter_ms = total_jitter / jitter_count as f64;
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
        self.total_retransmits = result
            .streams
            .iter()
            .filter_map(|s| s.retransmits)
            .sum();

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
            AppState::Running => self.state = AppState::Paused,
            AppState::Paused => self.state = AppState::Running,
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
