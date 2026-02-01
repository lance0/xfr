//! TUI application state machine

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::client::TestProgress;
use crate::protocol::{Direction, Protocol, TestResult};

const SPARKLINE_HISTORY: usize = 60;

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

    pub result: Option<TestResult>,
    pub error: Option<String>,

    pub start_time: Option<Instant>,
    pub show_help: bool,
}

impl App {
    pub fn new(
        host: String,
        port: u16,
        protocol: Protocol,
        direction: Direction,
        streams: u8,
        duration: Duration,
        bitrate: Option<u64>,
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

            result: None,
            error: None,

            start_time: None,
            show_help: false,
        }
    }

    pub fn on_connected(&mut self) {
        self.state = AppState::Running;
        self.start_time = Some(Instant::now());
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
        for interval in &progress.streams {
            if let Some(stream) = self.streams.get_mut(interval.id as usize) {
                stream.bytes = interval.bytes;
                stream.throughput_mbps = (interval.bytes as f64 * 8.0)
                    / (progress.elapsed_ms as f64 / 1000.0)
                    / 1_000_000.0;
                stream.retransmits = interval.retransmits.unwrap_or(0);
            }
        }

        // Sum retransmits
        self.total_retransmits = self.streams.iter().map(|s| s.retransmits).sum();
    }

    pub fn on_result(&mut self, result: TestResult) {
        self.state = AppState::Completed;
        if let Some(tcp_info) = &result.tcp_info {
            self.total_retransmits = tcp_info.retransmits;
            self.rtt_us = tcp_info.rtt_us;
            self.cwnd = tcp_info.cwnd;
        }
        self.result = Some(result);
    }

    pub fn on_error(&mut self, error: String) {
        self.state = AppState::Error;
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

    pub fn progress_percent(&self) -> f64 {
        if self.duration.as_secs() == 0 {
            0.0
        } else {
            (self.elapsed.as_secs_f64() / self.duration.as_secs_f64() * 100.0).min(100.0)
        }
    }

    pub fn time_remaining(&self) -> Duration {
        self.duration.saturating_sub(self.elapsed)
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
        )
    }
}
