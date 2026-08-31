//! Server TUI dashboard
//!
//! Real-time monitoring of active tests and server statistics.

use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Events sent from server to TUI
#[derive(Debug, Clone)]
pub enum ServerEvent {
    /// A new test has started
    TestStarted(ActiveTestInfo),
    /// Test progress update
    TestUpdated {
        id: String,
        bytes: u64,
        throughput_mbps: f64,
    },
    /// Test completed
    TestCompleted { id: String, bytes: u64 },
    /// Test failed before normal completion
    TestFailed { id: String, bytes: u64 },
    /// Connection blocked by ACL or rate limit
    ConnectionBlocked,
    /// Authentication failure
    AuthFailure,
}

use crate::stats::mbps_to_human;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};

const BANDWIDTH_HISTORY: usize = 60;

/// Active test info for display
#[derive(Debug, Clone)]
pub struct ActiveTestInfo {
    pub id: String,
    pub client_ip: IpAddr,
    pub protocol: String,
    pub direction: String,
    pub streams: u8,
    pub started: Instant,
    pub duration_secs: u32,
    pub bytes: u64,
    pub throughput_mbps: f64,
}

/// Server dashboard state
pub struct ServerApp {
    pub active_tests: Vec<ActiveTestInfo>,
    pub total_tests: u64,
    pub failed_tests: u64,
    pub total_bytes: u64,
    pub bandwidth_history: VecDeque<f64>,
    pub connections_blocked: u64,
    pub auth_failures: u64,
    pub start_time: Instant,
    pub show_help: bool,
}

impl Default for ServerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerApp {
    pub fn new() -> Self {
        Self {
            active_tests: Vec::new(),
            total_tests: 0,
            failed_tests: 0,
            total_bytes: 0,
            bandwidth_history: VecDeque::with_capacity(BANDWIDTH_HISTORY),
            connections_blocked: 0,
            auth_failures: 0,
            start_time: Instant::now(),
            show_help: false,
        }
    }

    pub fn add_test(&mut self, info: ActiveTestInfo) {
        self.active_tests.push(info);
        self.total_tests += 1;
    }

    pub fn update_test(&mut self, id: &str, bytes: u64, throughput_mbps: f64) {
        if let Some(test) = self.active_tests.iter_mut().find(|t| t.id == id) {
            test.bytes = bytes;
            test.throughput_mbps = throughput_mbps;
        }
    }

    pub fn remove_test(&mut self, id: &str, bytes: u64) {
        self.active_tests.retain(|t| t.id != id);
        self.total_bytes += bytes;
    }

    pub fn fail_test(&mut self, id: &str, bytes: u64) {
        self.active_tests.retain(|t| t.id != id);
        self.failed_tests += 1;
        self.total_bytes += bytes;
    }

    pub fn record_blocked(&mut self) {
        self.connections_blocked += 1;
    }

    pub fn record_auth_failure(&mut self) {
        self.auth_failures += 1;
    }

    pub fn update_bandwidth(&mut self) {
        let total_mbps: f64 = self.active_tests.iter().map(|t| t.throughput_mbps).sum();
        self.bandwidth_history.push_back(total_mbps);
        if self.bandwidth_history.len() > BANDWIDTH_HISTORY {
            self.bandwidth_history.pop_front();
        }
    }

    pub fn current_bandwidth(&self) -> f64 {
        self.active_tests.iter().map(|t| t.throughput_mbps).sum()
    }

    pub fn uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    /// Hash of every value rendered by [`draw`]. The server loop compares
    /// this with the last drawn frame so its 100 ms event poll can stay
    /// responsive without rebuilding an identical dashboard ten times per
    /// second. Wall-clock values are reduced to the whole seconds the UI
    /// displays, and the terminal size is included so resize reflow is never
    /// skipped.
    pub fn render_fingerprint(&self, size: Size) -> u64 {
        self.render_fingerprint_at(size, Instant::now())
    }

    fn render_fingerprint_at(&self, size: Size, now: Instant) -> u64 {
        // Keep these destructures exhaustive: a newly rendered field should
        // fail compilation here until its cache behavior is decided.
        let ServerApp {
            active_tests,
            total_tests,
            failed_tests,
            total_bytes: _,       // accumulated for bookkeeping, not rendered
            bandwidth_history: _, // reserved history is not currently rendered
            connections_blocked,
            auth_failures,
            start_time,
            show_help,
        } = self;

        let mut h = DefaultHasher::new();
        size.hash(&mut h);
        now.saturating_duration_since(*start_time)
            .as_secs()
            .hash(&mut h);
        total_tests.hash(&mut h);
        failed_tests.hash(&mut h);
        connections_blocked.hash(&mut h);
        auth_failures.hash(&mut h);
        show_help.hash(&mut h);

        active_tests.len().hash(&mut h);
        for ActiveTestInfo {
            id: _, // row identity used for updates, not rendered
            client_ip,
            protocol,
            direction,
            streams,
            started,
            duration_secs,
            bytes: _, // accumulated for bookkeeping, not rendered
            throughput_mbps,
        } in active_tests
        {
            client_ip.hash(&mut h);
            protocol.hash(&mut h);
            direction.hash(&mut h);
            streams.hash(&mut h);
            now.saturating_duration_since(*started)
                .as_secs()
                .hash(&mut h);
            duration_secs.hash(&mut h);
            throughput_mbps.to_bits().hash(&mut h);
        }
        h.finish()
    }
}

/// Draw the server dashboard
pub fn draw(frame: &mut Frame, app: &ServerApp) {
    let size = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // Header with stats
            Constraint::Min(10),   // Active tests table
            Constraint::Length(1), // Footer
        ])
        .split(size);

    draw_header(frame, app, chunks[0]);
    draw_tests_table(frame, app, chunks[1]);
    draw_footer(frame, chunks[2]);

    if app.show_help {
        draw_help_overlay(frame, size);
    }
}

fn draw_header(frame: &mut Frame, app: &ServerApp, area: Rect) {
    let uptime = app.uptime();
    let hours = uptime.as_secs() / 3600;
    let mins = (uptime.as_secs() % 3600) / 60;
    let secs = uptime.as_secs() % 60;

    let title = format!(" xfr server - uptime {:02}:{:02}:{:02} ", hours, mins, secs);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::White));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Stats layout
    let stats_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(inner);

    // Active tests
    let active = Paragraph::new(vec![
        Line::from(Span::styled(
            "Active Tests",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("{}", app.active_tests.len()),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
    ]);
    frame.render_widget(active, stats_chunks[0]);

    let bw_str = mbps_to_human(app.current_bandwidth());
    let bw_widget = Paragraph::new(vec![
        Line::from(Span::styled(
            "Bandwidth",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            bw_str,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
    ]);
    frame.render_widget(bw_widget, stats_chunks[1]);

    // Total / failed tests
    let total = Paragraph::new(vec![
        Line::from(Span::styled(
            "Tests / Failed",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("{} / {}", app.total_tests, app.failed_tests),
            Style::default().fg(if app.failed_tests > 0 {
                Color::Red
            } else {
                Color::Gray
            }),
        )),
    ]);
    frame.render_widget(total, stats_chunks[2]);

    // Blocked / Auth failures
    let security = Paragraph::new(vec![
        Line::from(Span::styled(
            "Blocked / Auth Fail",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("{} / {}", app.connections_blocked, app.auth_failures),
            Style::default().fg(if app.connections_blocked > 0 || app.auth_failures > 0 {
                Color::Yellow
            } else {
                Color::Gray
            }),
        )),
    ]);
    frame.render_widget(security, stats_chunks[3]);
}

fn draw_tests_table(frame: &mut Frame, app: &ServerApp, area: Rect) {
    let block = Block::default()
        .title(" Active Tests ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.active_tests.is_empty() {
        let msg = Paragraph::new("No active tests").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(msg, inner);
        return;
    }

    let header = Row::new(vec![
        "Client",
        "Protocol",
        "Direction",
        "Streams",
        "Elapsed",
        "Throughput",
    ])
    .style(Style::default().add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    let rows: Vec<Row> = app
        .active_tests
        .iter()
        .map(|test| {
            let elapsed = test.started.elapsed().as_secs();
            let throughput = mbps_to_human(test.throughput_mbps);

            Row::new(vec![
                test.client_ip.to_string(),
                test.protocol.clone(),
                test.direction.clone(),
                test.streams.to_string(),
                format!("{}s / {}s", elapsed, test.duration_secs),
                throughput,
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(16), // Client IP
            Constraint::Length(8),  // Protocol
            Constraint::Length(10), // Direction
            Constraint::Length(8),  // Streams
            Constraint::Length(12), // Elapsed
            Constraint::Length(12), // Throughput
        ],
    )
    .header(header)
    .style(Style::default().fg(Color::White));

    frame.render_widget(table, inner);
}

fn draw_footer(frame: &mut Frame, area: Rect) {
    let footer = Paragraph::new("[q] Quit   [?] Help").style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, area);
}

fn draw_help_overlay(frame: &mut Frame, area: Rect) {
    let help_width = 40;
    let help_height = 8;
    let help_area = Rect {
        x: (area.width.saturating_sub(help_width)) / 2,
        y: (area.height.saturating_sub(help_height)) / 2,
        width: help_width.min(area.width),
        height: help_height.min(area.height),
    };

    let help_text = vec![
        Line::from(vec![
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(" - Quit server"),
        ]),
        Line::from(vec![
            Span::styled("?", Style::default().fg(Color::Cyan)),
            Span::raw(" - Toggle help"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("Press "),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(" to close"),
        ]),
    ];

    let help = Paragraph::new(help_text)
        .block(
            Block::default()
                .title(" Help ")
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::White).bg(Color::Black)),
        )
        .style(Style::default().bg(Color::Black));

    frame.render_widget(ratatui::widgets::Clear, help_area);
    frame.render_widget(help, help_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn active_test(id: &str) -> ActiveTestInfo {
        ActiveTestInfo {
            id: id.to_string(),
            client_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            protocol: "TCP".to_string(),
            direction: "download".to_string(),
            streams: 1,
            started: Instant::now(),
            duration_secs: 10,
            bytes: 0,
            throughput_mbps: 0.0,
        }
    }

    #[test]
    fn failed_tests_are_counted_separately_from_completed_tests() {
        let mut app = ServerApp::new();
        app.add_test(active_test("ok"));
        app.add_test(active_test("fail"));

        app.remove_test("ok", 100);
        app.fail_test("fail", 50);

        assert!(app.active_tests.is_empty());
        assert_eq!(app.total_tests, 2);
        assert_eq!(app.failed_tests, 1);
        assert_eq!(app.total_bytes, 150);
    }

    #[test]
    fn render_fingerprint_tracks_visible_server_state() {
        let now = Instant::now();
        let size = Size::new(120, 40);
        let mut app = ServerApp::new();
        app.start_time = now - Duration::from_secs(10);

        let idle = app.render_fingerprint_at(size, now);
        assert_eq!(
            idle,
            app.render_fingerprint_at(size, now + Duration::from_millis(999)),
            "sub-second uptime drift is not visible"
        );
        assert_ne!(
            idle,
            app.render_fingerprint_at(size, now + Duration::from_secs(1)),
            "whole-second uptime changes the header"
        );
        assert_ne!(
            idle,
            app.render_fingerprint_at(Size::new(100, 30), now),
            "terminal resize reflows the dashboard"
        );

        let mut test = active_test("visible");
        test.started = now - Duration::from_secs(2);
        app.add_test(test);
        let active = app.render_fingerprint_at(size, now);
        assert_ne!(idle, active, "a new row must dirty the frame");

        app.update_test("visible", 1024, 42.0);
        let updated = app.render_fingerprint_at(size, now);
        assert_ne!(active, updated, "visible throughput changed");
        assert_ne!(
            updated,
            app.render_fingerprint_at(size, now + Duration::from_secs(1)),
            "active-test elapsed time changed"
        );

        app.toggle_help();
        let help = app.render_fingerprint_at(size, now);
        assert_ne!(updated, help, "help overlay changed");
        app.record_blocked();
        assert_ne!(help, app.render_fingerprint_at(size, now));
    }
}
