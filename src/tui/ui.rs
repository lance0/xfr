//! TUI rendering

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::app::{App, AppState};
use super::theme::Theme;
use super::widgets::{ProgressBar, Sparkline, StreamBar};
use crate::stats::{bytes_to_human, mbps_to_human};

/// Get color for retransmit count: green (0), yellow (1-100), red (>100)
fn retransmit_color(retransmits: u64, theme: &Theme) -> Color {
    if retransmits == 0 {
        theme.success
    } else if retransmits <= 100 {
        theme.warning
    } else {
        theme.error
    }
}

/// Get color for packet loss percentage: green (<0.1%), yellow (0.1-1%), red (>1%)
fn loss_color(loss_percent: f64, theme: &Theme) -> Color {
    if loss_percent < 0.1 {
        theme.success
    } else if loss_percent <= 1.0 {
        theme.warning
    } else {
        theme.error
    }
}

/// Get color for jitter: green (<1ms), yellow (1-10ms), red (>10ms)
fn jitter_color(jitter_ms: f64, theme: &Theme) -> Color {
    if jitter_ms < 1.0 {
        theme.success
    } else if jitter_ms <= 10.0 {
        theme.warning
    } else {
        theme.error
    }
}

/// Get color for RTT: green (<10ms), yellow (10-100ms), red (>100ms)
fn rtt_color(rtt_ms: f64, theme: &Theme) -> Color {
    if rtt_ms < 10.0 {
        theme.success
    } else if rtt_ms <= 100.0 {
        theme.warning
    } else {
        theme.error
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let size = frame.area();
    let theme = &app.theme;

    // Simple layout: main content + status bar (like ttl)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),    // Main content
            Constraint::Length(1), // Status bar
        ])
        .split(size);

    draw_main(frame, app, theme, chunks[0]);
    draw_status_bar(frame, app, theme, chunks[1]);

    if app.show_help {
        draw_help_overlay(frame, theme, size);
    }
}

/// Build the title with styling
fn build_title(app: &App, theme: &Theme) -> Line<'static> {
    let status_color = match app.state {
        AppState::Connecting => theme.text_dim,
        AppState::Running => theme.accent,
        AppState::Paused => theme.warning,
        AppState::Completed => theme.success,
        AppState::Error => theme.error,
    };
    let status_text = match app.state {
        AppState::Connecting => "connecting",
        AppState::Running => "●",
        AppState::Paused => "⏸",
        AppState::Completed => "✓",
        AppState::Error => "✗",
    };

    Line::from(vec![
        Span::styled(
            " xfr ",
            Style::default()
                .fg(theme.header)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("── ", Style::default().fg(theme.border)),
        Span::styled(
            format!("{}:{} ", app.host, app.port),
            Style::default().fg(theme.text),
        ),
        Span::styled("── ", Style::default().fg(theme.border)),
        Span::styled(
            format!("{}", app.protocol),
            Style::default().fg(theme.accent),
        ),
        Span::styled(
            format!("×{} ", app.streams_count),
            Style::default().fg(theme.text_dim),
        ),
        Span::styled(
            format!("{} ", app.direction),
            Style::default().fg(theme.text_dim),
        ),
        Span::styled("── ", Style::default().fg(theme.border)),
        Span::styled(
            format!("{}s/{}s ", app.elapsed.as_secs(), app.duration.as_secs()),
            Style::default().fg(theme.text),
        ),
        Span::styled(status_text, Style::default().fg(status_color)),
    ])
}

fn draw_main(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let title = build_title(app, theme);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    match app.state {
        AppState::Connecting => {
            let msg = Paragraph::new("Connecting...").style(Style::default().fg(theme.text_dim));
            frame.render_widget(msg, inner);
        }
        AppState::Error => {
            let msg = Paragraph::new(app.error.as_deref().unwrap_or("Unknown error"))
                .style(Style::default().fg(theme.error));
            frame.render_widget(msg, inner);
        }
        AppState::Running | AppState::Paused => {
            draw_running_content(frame, app, theme, inner);
        }
        AppState::Completed => {
            // Draw the running content as background, then overlay the completion modal
            draw_running_content(frame, app, theme, inner);
            draw_completion_modal(frame, app, theme, area);
        }
    }
}

fn draw_running_content(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    // Layout: throughput display + sparkline + progress + streams + stats
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Big throughput number + sparkline
            Constraint::Length(1), // Progress bar
            Constraint::Length(1), // Spacer
            Constraint::Min(2),    // Streams
            Constraint::Length(1), // Stats line
        ])
        .split(area);

    // Big throughput display with sparkline
    draw_throughput_section(frame, app, theme, chunks[0]);

    // Progress bar
    let progress = ProgressBar::new(app.progress_percent() / 100.0)
        .filled_style(Style::default().fg(theme.graph_secondary));
    frame.render_widget(progress, chunks[1]);

    // Streams
    draw_streams(frame, app, theme, chunks[3]);

    // Stats line
    draw_stats_line(frame, app, theme, chunks[4]);
}

fn draw_throughput_section(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    // Split into: big number on left, sparkline on right
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(20), // Throughput value
            Constraint::Min(10),    // Sparkline
        ])
        .split(area);

    // Big throughput number
    let throughput_str = mbps_to_human(app.current_throughput_mbps);
    let throughput = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            throughput_str,
            Style::default()
                .fg(theme.graph_primary)
                .add_modifier(Modifier::BOLD),
        )),
    ]);
    frame.render_widget(throughput, chunks[0]);

    // Sparkline
    if !app.throughput_history.is_empty() {
        let sparkline_area = Rect {
            x: chunks[1].x,
            y: chunks[1].y + 1,
            width: chunks[1].width,
            height: 2,
        };
        let data: Vec<f64> = app.throughput_history.iter().cloned().collect();
        let sparkline = Sparkline::new(&data)
            .max(app.max_throughput().max(100.0))
            .style(Style::default().fg(theme.graph_primary));
        frame.render_widget(sparkline, sparkline_area);
    }
}

fn draw_streams(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let max_throughput = app
        .streams
        .iter()
        .map(|s| s.throughput_mbps)
        .fold(0.0f64, f64::max)
        .max(100.0);

    for (i, stream) in app.streams.iter().enumerate() {
        if i as u16 >= area.height {
            break;
        }
        let stream_area = Rect {
            x: area.x,
            y: area.y + i as u16,
            width: area.width,
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
}

fn draw_stats_line(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let stats_line = if app.protocol == crate::protocol::Protocol::Udp {
        let jitter_col = jitter_color(app.udp_jitter_ms, theme);
        let loss_col = loss_color(app.udp_lost_percent, theme);
        Line::from(vec![
            Span::styled("Transfer ", Style::default().fg(theme.text_dim)),
            Span::styled(
                bytes_to_human(app.total_bytes),
                Style::default().fg(theme.text),
            ),
            Span::styled("  Jitter ", Style::default().fg(theme.text_dim)),
            Span::styled(
                format!("{:.2}ms", app.udp_jitter_ms),
                Style::default().fg(jitter_col),
            ),
            Span::styled("  Loss ", Style::default().fg(theme.text_dim)),
            Span::styled(
                format!("{:.1}%", app.udp_lost_percent),
                Style::default().fg(loss_col),
            ),
        ])
    } else {
        let rtt_ms = app.rtt_us as f64 / 1000.0;
        let retransmit_col = retransmit_color(app.total_retransmits, theme);
        let rtt_col = rtt_color(rtt_ms, theme);
        Line::from(vec![
            Span::styled("Transfer ", Style::default().fg(theme.text_dim)),
            Span::styled(
                bytes_to_human(app.total_bytes),
                Style::default().fg(theme.text),
            ),
            Span::styled("  Retrans ", Style::default().fg(theme.text_dim)),
            Span::styled(
                format!("{}", app.total_retransmits),
                Style::default().fg(retransmit_col),
            ),
            Span::styled("  RTT ", Style::default().fg(theme.text_dim)),
            Span::styled(format!("{:.2}ms", rtt_ms), Style::default().fg(rtt_col)),
            Span::styled("  Cwnd ", Style::default().fg(theme.text_dim)),
            Span::styled(
                format!("{}KB", app.cwnd / 1024),
                Style::default().fg(theme.text),
            ),
        ])
    };
    frame.render_widget(Paragraph::new(stats_line), area);
}

fn draw_completion_modal(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let modal_width = 44;
    let modal_height = 14;
    let modal_area = Rect {
        x: (area.width.saturating_sub(modal_width)) / 2,
        y: (area.height.saturating_sub(modal_height)) / 2,
        width: modal_width.min(area.width),
        height: modal_height.min(area.height),
    };

    // Calculate average throughput
    let avg_throughput = if !app.throughput_history.is_empty() {
        app.throughput_history.iter().sum::<f64>() / app.throughput_history.len() as f64
    } else {
        app.current_throughput_mbps
    };

    // Build content lines
    let mut lines = vec![
        Line::from(""),
        // Big throughput number centered
        Line::from(vec![Span::styled(
            format!("       {}       ", mbps_to_human(avg_throughput)),
            Style::default()
                .fg(theme.success)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
    ];

    if app.protocol == crate::protocol::Protocol::Udp {
        let jitter_col = jitter_color(app.udp_jitter_ms, theme);
        let loss_col = loss_color(app.udp_lost_percent, theme);
        lines.extend(vec![
            Line::from(vec![
                Span::styled("  Transfer     ", Style::default().fg(theme.text_dim)),
                Span::styled(
                    bytes_to_human(app.total_bytes),
                    Style::default().fg(theme.text),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Duration     ", Style::default().fg(theme.text_dim)),
                Span::styled(
                    format!("{:.2}s", app.duration.as_secs_f64()),
                    Style::default().fg(theme.text),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Jitter       ", Style::default().fg(theme.text_dim)),
                Span::styled(
                    format!("{:.2}ms", app.udp_jitter_ms),
                    Style::default().fg(jitter_col),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Loss         ", Style::default().fg(theme.text_dim)),
                Span::styled(
                    format!("{:.2}%", app.udp_lost_percent),
                    Style::default().fg(loss_col),
                ),
            ]),
        ]);
    } else {
        let rtt_ms = app.rtt_us as f64 / 1000.0;
        let retransmit_col = retransmit_color(app.total_retransmits, theme);
        let rtt_col = rtt_color(rtt_ms, theme);
        lines.extend(vec![
            Line::from(vec![
                Span::styled("  Transfer     ", Style::default().fg(theme.text_dim)),
                Span::styled(
                    bytes_to_human(app.total_bytes),
                    Style::default().fg(theme.text),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Duration     ", Style::default().fg(theme.text_dim)),
                Span::styled(
                    format!("{:.2}s", app.duration.as_secs_f64()),
                    Style::default().fg(theme.text),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Retransmits  ", Style::default().fg(theme.text_dim)),
                Span::styled(
                    format!("{}", app.total_retransmits),
                    Style::default().fg(retransmit_col),
                ),
            ]),
            Line::from(vec![
                Span::styled("  RTT          ", Style::default().fg(theme.text_dim)),
                Span::styled(format!("{:.2}ms", rtt_ms), Style::default().fg(rtt_col)),
            ]),
        ]);
    }

    lines.extend(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  q", Style::default().fg(theme.accent)),
            Span::styled(" quit  ", Style::default().fg(theme.text_dim)),
            Span::styled("j", Style::default().fg(theme.accent)),
            Span::styled(" json  ", Style::default().fg(theme.text_dim)),
            Span::styled("t", Style::default().fg(theme.accent)),
            Span::styled(" theme", Style::default().fg(theme.text_dim)),
        ]),
    ]);

    let modal = Paragraph::new(lines).block(
        Block::default()
            .title(" Test Complete ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.success)),
    );

    frame.render_widget(Clear, modal_area);
    frame.render_widget(modal, modal_area);
}

fn draw_status_bar(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    // ttl-style status bar: "q quit | p pause | t theme | ? help"
    let status_text = match app.state {
        AppState::Completed => "q quit | t theme | j json | ? help",
        AppState::Error => "q quit | ? help",
        _ => "q quit | p pause | t theme | j json | ? help",
    };

    let status = Paragraph::new(status_text).style(Style::default().fg(theme.text_dim));
    frame.render_widget(status, area);
}

fn draw_help_overlay(frame: &mut Frame, theme: &Theme, area: Rect) {
    let help_width = 36;
    let help_height = 12;
    let help_area = Rect {
        x: (area.width.saturating_sub(help_width)) / 2,
        y: (area.height.saturating_sub(help_height)) / 2,
        width: help_width.min(area.width),
        height: help_height.min(area.height),
    };

    let help_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  q", Style::default().fg(theme.accent)),
            Span::styled("  quit", Style::default().fg(theme.text_dim)),
        ]),
        Line::from(vec![
            Span::styled("  p", Style::default().fg(theme.accent)),
            Span::styled("  pause/resume", Style::default().fg(theme.text_dim)),
        ]),
        Line::from(vec![
            Span::styled("  t", Style::default().fg(theme.accent)),
            Span::styled("  cycle theme", Style::default().fg(theme.text_dim)),
        ]),
        Line::from(vec![
            Span::styled("  j", Style::default().fg(theme.accent)),
            Span::styled("  print JSON", Style::default().fg(theme.text_dim)),
        ]),
        Line::from(vec![
            Span::styled("  ?", Style::default().fg(theme.accent)),
            Span::styled("  toggle help", Style::default().fg(theme.text_dim)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Press ", Style::default().fg(theme.text_dim)),
            Span::styled("Esc", Style::default().fg(theme.accent)),
            Span::styled(" to close", Style::default().fg(theme.text_dim)),
        ]),
    ];

    let help = Paragraph::new(help_text).block(
        Block::default()
            .title(" Keybindings ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border)),
    );

    frame.render_widget(Clear, help_area);
    frame.render_widget(help, help_area);
}
