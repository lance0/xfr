//! TUI rendering

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::app::{App, AppState};
use super::theme::Theme;
use super::widgets::Sparkline;
use crate::stats::{bytes_to_human, mbps_to_human};

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

pub fn draw(frame: &mut Frame, app: &App) {
    let size = frame.area();
    let theme = &app.theme;

    // Main layout: title + content + footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Title
            Constraint::Min(0),    // Main content
            Constraint::Length(1), // Footer
        ])
        .split(size);

    draw_title(frame, theme, chunks[0]);
    draw_content(frame, app, theme, chunks[1]);
    draw_footer(frame, app, theme, chunks[2]);

    if app.show_help {
        draw_help_overlay(frame, theme, size);
    }
}

fn draw_title(frame: &mut Frame, theme: &Theme, area: Rect) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled("[ ", Style::default().fg(theme.border)),
        Span::styled(
            "xfr",
            Style::default()
                .fg(theme.header)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " - Modern Rust TUI for Network Testing ",
            Style::default().fg(theme.text_dim),
        ),
        Span::styled("]", Style::default().fg(theme.border)),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(title, area);
}

fn draw_content(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    // Layout: Configuration + Real-time Stats + History
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),  // Configuration
            Constraint::Length(10), // Real-time Stats
            Constraint::Min(4),     // History
        ])
        .split(area);

    draw_configuration(frame, app, theme, chunks[0]);
    draw_realtime_stats(frame, app, theme, chunks[1]);
    draw_history(frame, app, theme, chunks[2]);
}

fn draw_configuration(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let block = Block::default()
        .title(Span::styled(
            " Configuration ",
            Style::default()
                .fg(theme.header)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let role = match app.direction {
        crate::protocol::Direction::Upload => "Client (Upload)",
        crate::protocol::Direction::Download => "Client (Download)",
        crate::protocol::Direction::Bidir => "Client (Bidirectional)",
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("  Role:      ", Style::default().fg(theme.text_dim)),
            Span::styled(role, Style::default().fg(theme.text)),
        ]),
        Line::from(vec![
            Span::styled("  Target:    ", Style::default().fg(theme.text_dim)),
            Span::styled(
                format!("{}:{}", app.host, app.port),
                Style::default().fg(theme.text),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Protocol:  ", Style::default().fg(theme.text_dim)),
            Span::styled(
                format!("{} ×{}", app.protocol, app.streams_count),
                Style::default().fg(theme.accent),
            ),
        ]),
    ];

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_realtime_stats(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let block = Block::default()
        .title(Span::styled(
            " Real-time Stats ",
            Style::default()
                .fg(theme.header)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Throughput sparkline + current value
            Constraint::Length(1), // Transfer progress
            Constraint::Length(1), // Spacer
            Constraint::Length(2), // Stats row
        ])
        .split(inner);

    // Throughput sparkline with current value
    let sparkline_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(14), // Current throughput value
            Constraint::Min(10),    // Sparkline graph
        ])
        .split(chunks[0]);

    // Current throughput value (big number)
    let throughput_str = mbps_to_human(app.current_throughput_mbps);
    let throughput_display = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            throughput_str,
            Style::default()
                .fg(theme.graph_primary)
                .add_modifier(Modifier::BOLD),
        )),
    ]);
    frame.render_widget(throughput_display, sparkline_chunks[0]);

    // Sparkline showing throughput history
    if !app.throughput_history.is_empty() {
        let data: Vec<f64> = app.throughput_history.iter().cloned().collect();
        let sparkline = Sparkline::new(&data)
            .max(app.max_throughput().max(100.0))
            .style(Style::default().fg(theme.graph_primary));
        let sparkline_area = Rect {
            x: sparkline_chunks[1].x,
            y: sparkline_chunks[1].y,
            width: sparkline_chunks[1].width,
            height: 3,
        };
        frame.render_widget(sparkline, sparkline_area);
    }

    // Transfer progress line
    let progress = app.progress_percent() / 100.0;
    let transferred = bytes_to_human(app.total_bytes);
    let elapsed_secs = app.elapsed.as_secs();
    let duration_secs = app.duration.as_secs();

    // Build arrow-style progress bar: [====>------] 45%
    // Use inner width for calculation, with minimum of 10 chars for bar
    let prefix_len = 12; // "  Transfer: "
    let suffix_len = 25; // " X.XX GB / Xs [" + "] XX%"
    let available_width = (inner.width as usize).saturating_sub(prefix_len + suffix_len);
    let bar_width = available_width.clamp(10, 40);

    let filled = (progress * bar_width as f64) as usize;
    let empty = bar_width.saturating_sub(filled);
    let arrow = if filled > 0 && filled < bar_width {
        ">"
    } else {
        ""
    };
    let fill_chars = if arrow.is_empty() {
        filled
    } else {
        filled.saturating_sub(1)
    };

    let progress_bar = format!(
        "[{}{}{}]",
        "=".repeat(fill_chars),
        arrow,
        "-".repeat(empty)
    );

    let transfer_line = Line::from(vec![
        Span::styled("  Transfer: ", Style::default().fg(theme.text_dim)),
        Span::styled(format!("{} ", transferred), Style::default().fg(theme.text)),
        Span::styled(progress_bar, Style::default().fg(theme.graph_secondary)),
        Span::styled(
            format!(" {}s/{}s", elapsed_secs, duration_secs),
            Style::default().fg(theme.text),
        ),
    ]);
    frame.render_widget(Paragraph::new(transfer_line), chunks[1]);

    // Stats row: Current/Average Speed | Jitter/Packet Loss
    let stats_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[3]);

    let current_speed = mbps_to_human(app.current_throughput_mbps);
    let avg_speed = mbps_to_human(app.average_throughput_mbps);

    let speed_lines = vec![
        Line::from(vec![
            Span::styled("  Current Speed: ", Style::default().fg(theme.text_dim)),
            Span::styled(current_speed, Style::default().fg(theme.graph_primary)),
        ]),
        Line::from(vec![
            Span::styled("  Average Speed: ", Style::default().fg(theme.text_dim)),
            Span::styled(avg_speed, Style::default().fg(theme.text)),
        ]),
    ];
    frame.render_widget(Paragraph::new(speed_lines), stats_chunks[0]);

    // Right side: Jitter/Loss for UDP, RTT/Retrans for TCP
    let quality_lines = if app.protocol == crate::protocol::Protocol::Udp {
        let jitter_col = jitter_color(app.udp_jitter_ms, theme);
        let loss_col = loss_color(app.udp_lost_percent, theme);
        vec![
            Line::from(vec![
                Span::styled("Jitter:       ", Style::default().fg(theme.text_dim)),
                Span::styled(
                    format!("{:.2} ms", app.udp_jitter_ms),
                    Style::default().fg(jitter_col),
                ),
            ]),
            Line::from(vec![
                Span::styled("Packet Loss:  ", Style::default().fg(theme.text_dim)),
                Span::styled(
                    format!("{:.1}%", app.udp_lost_percent),
                    Style::default().fg(loss_col),
                ),
            ]),
        ]
    } else {
        let rtt_ms = app.rtt_us as f64 / 1000.0;
        vec![
            Line::from(vec![
                Span::styled("RTT:          ", Style::default().fg(theme.text_dim)),
                Span::styled(format!("{:.2} ms", rtt_ms), Style::default().fg(theme.text)),
            ]),
            Line::from(vec![
                Span::styled("Retransmits:  ", Style::default().fg(theme.text_dim)),
                Span::styled(
                    format!("{}", app.total_retransmits),
                    Style::default().fg(if app.total_retransmits == 0 {
                        theme.success
                    } else {
                        theme.warning
                    }),
                ),
            ]),
        ]
    };
    frame.render_widget(Paragraph::new(quality_lines), stats_chunks[1]);
}

fn draw_history(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let block = Block::default()
        .title(Span::styled(
            " History ",
            Style::default()
                .fg(theme.header)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Show history entries (most recent first)
    let mut lines = Vec::new();
    for entry in app.history.iter().take(inner.height as usize) {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  [{}] ", entry.timestamp),
                Style::default().fg(theme.text_dim),
            ),
            Span::styled(&entry.message, Style::default().fg(theme.text)),
        ]));
    }

    // If no history, show status message
    if lines.is_empty() {
        let status_msg = match app.state {
            AppState::Connecting => "  Connecting...",
            AppState::Running => "  Test running...",
            AppState::Paused => "  Test paused",
            AppState::Completed => "  Test complete",
            AppState::Error => app.error.as_deref().unwrap_or("  Error"),
        };
        lines.push(Line::from(Span::styled(
            status_msg,
            Style::default().fg(theme.text_dim),
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_footer(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let status_text = match app.state {
        AppState::Connecting => "Connecting...",
        AppState::Running => "Running...",
        AppState::Paused => "Paused",
        AppState::Completed => "Complete",
        AppState::Error => "Error",
    };

    let status_color = match app.state {
        AppState::Connecting => theme.text_dim,
        AppState::Running => theme.success,
        AppState::Paused => theme.warning,
        AppState::Completed => theme.success,
        AppState::Error => theme.error,
    };

    let footer = Line::from(vec![
        Span::styled("[", Style::default().fg(theme.text_dim)),
        Span::styled("q", Style::default().fg(theme.accent)),
        Span::styled("] Quit | [", Style::default().fg(theme.text_dim)),
        Span::styled("p", Style::default().fg(theme.accent)),
        Span::styled("] Pause | [", Style::default().fg(theme.text_dim)),
        Span::styled("t", Style::default().fg(theme.accent)),
        Span::styled("] Theme | [", Style::default().fg(theme.text_dim)),
        Span::styled("?", Style::default().fg(theme.accent)),
        Span::styled("] Help | Status: ", Style::default().fg(theme.text_dim)),
        Span::styled(status_text, Style::default().fg(status_color)),
    ]);

    frame.render_widget(Paragraph::new(footer), area);
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
