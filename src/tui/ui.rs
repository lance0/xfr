//! TUI rendering

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::app::{App, AppState};
use super::settings::SettingsCategory;
use super::theme::Theme;
use super::widgets::{Sparkline, StreamBar};
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

    // Main layout: title + optional update banner + content + footer
    let has_update = app.update_available.is_some();
    let chunks = if has_update {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Title
                Constraint::Length(1), // Update banner
                Constraint::Min(0),    // Main content
                Constraint::Length(1), // Footer
            ])
            .split(size)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Title
                Constraint::Min(0),    // Main content
                Constraint::Length(1), // Footer
            ])
            .split(size)
    };

    draw_title(frame, theme, chunks[0]);

    if has_update {
        draw_update_banner(frame, app, theme, chunks[1]);
        draw_content(frame, app, theme, chunks[2]);
        draw_footer(frame, app, theme, chunks[3]);
    } else {
        draw_content(frame, app, theme, chunks[1]);
        draw_footer(frame, app, theme, chunks[2]);
    }

    // Show pause overlay (behind other modals)
    if app.state == AppState::Paused && !app.show_help && !app.settings.visible {
        draw_pause_overlay(frame, theme, size);
    }

    if app.show_help {
        draw_help_overlay(frame, theme, size);
    }

    if app.settings.visible {
        draw_settings_modal(frame, app, theme, size);
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

fn draw_update_banner(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    if let Some(ref version) = app.update_available {
        let install_method = crate::update::InstallMethod::detect();
        let update_cmd = install_method.update_command();
        let new_ver = version.strip_prefix('v').unwrap_or(version);
        let text = format!(
            " Update available: v{} → v{} | {} | Press 'u' to dismiss ",
            env!("CARGO_PKG_VERSION"),
            new_ver,
            update_cmd
        );
        let banner = Paragraph::new(text)
            .style(Style::default().fg(Color::Black).bg(theme.warning))
            .alignment(Alignment::Center);
        frame.render_widget(banner, area);
    }
}

fn draw_content(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    // Layout: Configuration + Real-time Stats + History/Streams
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),  // Configuration
            Constraint::Length(10), // Real-time Stats
            Constraint::Min(4),     // History or Streams
        ])
        .split(area);

    draw_configuration(frame, app, theme, chunks[0]);
    draw_realtime_stats(frame, app, theme, chunks[1]);

    // Show streams panel or history based on toggle
    if app.show_streams && app.streams_count > 1 {
        draw_streams(frame, app, theme, chunks[2]);
    } else {
        draw_history(frame, app, theme, chunks[2]);
    }
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

    // Build arrow-style progress bar: [====>------] 45%
    // Adaptive width: use available space with minimum of 10 chars
    let prefix_len = 12; // "  Transfer: "
    let suffix_len = 18; // " Xs/Xs" + "[]"
    let transferred_len = transferred.len() + 1;
    let available_width =
        (inner.width as usize).saturating_sub(prefix_len + suffix_len + transferred_len);
    let bar_width = available_width.max(10);

    let (progress_bar, time_display) = if app.is_infinite() {
        // Infinite duration: show elapsed time with ∞
        let filled = bar_width; // Full bar since no target
        (
            format!("[{}]", "=".repeat(filled)),
            format!(" {}s/∞", elapsed_secs),
        )
    } else {
        let duration_secs = app.duration.as_secs();
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
        (
            format!("[{}{}{}]", "=".repeat(fill_chars), arrow, "-".repeat(empty)),
            format!(" {}s/{}s", elapsed_secs, duration_secs),
        )
    };

    let transfer_line = Line::from(vec![
        Span::styled("  Transfer: ", Style::default().fg(theme.text_dim)),
        Span::styled(format!("{} ", transferred), Style::default().fg(theme.text)),
        Span::styled(progress_bar, Style::default().fg(theme.graph_secondary)),
        Span::styled(time_display, Style::default().fg(theme.text)),
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

fn draw_streams(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let block = Block::default()
        .title(Span::styled(
            " Streams ",
            Style::default()
                .fg(theme.header)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Find max throughput for scaling bars
    let max_throughput = app
        .streams
        .iter()
        .map(|s| s.throughput_mbps)
        .fold(0.0f64, f64::max)
        .max(100.0); // Minimum scale of 100 Mbps

    // Draw each stream bar
    for (i, stream) in app.streams.iter().enumerate() {
        if i as u16 >= inner.height {
            break;
        }
        let stream_area = Rect {
            x: inner.x,
            y: inner.y + i as u16,
            width: inner.width,
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

    let mut spans = vec![
        Span::styled("[", Style::default().fg(theme.text_dim)),
        Span::styled("q", Style::default().fg(theme.accent)),
        Span::styled("] Quit | [", Style::default().fg(theme.text_dim)),
        Span::styled("p", Style::default().fg(theme.accent)),
        Span::styled("] Pause | [", Style::default().fg(theme.text_dim)),
        Span::styled("s", Style::default().fg(theme.accent)),
        Span::styled("] Settings | [", Style::default().fg(theme.text_dim)),
        Span::styled("?", Style::default().fg(theme.accent)),
        Span::styled("] Help | [", Style::default().fg(theme.text_dim)),
        Span::styled("t", Style::default().fg(theme.accent)),
        Span::styled("] Theme", Style::default().fg(theme.text_dim)),
    ];

    // Add streams toggle hint when multiple streams exist
    if app.streams_count > 1 {
        spans.push(Span::styled(" | [", Style::default().fg(theme.text_dim)));
        spans.push(Span::styled("d", Style::default().fg(theme.accent)));
        spans.push(Span::styled(
            "] Streams",
            Style::default().fg(theme.text_dim),
        ));
    }

    // Add dismiss hint when update available
    if app.update_available.is_some() {
        spans.push(Span::styled(" | [", Style::default().fg(theme.text_dim)));
        spans.push(Span::styled("u", Style::default().fg(theme.accent)));
        spans.push(Span::styled(
            "] Dismiss",
            Style::default().fg(theme.text_dim),
        ));
    }

    spans.push(Span::styled(
        " | Status: ",
        Style::default().fg(theme.text_dim),
    ));
    spans.push(Span::styled(status_text, Style::default().fg(status_color)));

    let footer = Line::from(spans);

    frame.render_widget(Paragraph::new(footer), area);
}

fn draw_help_overlay(frame: &mut Frame, theme: &Theme, area: Rect) {
    let help_width = 36;
    let help_height = 14;
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
            Span::styled("  s", Style::default().fg(theme.accent)),
            Span::styled("  settings", Style::default().fg(theme.text_dim)),
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
            Span::styled("  d", Style::default().fg(theme.accent)),
            Span::styled("  toggle streams", Style::default().fg(theme.text_dim)),
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

fn draw_pause_overlay(frame: &mut Frame, theme: &Theme, area: Rect) {
    let pause_width = 20u16;
    let pause_height = 5u16;
    let pause_area = Rect {
        x: area.width.saturating_sub(pause_width) / 2,
        y: area.height.saturating_sub(pause_height) / 2,
        width: pause_width.min(area.width),
        height: pause_height.min(area.height),
    };

    let pause_text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "PAUSED",
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Press p to resume",
            Style::default().fg(theme.text_dim),
        )),
    ];

    let pause = Paragraph::new(pause_text)
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.warning)),
        );

    frame.render_widget(Clear, pause_area);
    frame.render_widget(pause, pause_area);
}

fn draw_settings_modal(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let settings = &app.settings;

    // Modal dimensions
    let modal_width = 58u16;
    let modal_height = 18u16;
    let modal_area = Rect {
        x: area.width.saturating_sub(modal_width) / 2,
        y: area.height.saturating_sub(modal_height) / 2,
        width: modal_width.min(area.width),
        height: modal_height.min(area.height),
    };

    // Clear background and draw modal border
    let block = Block::default()
        .title(Span::styled(
            " Settings ",
            Style::default()
                .fg(theme.header)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));

    let inner = block.inner(modal_area);
    frame.render_widget(Clear, modal_area);
    frame.render_widget(block, modal_area);

    // Layout: tabs, content, buttons, help
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // Tabs
            Constraint::Min(8),    // Content
            Constraint::Length(2), // Buttons
            Constraint::Length(1), // Help line
        ])
        .split(inner);

    // Draw category tabs
    draw_settings_tabs(frame, settings, theme, chunks[0]);

    // Draw content based on category
    match settings.category {
        SettingsCategory::Display => draw_display_settings(frame, app, settings, theme, chunks[1]),
        SettingsCategory::Test => draw_test_settings(frame, settings, theme, chunks[1]),
    }

    // Draw buttons
    draw_settings_buttons(frame, settings, theme, chunks[2]);

    // Draw help line
    let help_line = Line::from(vec![
        Span::styled("↑↓", Style::default().fg(theme.accent)),
        Span::styled("/", Style::default().fg(theme.text_dim)),
        Span::styled("jk", Style::default().fg(theme.accent)),
        Span::styled(" Navigate  ", Style::default().fg(theme.text_dim)),
        Span::styled("←→", Style::default().fg(theme.accent)),
        Span::styled("/", Style::default().fg(theme.text_dim)),
        Span::styled("hl", Style::default().fg(theme.accent)),
        Span::styled(" Change  ", Style::default().fg(theme.text_dim)),
        Span::styled("Tab", Style::default().fg(theme.accent)),
        Span::styled(" Switch  ", Style::default().fg(theme.text_dim)),
        Span::styled("Esc", Style::default().fg(theme.accent)),
        Span::styled(" Close", Style::default().fg(theme.text_dim)),
    ]);
    frame.render_widget(
        Paragraph::new(help_line).alignment(Alignment::Center),
        chunks[3],
    );
}

fn draw_settings_tabs(
    frame: &mut Frame,
    settings: &super::settings::SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let display_style = if settings.category == SettingsCategory::Display {
        Style::default()
            .fg(theme.header)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text_dim)
    };

    let test_style = if settings.category == SettingsCategory::Test {
        Style::default()
            .fg(theme.header)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text_dim)
    };

    let tabs = Line::from(vec![
        Span::raw("  "),
        Span::styled("[", Style::default().fg(theme.border)),
        Span::styled("Display", display_style),
        Span::styled("]", Style::default().fg(theme.border)),
        Span::raw("  "),
        Span::styled("[", Style::default().fg(theme.border)),
        Span::styled("Test", test_style),
        Span::styled("]", Style::default().fg(theme.border)),
    ]);

    frame.render_widget(Paragraph::new(tabs), area);
}

fn draw_display_settings(
    frame: &mut Frame,
    _app: &App,
    settings: &super::settings::SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let theme_list = super::theme::Theme::list();
    let theme_name = theme_list
        .get(settings.theme_index)
        .copied()
        .unwrap_or("default");

    let items: Vec<(&str, String)> = vec![
        ("Theme:", theme_name.to_string()),
        ("Timestamp:", settings.timestamp_format.as_str().to_string()),
        ("Units:", settings.units.as_str().to_string()),
    ];

    draw_setting_items(
        frame,
        &items,
        settings.selected_index,
        settings.button_focused,
        theme,
        area,
    );
}

fn draw_test_settings(
    frame: &mut Frame,
    settings: &super::settings::SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let items: Vec<(&str, String)> = vec![
        ("Streams:", format!("{}", settings.streams)),
        ("Protocol:", format!("{}", settings.protocol)),
        ("Duration:", format!("{}s", settings.duration_secs)),
        ("Direction:", format!("{:?}", settings.direction)),
    ];

    draw_setting_items(
        frame,
        &items,
        settings.selected_index,
        settings.button_focused,
        theme,
        area,
    );
}

fn draw_setting_items(
    frame: &mut Frame,
    items: &[(&str, String)],
    selected_index: usize,
    button_focused: bool,
    theme: &Theme,
    area: Rect,
) {
    let mut lines = Vec::new();
    lines.push(Line::from("")); // Top padding

    for (i, (label, value)) in items.iter().enumerate() {
        let is_selected = !button_focused && i == selected_index;

        let prefix = if is_selected { "▶ " } else { "  " };
        let label_style = Style::default().fg(theme.text_dim);
        let value_style = if is_selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };

        // Add arrows for selected item
        let value_display = if is_selected {
            format!("◀ {} ▶", value)
        } else {
            value.clone()
        };

        lines.push(Line::from(vec![
            Span::raw(prefix),
            Span::styled(format!("{:<14}", label), label_style),
            Span::styled(value_display, value_style),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_settings_buttons(
    frame: &mut Frame,
    settings: &super::settings::SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let show_apply = settings.test_params_dirty();

    let apply_style = if settings.button_focused && settings.button_index == 0 {
        Style::default()
            .fg(theme.success)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text_dim)
    };

    let close_style = if settings.button_focused && settings.button_index == 1 {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text_dim)
    };

    let mut buttons = vec![Span::raw("          ")]; // Left padding

    if show_apply {
        buttons.push(Span::styled("[Apply & Restart]", apply_style));
        buttons.push(Span::raw("  "));
    }

    buttons.push(Span::styled("[Esc]", close_style));

    let button_line = Line::from(buttons);
    frame.render_widget(
        Paragraph::new(button_line).alignment(Alignment::Center),
        area,
    );
}
