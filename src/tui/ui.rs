//! TUI rendering

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::app::{App, AppState};
use super::widgets::{ProgressBar, Sparkline, StreamBar};
use crate::stats::{bytes_to_human, mbps_to_human};

pub fn draw(frame: &mut Frame, app: &App) {
    let size = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(10),   // Main content
            Constraint::Length(1), // Footer
        ])
        .split(size);

    draw_header(frame, app, chunks[0]);
    draw_main(frame, app, chunks[1]);
    draw_footer(frame, app, chunks[2]);

    if app.show_help {
        draw_help_overlay(frame, size);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
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
        .style(Style::default().fg(Color::White));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let info = format!(
        "Protocol: {}    Streams: {}    Direction: {}    Elapsed: {}s / {}s",
        app.protocol,
        app.streams_count,
        app.direction,
        app.elapsed.as_secs(),
        app.duration.as_secs()
    );

    let info_widget = Paragraph::new(info).style(Style::default().fg(Color::Gray));
    frame.render_widget(info_widget, inner);
}

fn draw_main(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match app.state {
        AppState::Connecting => {
            let msg = Paragraph::new("Connecting...")
                .style(Style::default().fg(Color::Yellow));
            frame.render_widget(msg, inner);
        }
        AppState::Error => {
            let msg = Paragraph::new(app.error.as_deref().unwrap_or("Unknown error"))
                .style(Style::default().fg(Color::Red));
            frame.render_widget(msg, inner);
        }
        AppState::Running | AppState::Paused | AppState::Completed => {
            draw_test_content(frame, app, inner);
        }
    }
}

fn draw_test_content(frame: &mut Frame, app: &App, area: Rect) {
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
        .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
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
            .style(Style::default().fg(Color::Green));
        frame.render_widget(sparkline, sparkline_area);

        // Current value
        let value_area = Rect {
            x: sparkline_area.x + sparkline_area.width + 1,
            y: sparkline_area.y,
            width: 18,
            height: 1,
        };
        let value = Paragraph::new(mbps_to_human(app.current_throughput_mbps))
            .style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD));
        frame.render_widget(value, value_area);
    }

    // Progress bar
    let progress = ProgressBar::new(app.progress_percent() / 100.0)
        .filled_style(Style::default().fg(Color::Cyan));
    frame.render_widget(progress, chunks[1]);

    // Streams label
    let streams_label = Paragraph::new("Streams")
        .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD));
    frame.render_widget(streams_label, chunks[3]);

    // Stream bars
    let max_throughput = app.streams.iter().map(|s| s.throughput_mbps).fold(0.0f64, f64::max).max(100.0);

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
        let bar = StreamBar::new(stream.id, stream.throughput_mbps, max_throughput, stream.retransmits);
        frame.render_widget(bar, stream_area);
    }

    // Stats
    let stats = format!(
        "Transfer: {}    Retransmits: {}    RTT: {:.2}ms    Cwnd: {}KB",
        bytes_to_human(app.total_bytes),
        app.total_retransmits,
        app.rtt_us as f64 / 1000.0,
        app.cwnd / 1024
    );
    let stats_widget = Paragraph::new(stats).style(Style::default().fg(Color::Gray));
    frame.render_widget(stats_widget, chunks[5]);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let keys = match app.state {
        AppState::Completed => "[q] Quit   [j] JSON   [?] Help",
        AppState::Error => "[q] Quit   [r] Retry   [?] Help",
        _ => "[q] Quit   [p] Pause   [r] Restart   [j] JSON   [?] Help",
    };

    let footer = Paragraph::new(keys)
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, area);
}

fn draw_help_overlay(frame: &mut Frame, area: Rect) {
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
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(" - Quit"),
        ]),
        Line::from(vec![
            Span::styled("p", Style::default().fg(Color::Cyan)),
            Span::raw(" - Pause/Resume"),
        ]),
        Line::from(vec![
            Span::styled("r", Style::default().fg(Color::Cyan)),
            Span::raw(" - Restart test"),
        ]),
        Line::from(vec![
            Span::styled("j", Style::default().fg(Color::Cyan)),
            Span::raw(" - Output JSON"),
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

    // Clear the area first
    frame.render_widget(ratatui::widgets::Clear, help_area);
    frame.render_widget(help, help_area);
}
