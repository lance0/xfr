//! TUI rendering

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::app::{App, AppState};
use super::theme::Theme;
use super::widgets::{ProgressBar, Sparkline, StreamBar};
use crate::stats::{bytes_to_human, mbps_to_human};

pub fn draw(frame: &mut Frame, app: &App) {
    let size = frame.area();
    let theme = &app.theme;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(10),   // Main content
            Constraint::Length(1), // Footer
        ])
        .split(size);

    draw_header(frame, app, theme, chunks[0]);
    draw_main(frame, app, theme, chunks[1]);
    draw_footer(frame, app, theme, chunks[2]);

    if app.show_help {
        draw_help_overlay(frame, theme, size);
    }
}

fn draw_header(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let title = format!(
        " xfr {} {}:{}",
        match app.state {
            AppState::Connecting => "connecting to",
            AppState::Running => "→",
            AppState::Paused => "(paused) →",
            AppState::Completed => "✓",
            AppState::Error => "✗",
        },
        app.host,
        app.port
    );

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .style(Style::default().fg(theme.border));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let bitrate_str = app.bitrate.map(|b| {
        if b >= 1_000_000_000 {
            format!("{}G", b / 1_000_000_000)
        } else if b >= 1_000_000 {
            format!("{}M", b / 1_000_000)
        } else {
            format!("{}K", b / 1_000)
        }
    });

    let info = if let Some(br) = bitrate_str {
        format!(
            "Protocol: {} @ {}bps    Streams: {}    Direction: {}    Elapsed: {}s / {}s",
            app.protocol,
            br,
            app.streams_count,
            app.direction,
            app.elapsed.as_secs(),
            app.duration.as_secs()
        )
    } else {
        format!(
            "Protocol: {}    Streams: {}    Direction: {}    Elapsed: {}s / {}s",
            app.protocol,
            app.streams_count,
            app.direction,
            app.elapsed.as_secs(),
            app.duration.as_secs()
        )
    };

    let info_widget = Paragraph::new(info).style(Style::default().fg(theme.text_dim));
    frame.render_widget(info_widget, inner);
}

fn draw_main(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match app.state {
        AppState::Connecting => {
            let msg = Paragraph::new("Connecting...").style(Style::default().fg(theme.warning));
            frame.render_widget(msg, inner);
        }
        AppState::Error => {
            let msg = Paragraph::new(app.error.as_deref().unwrap_or("Unknown error"))
                .style(Style::default().fg(theme.error));
            frame.render_widget(msg, inner);
        }
        AppState::Running | AppState::Paused | AppState::Completed => {
            draw_test_content(frame, app, theme, inner);
        }
    }
}

fn draw_test_content(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Throughput sparkline
            Constraint::Length(1), // Current throughput
            Constraint::Length(1), // Separator
            Constraint::Min(4),    // Streams
            Constraint::Length(1), // Separator
            Constraint::Length(1), // Stats
        ])
        .margin(1)
        .split(area);

    // Throughput label
    let label = Paragraph::new("Throughput")
        .style(Style::default().fg(theme.text).add_modifier(Modifier::BOLD));
    frame.render_widget(label, chunks[0]);

    // Sparkline
    if !app.throughput_history.is_empty() {
        let sparkline_area = Rect {
            x: chunks[0].x,
            y: chunks[0].y + 1,
            width: chunks[0].width.saturating_sub(20),
            height: 1,
        };
        let data: Vec<f64> = app.throughput_history.iter().cloned().collect();
        let sparkline = Sparkline::new(&data)
            .max(app.max_throughput().max(100.0))
            .style(Style::default().fg(theme.graph_primary));
        frame.render_widget(sparkline, sparkline_area);

        // Current value
        let value_area = Rect {
            x: sparkline_area.x + sparkline_area.width + 1,
            y: sparkline_area.y,
            width: 18,
            height: 1,
        };
        let value = Paragraph::new(mbps_to_human(app.current_throughput_mbps)).style(
            Style::default()
                .fg(theme.graph_primary)
                .add_modifier(Modifier::BOLD),
        );
        frame.render_widget(value, value_area);
    }

    // Progress bar
    let progress = ProgressBar::new(app.progress_percent() / 100.0)
        .filled_style(Style::default().fg(theme.graph_secondary));
    frame.render_widget(progress, chunks[1]);

    // Streams label
    let streams_label = Paragraph::new("Streams")
        .style(Style::default().fg(theme.text).add_modifier(Modifier::BOLD));
    frame.render_widget(streams_label, chunks[3]);

    // Stream bars
    let max_throughput = app
        .streams
        .iter()
        .map(|s| s.throughput_mbps)
        .fold(0.0f64, f64::max)
        .max(100.0);

    for (i, stream) in app.streams.iter().enumerate() {
        if i as u16 + 1 >= chunks[3].height {
            break;
        }
        let stream_area = Rect {
            x: chunks[3].x,
            y: chunks[3].y + 1 + i as u16,
            width: chunks[3].width,
            height: 1,
        };
        let bar = StreamBar::new(
            stream.id,
            stream.throughput_mbps,
            max_throughput,
            stream.retransmits,
        )
        .bar_color(theme.graph_primary)
        .text_color(theme.text);
        frame.render_widget(bar, stream_area);
    }

    // Stats - show different info for TCP vs UDP
    let stats = if app.protocol == crate::protocol::Protocol::Udp {
        format!(
            "Transfer: {}    Jitter: {:.2}ms    Loss: {:.1}% ({}/{} pkts)",
            bytes_to_human(app.total_bytes),
            app.udp_jitter_ms,
            app.udp_lost_percent,
            app.udp_packets_lost,
            app.udp_packets_sent,
        )
    } else {
        format!(
            "Transfer: {}    Retransmits: {}    RTT: {:.2}ms    Cwnd: {}KB",
            bytes_to_human(app.total_bytes),
            app.total_retransmits,
            app.rtt_us as f64 / 1000.0,
            app.cwnd / 1024
        )
    };
    let stats_widget = Paragraph::new(stats).style(Style::default().fg(theme.text_dim));
    frame.render_widget(stats_widget, chunks[5]);
}

fn draw_footer(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let keys = match app.state {
        AppState::Completed => "[q] Quit   [t] Theme   [j] JSON   [?] Help",
        AppState::Error => "[q] Quit   [r] Retry   [?] Help",
        _ => "[q] Quit   [p] Pause   [t] Theme   [j] JSON   [?] Help",
    };

    let footer = Paragraph::new(keys).style(Style::default().fg(theme.text_dim));
    frame.render_widget(footer, area);
}

fn draw_help_overlay(frame: &mut Frame, theme: &Theme, area: Rect) {
    let help_width = 50;
    let help_height = 12;
    let help_area = Rect {
        x: (area.width.saturating_sub(help_width)) / 2,
        y: (area.height.saturating_sub(help_height)) / 2,
        width: help_width.min(area.width),
        height: help_height.min(area.height),
    };

    let help_text = vec![
        Line::from(vec![
            Span::styled("q", Style::default().fg(theme.accent)),
            Span::raw(" - Quit"),
        ]),
        Line::from(vec![
            Span::styled("p", Style::default().fg(theme.accent)),
            Span::raw(" - Pause/Resume"),
        ]),
        Line::from(vec![
            Span::styled("t", Style::default().fg(theme.accent)),
            Span::raw(" - Cycle theme"),
        ]),
        Line::from(vec![
            Span::styled("j", Style::default().fg(theme.accent)),
            Span::raw(" - Output JSON"),
        ]),
        Line::from(vec![
            Span::styled("?", Style::default().fg(theme.accent)),
            Span::raw(" - Toggle help"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("Press "),
            Span::styled("Esc", Style::default().fg(theme.accent)),
            Span::raw(" to close"),
        ]),
    ];

    let help = Paragraph::new(help_text)
        .block(
            Block::default()
                .title(" Help ")
                .borders(Borders::ALL)
                .style(Style::default().fg(theme.text).bg(Color::Black)),
        )
        .style(Style::default().bg(Color::Black));

    // Clear the area first
    frame.render_widget(ratatui::widgets::Clear, help_area);
    frame.render_widget(help, help_area);
}
