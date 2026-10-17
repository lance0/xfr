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

use super::theme::Theme;
use crate::stats::mbps_to_human;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

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
    pub help_scroll: u16,
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
            help_scroll: 0,
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
        self.help_scroll = 0;
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
            help_scroll,
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
        help_scroll.hash(&mut h);

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
pub fn draw(frame: &mut Frame, app: &ServerApp, theme: &Theme) {
    let size = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            // Header with stats, collapsed to its uptime title below 10 rows.
            Constraint::Length(if size.height < 10 { 1 } else { 5 }),
            // Active tests table. It shrinks first so the header and the
            // footer's help hint survive on short terminals.
            Constraint::Min(0),
            Constraint::Length(1), // Footer
        ])
        .split(size);

    draw_header(frame, app, theme, chunks[0]);
    draw_tests_table(frame, app, theme, chunks[1]);
    // Same hints, order and style as the client footer at this width.
    let compact = !super::ui::roomy_hints_fit(chunks[2].width);
    frame.render_widget(
        Paragraph::new(super::ui::key_hints(
            &[("?", "Help"), ("q", "Quit")],
            chunks[2].width,
            compact,
            theme,
        )),
        chunks[2],
    );

    if app.show_help {
        super::ui::draw_help_overlay(frame, theme, size, app.help_scroll, true);
    }
}

fn draw_header(frame: &mut Frame, app: &ServerApp, theme: &Theme, area: Rect) {
    let uptime = app.uptime();
    let hours = uptime.as_secs() / 3600;
    let mins = (uptime.as_secs() % 3600) / 60;
    let secs = uptime.as_secs() % 60;

    let title = format!(" xfr server - uptime {:02}:{:02}:{:02} ", hours, mins, secs);

    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.header)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().fg(theme.text));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let quiet_or =
        |alert: bool, color| Style::default().fg(if alert { color } else { theme.text_dim });
    let stats = [
        (
            "Active Tests",
            app.active_tests.len().to_string(),
            Style::default()
                .fg(theme.success)
                .add_modifier(Modifier::BOLD),
        ),
        (
            "Bandwidth",
            mbps_to_human(app.current_bandwidth()),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        (
            "Tests / Failed",
            format!("{} / {}", app.total_tests, app.failed_tests),
            quiet_or(app.failed_tests > 0, theme.error),
        ),
        (
            "Blocked / Auth Fail",
            format!("{} / {}", app.connections_blocked, app.auth_failures),
            quiet_or(
                app.connections_blocked > 0 || app.auth_failures > 0,
                theme.warning,
            ),
        ),
    ];

    // Equal columns that each hold their label and value plus a 1-column gap
    // (the border serves as the last one's). When they don't fit, the
    // lower-value columns go first; Bandwidth stays to the end.
    let fits = |columns: &[usize]| {
        let width = usize::from(inner.width) / columns.len();
        columns.iter().enumerate().all(|(n, &i)| {
            let (label, value, _) = &stats[i];
            let gap = usize::from(n + 1 < columns.len());
            label.len().max(value.len()) + gap <= width
        })
    };
    let columns: &[usize] = [&[0, 1, 2, 3][..], &[0, 1, 2], &[0, 1]]
        .into_iter()
        .find(|columns| fits(columns))
        .unwrap_or(&[1]);
    let stats_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![
            Constraint::Ratio(1, columns.len() as u32);
            columns.len()
        ])
        .split(inner);

    for (&i, &chunk) in columns.iter().zip(stats_chunks.iter()) {
        let (label, value, value_style) = &stats[i];
        let column = Paragraph::new(vec![
            Line::from(Span::styled(
                *label,
                Style::default()
                    .fg(theme.text_dim)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(value.as_str(), *value_style)),
        ]);
        frame.render_widget(column, chunk);
    }
}

fn draw_tests_table(frame: &mut Frame, app: &ServerApp, theme: &Theme, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    // The column header takes a row, plus a spacer row when there is room.
    let header_rows = 1 + u16::from(inner.height >= 4);
    let visible = usize::from(inner.height.saturating_sub(header_rows));
    let hidden = app.active_tests.len().saturating_sub(visible);
    let title = if hidden > 0 {
        format!(
            " Active Tests ({}, {hidden} hidden) ",
            app.active_tests.len()
        )
    } else {
        " Active Tests ".to_string()
    };
    let block = block.title(Span::styled(
        title,
        Style::default()
            .fg(theme.header)
            .add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(block, area);

    if app.active_tests.is_empty() {
        let msg = Paragraph::new("No active tests").style(Style::default().fg(theme.text_dim));
        frame.render_widget(msg, inner);
        return;
    }

    let columns: &[usize] = if inner.width >= 71 {
        &[0, 1, 2, 3, 4, 5]
    } else if inner.width >= 49 {
        &[0, 1, 2, 5]
    } else {
        &[0, 5]
    };
    let labels = [
        "Client",
        "Protocol",
        "Direction",
        "Streams",
        "Elapsed",
        "Throughput",
    ];
    let header = Row::new(columns.iter().map(|&i| labels[i]))
        .style(
            Style::default()
                .fg(theme.header)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(header_rows - 1);

    let rows = app.active_tests.iter().map(|test| {
        Row::new(columns.iter().map(|&i| match i {
            0 => Cell::from(test.client_ip.to_string()),
            1 => Cell::from(test.protocol.as_str()),
            2 => Cell::from(test.direction.as_str()),
            3 => Cell::from(test.streams.to_string()),
            4 => Cell::from(format!(
                "{}s / {}s",
                test.started.elapsed().as_secs(),
                test.duration_secs
            )),
            _ => Cell::from(mbps_to_human(test.throughput_mbps)),
        }))
    });

    let table = Table::new(
        rows,
        columns
            .iter()
            .map(|&i| match i {
                0 => Constraint::Min(8),
                5 => Constraint::Length(12),
                1 | 3 => Constraint::Length(8),
                2 => Constraint::Length(10),
                _ => Constraint::Length(12),
            })
            .collect::<Vec<_>>(),
    )
    .header(header)
    .style(Style::default().fg(theme.text));

    frame.render_widget(table, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use ratatui::{Terminal, backend::TestBackend};
    use std::collections::HashSet;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn narrow_server_keeps_client_and_throughput() {
        let mut app = ServerApp::new();
        app.add_test(ActiveTestInfo {
            id: "test".into(),
            client_ip: "192.0.2.1".parse().unwrap(),
            protocol: "TCP".into(),
            direction: "Download".into(),
            streams: 128,
            started: Instant::now(),
            duration_secs: 10,
            bytes: 0,
            throughput_mbps: 42.0,
        });
        for (width, height) in [
            (73, 16),
            (51, 16),
            (80, 16),
            (80, 10),
            (80, 8),
            (40, 16),
            (40, 12),
            (40, 10),
            (40, 8),
            (40, 3),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| draw(f, &app, &Theme::default())).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            // The header and the footer's help hint must never be squeezed out.
            assert!(text.contains("uptime"), "{width}x{height}\n{text}");
            assert!(text.contains("?:Help q:Quit"), "{width}x{height}\n{text}");
            assert_eq!(text.contains("Bandwidth"), height >= 10, "{text}");
            if height >= 8 {
                assert!(text.contains("192.0.2.1"), "{text}");
                assert!(text.contains("42.0 Mbps"), "{text}");
            }
            assert_eq!(text.contains("Protocol"), width >= 51, "{text}");
            if width >= 51 {
                assert!(text.contains("TCP") && text.contains("Download"), "{text}");
            }
            assert_eq!(text.contains("Streams"), width >= 73, "{text}");
            assert_eq!(text.contains("Elapsed"), width >= 73, "{text}");
            if width >= 73 {
                assert!(text.contains("128"), "{text}");
            }
        }
        app.active_tests.clear();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal.draw(|f| draw(f, &app, &Theme::default())).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("No active tests"));
        let mut terminal = Terminal::new(TestBackend::new(1, 1)).unwrap();
        terminal.draw(|f| draw(f, &app, &Theme::default())).unwrap();
        app.show_help = true;
        terminal.draw(|f| draw(f, &app, &Theme::default())).unwrap();
    }

    fn screen(app: &ServerApp, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app, &Theme::default())).unwrap();
        let buf = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    fn busy_server() -> ServerApp {
        let mut app = ServerApp::new();
        for (id, mbps) in [("a", 60_960.0), ("b", 299.9), ("c", 1990.0)] {
            app.add_test(active_test(id));
            app.update_test(id, 0, mbps);
        }
        app
    }

    /// At 40 columns the four equal header columns ran together:
    /// "Active TesBandwidthTests / FaBlocked /" over "3  61.93 Gbp3 / 0".
    #[test]
    fn narrow_header_drops_columns_instead_of_overlapping() {
        let app = busy_server();
        for width in [30, 40, 50, 60, 79, 80, 120] {
            let rows = screen(&app, width, 10);
            let (labels, values) = (&rows[1], &rows[2]);
            assert!(values.contains("63.25 Gbps"), "{width}: {values:?}");
            for label in ["Active Tests", "Bandwidth"] {
                assert!(labels.contains(label), "{width}: {labels:?}");
            }
            // Every label shown is whole and set off by a space or border.
            let chars: Vec<char> = labels.chars().collect();
            for label in [
                "Active Tests",
                "Bandwidth",
                "Tests / Failed",
                "Blocked / Auth Fail",
            ] {
                let Some(at) = labels.find(&label[..7]) else {
                    continue;
                };
                let at = labels[..at].chars().count();
                let end = at + label.len();
                let whole: String = chars[at..end].iter().collect();
                assert_eq!(whole, label, "{width}: {labels:?}");
                for edge in [chars[at - 1], chars[end]] {
                    assert!(edge == ' ' || edge == '│', "{width}: {labels:?}");
                }
            }
            for joined in ["TesBandwidth", "BandwidthTests", "Gbp3", "Gbps3"] {
                assert!(!labels.contains(joined) && !values.contains(joined));
            }
            assert_eq!(
                values.contains("3 / 0"),
                labels.contains("Tests / Failed"),
                "{width}: {rows:?}"
            );
        }
        let rows = screen(&app, 40, 10);
        assert!(!rows[1].contains("Tests /"), "{:?}", rows[1]);
        let rows = screen(&app, 80, 10);
        assert!(rows[1].contains("Blocked / Auth Fail"), "{:?}", rows[1]);
    }

    /// Help leads, as in the client, in the client's style for that width.
    #[test]
    fn footer_matches_the_client_hint_order_and_style() {
        let app = ServerApp::new();
        for (width, footer) in [
            (40, "?:Help q:Quit"),
            (80, "?:Help q:Quit"),
            (120, "[?] Help | [q] Quit"),
        ] {
            let rows = screen(&app, width, 12);
            assert!(rows[11].starts_with(footer), "{width}: {:?}", rows[11]);
        }
    }

    /// A short terminal showed one of three active tests with no sign of
    /// the other two.
    #[test]
    fn table_title_counts_hidden_tests() {
        let app = busy_server();
        let rows = screen(&app, 80, 10);
        assert!(rows[5].contains(" Active Tests (3, 2 hidden) "), "{rows:?}");
        let rows = screen(&app, 80, 24);
        assert!(rows[5].contains(" Active Tests "), "{rows:?}");
        assert!(!rows.concat().contains("hidden"), "{rows:?}");
    }

    /// Every foreground colour the dashboard actually paints.
    fn rendered_colors(app: &ServerApp, theme: &Theme) -> HashSet<Color> {
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
        terminal.draw(|f| draw(f, app, theme)).unwrap();
        let buf = terminal.backend().buffer();
        (0..20)
            .flat_map(|y| (0..90).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].style().fg.unwrap_or(Color::Reset))
            .collect()
    }

    /// Regression: the dashboard hardcoded `Color::White/Cyan/Green/Red/Gray`,
    /// so it ignored `--theme` entirely and rendered white-on-nothing — which
    /// is unreadable on a light terminal background.
    #[test]
    fn dashboard_paints_only_theme_colors() {
        let mut app = ServerApp::new();
        app.add_test(active_test("t1"));
        app.record_blocked();
        app.fail_test("t1", 1);

        let mono = Theme::by_name("monochrome");
        let painted = rendered_colors(&app, &mono);

        // A hue-free theme must not leak the old hardcoded accents.
        for banned in [Color::Cyan, Color::Green, Color::Red, Color::Yellow] {
            assert!(
                !painted.contains(&banned),
                "monochrome dashboard painted {banned:?}: {painted:?}"
            );
        }

        // ...and switching themes must actually change what lands on screen,
        // which a hardcoded palette could never do.
        let kawaii = rendered_colors(&app, &Theme::by_name("kawaii"));
        assert_ne!(
            painted, kawaii,
            "theme change did not alter the rendered colours"
        );
    }

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
