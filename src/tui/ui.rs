//! TUI rendering

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

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

/// Style a throughput sparkline cell by the per-interval loss rate. Unknown
/// rate stays primary so we don't fake a signal we don't have; clean
/// intervals stay primary; light loss (<1%) goes warning, heavy loss
/// (>=1%) goes error. Thresholds intentionally differ from `loss_color`'s
/// (cumulative): a per-interval rate of 0.05% in a single second is a
/// real burst worth tinting, while the same number averaged across an
/// entire run is healthy. We don't share the green/yellow/red palette
/// either — the sparkline's "clean" state is the graph color so the
/// background blends with the chart, not the success-green used in the
/// stats panel.
///
/// Severity is also encoded without hue (issue #158): lossy columns are
/// UNDERLINED — a baseline tick that renders identically at every bar
/// height, giving colorblind users, glare, and NO_COLOR a "loss happened
/// here" mark in every theme. In a hue-free theme, where the palette can't
/// separate light from heavy at all, heavy loss adds REVERSED. That is a
/// *signal to the Sparkline widget*, not a style handed to the terminal:
/// the widget stipples the empty cells above the bar so the whole column
/// is marked while the bar keeps its true height.
///
/// REVERSED is deliberately NOT applied in color themes (#93 follow-up,
/// reported by brettowe against v0.9.24): reverse video swaps fg/bg
/// *within each cell*, and a partial bar glyph like `▅` only covers the
/// cell's bottom — the swap painted the empty top fraction of the cell
/// and hid the bar, so heavy-loss columns read as red fragments floating
/// at the top of the graph. Color themes carry severity in the hue they
/// already have; the stipple exists for the theme that has nothing else.
///
/// The widget used to render REVERSED literally, filling the empty cells so
/// the swap produced "a solid pillar with the bar as a negative silhouette".
/// It did produce a negative — which is the problem. A full block under
/// reverse video paints in the background colour and vanishes, so the
/// visible shape was the bar's complement and a marked column read as a bar
/// of *inverted* height: 10% looked nearly full, 90% looked nearly empty,
/// while its unmarked neighbours in the same row still read normally.
fn loss_severity_style(rate: Option<f64>, theme: &Theme) -> Style {
    match rate {
        None => Style::default().fg(theme.graph_primary),
        Some(r) if r <= 0.0 => Style::default().fg(theme.graph_primary),
        Some(r) if r < 1.0 => Style::default()
            .fg(theme.warning)
            .add_modifier(Modifier::UNDERLINED),
        Some(_) if theme.hue_free() => Style::default()
            .fg(theme.error)
            .add_modifier(Modifier::UNDERLINED | Modifier::REVERSED),
        Some(_) => Style::default()
            .fg(theme.error)
            .add_modifier(Modifier::UNDERLINED),
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

    // Show pause overlay (behind other modals). The compact view (content
    // under 10 rows) already reads "Paused" and the box would hide its rate.
    let compact = chunks[chunks.len() - 2].height < 10;
    if app.state == AppState::Paused && !compact && !app.show_help && !app.settings.visible {
        draw_pause_overlay(frame, theme, size);
    }

    if app.show_help {
        draw_help_overlay(frame, theme, size, app.help_scroll, false);
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
            concat!(" v", env!("CARGO_PKG_VERSION"), " "),
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
    if area.height < 20 {
        if app.state == AppState::Error {
            frame.render_widget(
                Paragraph::new(format!(
                    "Error: {}",
                    app.error.as_deref().unwrap_or("Test failed")
                ))
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(theme.error)),
                area,
            );
        } else if area.height >= 10 {
            draw_realtime_stats(frame, app, theme, area);
        } else {
            let elapsed = app.elapsed.as_secs();
            let transferred = bytes_to_human(app.total_bytes);
            let progress = if let Some(budget) = app.byte_budget.filter(|b| *b > 0) {
                format!("{} / {}  {}s", transferred, bytes_to_human(budget), elapsed)
            } else if app.is_infinite() {
                format!("{}s / ∞  {}", elapsed, transferred)
            } else {
                format!(
                    "{}s / {}s  {}",
                    elapsed,
                    app.duration.as_secs(),
                    transferred
                )
            };
            let rate = mbps_to_human(if app.state == AppState::Completed {
                app.average_throughput_mbps
            } else {
                app.current_throughput_mbps
            });
            // The rate is the point of this view: shorten the state label,
            // then drop it, before the rate would be cut mid-unit.
            let state = app.state.to_string();
            let headline = [state.as_str(), state.trim_end_matches('.')]
                .into_iter()
                .map(|state| format!("{state}  {rate}"))
                .find(|line| line.chars().count() <= usize::from(area.width))
                .unwrap_or(rate);
            let lines = vec![Line::from(headline), Line::from(progress)];
            frame.render_widget(
                Paragraph::new(lines).style(Style::default().fg(theme.text)),
                area,
            );
        }
        return;
    }
    // Layout: Configuration + Real-time Stats + History/Streams
    // Configuration is 6 lines tall: 2 border rows + 4 content rows
    // (Role, Target, Protocol, Version).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),  // Configuration
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

    let server_label: &str = match app.server_version.as_deref() {
        Some(v) => v,
        None => "(waiting…)",
    };
    let version_value = format!("xfr/{} ↔ {}", env!("CARGO_PKG_VERSION"), server_label);

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
        Line::from(vec![
            Span::styled("  Version:   ", Style::default().fg(theme.text_dim)),
            Span::styled(version_value, Style::default().fg(theme.text_dim)),
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

    // The gap between the graph and the transfer bar is a luxury, and the
    // panel only has 8 inner rows to spend at full height. On a short
    // terminal ratatui shrinks these chunks, and a row spent on whitespace
    // is a row taken from the sparkline — at 15 rows it blanked the graph
    // completely. Collapse the gap to nothing rather than the data.
    let graph_gap = u16::from(inner.height >= 8);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),         // Throughput sparkline + current value
            Constraint::Length(graph_gap), // Spacer
            Constraint::Length(1),         // Transfer progress
            Constraint::Length(1),         // Spacer
            Constraint::Length(2),         // Stats row
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

    // Current throughput value. For bidir tests with per-direction numbers
    // available, show "↑ X / ↓ Y" split; otherwise show the combined value.
    let throughput_display = if app.direction == crate::protocol::Direction::Bidir
        && (app.throughput_send_mbps > 0.0 || app.throughput_recv_mbps > 0.0)
    {
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("↑ {}", mbps_to_human(app.throughput_send_mbps)),
                Style::default()
                    .fg(theme.graph_primary)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("↓ {}", mbps_to_human(app.throughput_recv_mbps)),
                Style::default()
                    .fg(theme.graph_primary)
                    .add_modifier(Modifier::BOLD),
            )),
        ])
    } else {
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                mbps_to_human(app.current_throughput_mbps),
                Style::default()
                    .fg(theme.graph_primary)
                    .add_modifier(Modifier::BOLD),
            )),
        ])
    };
    frame.render_widget(throughput_display, sparkline_chunks[0]);

    // Sparkline showing throughput history. Intervals are graded by their
    // per-interval loss rate so a single-packet hiccup and a heavy drop
    // burst render distinctly: clean → primary, light loss → warning,
    // heavy loss → error. When loss magnitude is unknown (TCP run,
    // pre-0.9.11 server, or first UDP sample) the bar stays primary —
    // honest "no signal" rather than a misleading tint.
    if !app.throughput_history.is_empty() {
        let data: Vec<f64> = app.throughput_history.iter().map(|s| s.mbps).collect();
        let styles: Vec<Style> = app
            .throughput_history
            .iter()
            .map(|s| loss_severity_style(s.loss_rate_percent(), theme))
            .collect();
        let sparkline = Sparkline::new(&data)
            .max(app.max_throughput().max(100.0))
            .style(Style::default().fg(theme.graph_primary))
            .styles(&styles);
        // Render into the allotted chunk rather than a hand-built rect with a
        // hardcoded height. The band is `Constraint::Length(3)`, so this is the
        // same 3 rows on any normal terminal — but when the window is too short
        // for the full layout, ratatui shrinks the chunk to 0-2 rows and a
        // fixed height of 3 spilled the bars over the panel's bottom border
        // (visible below ~16 rows).
        frame.render_widget(sparkline, sparkline_chunks[1]);
    }

    // Transfer progress line: an eighth-resolution block bar plus elapsed
    // against the test's own denominator.
    let transferred = bytes_to_human(app.total_bytes);
    let elapsed_secs = app.elapsed.as_secs();
    let (bar_width, filled_eighths) = progress_bar_cells(app, inner.width);
    let full_cells = filled_eighths / 8;
    let remainder = filled_eighths % 8;
    let mut bar_filled = BAR_FULL.to_string().repeat(full_cells);
    if remainder > 0 {
        bar_filled.push(BAR_PARTIALS[remainder - 1]);
    }
    let drawn = full_cells + usize::from(remainder > 0);
    let bar_empty = BAR_EMPTY
        .to_string()
        .repeat(bar_width.saturating_sub(drawn));
    let time_display = if let Some(budget) = app.byte_budget.filter(|b| *b > 0) {
        // `-n`: the requested transfer size is the denominator, the way a
        // timed test shows its duration.
        format!(" {}s/{}", elapsed_secs, bytes_to_human(budget))
    } else if app.is_infinite() {
        format!(" {}s/∞", elapsed_secs)
    } else {
        format!(" {}s/{}s", elapsed_secs, app.duration.as_secs())
    };

    let transfer_line = Line::from(vec![
        Span::styled("  Transfer: ", Style::default().fg(theme.text_dim)),
        Span::styled(format!("{} ", transferred), Style::default().fg(theme.text)),
        Span::styled(bar_filled, Style::default().fg(theme.graph_primary)),
        Span::styled(bar_empty, Style::default().fg(theme.text_dim)),
        Span::styled(time_display, Style::default().fg(theme.text)),
    ]);
    frame.render_widget(Paragraph::new(transfer_line), chunks[2]);

    // Stats row: Current/Average Speed | Jitter/Packet Loss
    let stats_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[4]);

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
    let quality_lines = if app.protocol == crate::protocol::Protocol::Udp
        && app.direction == crate::protocol::Direction::Bidir
    {
        vec![
            udp_direction_line("Upload", &app.udp_send, theme),
            udp_direction_line("Download", &app.udp_recv, theme),
        ]
    } else if app.protocol == crate::protocol::Protocol::Udp {
        // Jitter line shows both the latest per-interval aggregate (primary,
        // color-coded against the thresholds) and the 10-second rolling mean
        // in parentheses while running. On completion only the authoritative
        // final value is shown. See App::jitter_display (issue #48 follow-up).
        let jd = app.jitter_display();
        let jitter_col = jitter_color(jd.primary, theme);
        let jitter_value = match jd.smoothed {
            Some(avg) => format!("{:.2} ms (10s avg: {:.2} ms)", jd.primary, avg),
            None => format!("{:.2} ms", jd.primary),
        };
        // Loss render distinguishes "fresh 0%" from "no data yet" — None
        // displays as a dimmed `--%` so users on a pre-0.9.11 server don't
        // mistake stale silence for clean traffic. The color also drops to
        // text_dim in that case rather than reusing the success/warning/
        // error palette, which is reserved for actual measurements.
        let (loss_text, loss_style) = match app.udp_lost_percent {
            Some(p) => (
                format!("{:.1}%", p),
                Style::default().fg(loss_color(p, theme)),
            ),
            None => ("--%".to_string(), Style::default().fg(theme.text_dim)),
        };
        vec![
            Line::from(vec![
                Span::styled("Jitter:       ", Style::default().fg(theme.text_dim)),
                Span::styled(jitter_value, Style::default().fg(jitter_col)),
            ]),
            Line::from(vec![
                Span::styled("Packet Loss:  ", Style::default().fg(theme.text_dim)),
                Span::styled(loss_text, loss_style),
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

fn udp_direction_line<'a>(
    label: &str,
    direction: &super::app::UdpDirectionState,
    theme: &Theme,
) -> Line<'a> {
    let (loss, loss_style) = match direction.lost_percent() {
        Some(percent) => (
            format!("{percent:.1}%"),
            Style::default().fg(loss_color(percent, theme)),
        ),
        None => ("--%".to_string(), Style::default().fg(theme.text_dim)),
    };
    let (jitter, jitter_style) = match direction.jitter_ms {
        Some(jitter) => (
            format!("{jitter:.2}ms"),
            Style::default().fg(jitter_color(jitter, theme)),
        ),
        None => ("--ms".to_string(), Style::default().fg(theme.text_dim)),
    };
    Line::from(vec![
        Span::styled(
            format!("{label}: loss "),
            Style::default().fg(theme.text_dim),
        ),
        Span::styled(loss, loss_style),
        Span::styled(" jitter ", Style::default().fg(theme.text_dim)),
        Span::styled(jitter, jitter_style),
    ])
}

/// Progress-bar geometry as `(bar_width, filled)` cells for the Real-time
/// Stats block's inner width. Adaptive: whatever is left after the label,
/// the transferred-bytes figure and the time suffix, minimum 10 cells. A
/// `-n` test fills by bytes, a timed test by elapsed time, an open-ended
/// test shows a full bar. Shared with `App::render_fingerprint` so the
/// dirty check sees exactly the cell-quantized fill the renderer draws.
/// Progress-bar glyphs, the sparkline's eighth resolution turned on its side.
/// `BAR_PARTIALS` are the left-aligned partial blocks (1/8 through 7/8), so the
/// bar advances an eighth of a cell at a time instead of jumping a whole
/// character. `BAR_FULL` against `BAR_EMPTY` differs in texture, not just hue,
/// so progress stays readable on the hue-free themes and under `NO_COLOR` —
/// the same reason the heavy-loss marker stipples rather than recolors.
const BAR_FULL: char = '█';
const BAR_EMPTY: char = '░';
const BAR_PARTIALS: [char; 7] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉'];

/// Bar width in cells and how much of it is filled, in **eighths of a cell**.
///
/// The renderer draws at eighth resolution, so this reports eighths: the
/// fingerprint in `App::render_fingerprint` hashes the result to decide
/// whether a frame changed, and a whole-cell count would let up to seven
/// eighths of visible movement pass unnoticed and freeze the bar.
pub(super) fn progress_bar_cells(app: &App, inner_width: u16) -> (usize, usize) {
    let prefix_len = 12; // "  Transfer: "
    let suffix_len = 16; // " Xs/Xs" (the bar carries no [] delimiters)
    let transferred_len = bytes_to_human(app.total_bytes).len() + 1;
    let bar_width = (inner_width as usize)
        .saturating_sub(prefix_len + suffix_len + transferred_len)
        .max(10);
    let total_eighths = bar_width * 8;
    let filled_eighths = if app.byte_budget.is_none_or(|b| b == 0) && app.is_infinite() {
        total_eighths
    } else {
        // Clamped: progress_percent can read past 100 on an overshooting `-n`
        // transfer, and the renderer repeats these counts into a String.
        ((app.progress_percent() / 100.0 * total_eighths as f64) as usize).min(total_eighths)
    };
    (bar_width, filled_eighths)
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
        .jitter(stream.jitter_ms)
        .directions(stream.throughput_send_mbps, stream.throughput_recv_mbps)
        .bar_color(theme.graph_primary)
        .text_color(theme.text)
        .dim_color(theme.text_dim);

        frame.render_widget(bar, stream_area);
    }
}

fn draw_footer(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    // A pending quit overrides the state label: show a live countdown so a
    // cancel on a dead link reads as a bounded wait, not a hang (issue #159).
    let cancel_wait_secs = app.cancel_wait_secs();

    let status_text = match cancel_wait_secs {
        Some(secs) => format!("Waiting for server ({secs}s)..."),
        None => app.state.to_string(),
    };

    let status_color = if cancel_wait_secs.is_some() {
        theme.warning
    } else {
        match app.state {
            AppState::Connecting => theme.text_dim,
            AppState::Running => theme.success,
            AppState::Paused => theme.warning,
            AppState::Completed => theme.success,
            AppState::Error => theme.error,
        }
    };

    // Help leads: hints are dropped from the right as the width shrinks, and
    // help lists every other key.
    let mut keys = vec![
        ("?", "Help"),
        ("q", "Quit"),
        ("p", "Pause"),
        ("r", "Restart"),
        ("s", "Settings"),
        ("t", "Theme"),
    ];
    if app.streams_count > 1 {
        keys.push(("d", "Streams"));
    }
    if app.update_available.is_some() {
        keys.push(("u", "Dismiss"));
    }

    // Status is the only thing down here that changes, so it gets its own
    // chunk on the right and the key hints absorb any clipping. Laid out as
    // one line, the full hint list runs ~107 columns and an 80-column
    // terminal used to lose "Status: ..." off the right edge entirely —
    // truncating the single field worth reading. The `?:Help` hint still
    // outranks it: on very narrow terminals Status loses its label first,
    // then disappears.
    let room = usize::from(area.width).saturating_sub("?:Help".len());
    let status_label = [" Status: ", " "]
        .into_iter()
        .find(|label| label.len() + status_text.chars().count() <= room);
    let status_width =
        status_label.map_or(0, |label| label.len() + status_text.chars().count()) as u16;
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(status_width)])
        .split(area);

    // "[q] Quit | ..." costs len(key) + len(label) + 3 per hint plus 3-column
    // separators; the compact "q:Quit ..." form drops to len+1 and single
    // spaces, which fits the default hint set beside Status on 80 columns.
    let full_width: usize = keys
        .iter()
        .map(|(k, l)| k.len() + l.len() + 3)
        .sum::<usize>()
        + 3 * keys.len().saturating_sub(1);
    let compact = full_width > chunks[0].width as usize;

    // Drop whole hints that don't fit rather than letting ratatui clip the
    // last one part-way through its label. A shorter list reads as a shorter
    // list; half a label reads as a rendering bug.
    let sep_width = if compact { 1 } else { 3 };
    let hint_pad = if compact { 1 } else { 3 };
    let available = chunks[0].width as usize;
    let mut used = 0usize;

    let mut spans = Vec::with_capacity(keys.len() * 3);
    for (i, (key, label)) in keys.iter().enumerate() {
        let sep_cost = if i > 0 { sep_width } else { 0 };
        let cost = sep_cost + key.len() + label.len() + hint_pad;
        if used + cost > available {
            break;
        }
        used += cost;

        if i > 0 {
            let sep = if compact { " " } else { " | " };
            spans.push(Span::styled(sep, Style::default().fg(theme.text_dim)));
        }
        if compact {
            spans.push(Span::styled(*key, Style::default().fg(theme.accent)));
            spans.push(Span::styled(
                format!(":{}", label),
                Style::default().fg(theme.text_dim),
            ));
        } else {
            spans.push(Span::styled("[", Style::default().fg(theme.text_dim)));
            spans.push(Span::styled(*key, Style::default().fg(theme.accent)));
            spans.push(Span::styled(
                format!("] {}", label),
                Style::default().fg(theme.text_dim),
            ));
        }
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);

    if let Some(label) = status_label {
        let status = Line::from(vec![
            Span::styled(label, Style::default().fg(theme.text_dim)),
            Span::styled(status_text, Style::default().fg(status_color)),
        ]);
        frame.render_widget(Paragraph::new(status), chunks[1]);
    }
}

// The key column is padded to the widest key so descriptions line up.
const CLIENT_HELP: &str = "q          quit
r          restart
p          pause/resume
s          settings
t          cycle theme
j          print JSON
d          toggle streams
?          toggle help
PgUp/PgDn  page help
Home/End   first/last page
Esc        close";
const SERVER_HELP: &str = "q          quit server
?          toggle help
PgUp/PgDn  page help
Home/End   first/last page
Esc        close";

/// Help rows hard-wrapped to `width`, each split into its (key, description)
/// parts. Help text is ASCII, so byte slicing keeps paging exact even at one
/// column.
fn help_rows(width: u16, server: bool) -> impl Iterator<Item = (&'static str, &'static str)> {
    let text = if server { SERVER_HELP } else { CLIENT_HELP };
    let width = usize::from(width.max(1));
    text.lines().flat_map(move |line| {
        let key_len = line.find("  ").unwrap_or(0);
        (0..line.len()).step_by(width).map(move |start| {
            let end = (start + width).min(line.len());
            let split = key_len.clamp(start, end);
            (&line[start..split], &line[split..end])
        })
    })
}

pub fn draw_help_overlay(frame: &mut Frame, theme: &Theme, area: Rect, scroll: u16, server: bool) {
    let width = area.width.min(40);
    let height = area.height.min(if server { 8 } else { 16 });
    let help_area = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let rows: Vec<_> = help_rows(width.saturating_sub(2), server).collect();
    let max = (rows.len() as u16).saturating_sub(height.saturating_sub(2));
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|(key, desc)| {
            Line::from(vec![
                Span::styled(key, Style::default().fg(theme.accent)),
                Span::styled(desc, Style::default().fg(theme.text)),
            ])
        })
        .collect();
    let mut block = Block::default()
        .title(Span::styled(
            " Help ",
            Style::default()
                .fg(theme.header)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    // Overflowing help says which way the rest of it is.
    if scroll < max {
        block = block.title_bottom(Span::styled(
            " PgDn more ",
            Style::default().fg(theme.accent),
        ));
    } else if max > 0 {
        block = block.title_bottom(Span::styled(
            " PgUp back ",
            Style::default().fg(theme.accent),
        ));
    }
    let help = Paragraph::new(lines).scroll((scroll, 0)).block(block);
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
    let modal_height = 19u16;
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
    // Reserve the controls before giving the remaining rows to the settings.
    // A fixed minimum for content squeezed the tabs and actions out entirely
    // in short terminals, leaving an invisible Apply/Close selection.
    let roomy = inner.height >= 13;
    let tabs_height = if roomy {
        2
    } else {
        u16::from(inner.height >= 3)
    };
    let buttons_height = if roomy {
        2
    } else {
        u16::from(inner.height >= 2)
    };
    let help_height = u16::from(inner.height >= 6);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(tabs_height),
            Constraint::Min(0),
            Constraint::Length(buttons_height),
            Constraint::Length(help_height),
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
        (
            "Update check:",
            match settings.update_check_lock_reason {
                Some(reason) => format!(
                    "{} ({reason})",
                    if settings.update_check { "on" } else { "off" }
                ),
                None => if settings.update_check { "on" } else { "off" }.to_string(),
            },
        ),
    ];

    // A locked update check cannot be changed, so it gets no change arrows.
    let locked_row = settings.update_check_lock_reason.map(|_| 3);
    draw_setting_items(
        frame,
        &items,
        settings.selected_index,
        settings.button_focused,
        locked_row,
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
        (
            "Bitrate:",
            super::settings::bitrate_display(settings.bitrate),
        ),
    ];

    draw_setting_items(
        frame,
        &items,
        settings.selected_index,
        settings.button_focused,
        None,
        theme,
        area,
    );
}

fn draw_setting_items(
    frame: &mut Frame,
    items: &[(&str, String)],
    selected_index: usize,
    button_focused: bool,
    locked_row: Option<usize>,
    theme: &Theme,
    area: Rect,
) {
    let mut lines = Vec::new();
    lines.push(Line::from("")); // Top padding

    for (i, (label, value)) in items.iter().enumerate() {
        let is_selected = !button_focused && i == selected_index;
        let locked = locked_row == Some(i);

        let prefix = if is_selected { "▶ " } else { "  " };
        let label_style = Style::default().fg(theme.text_dim);
        let value_style = if locked {
            label_style
        } else if is_selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };

        // On a narrow terminal, spend the space on the value before adding
        // change arrows (prefix 2 + label column 14 + value + arrows 4). The
        // selection prefix still identifies the active row.
        let value_width = Span::raw(value.as_str()).width();
        let value_display =
            if is_selected && !locked && 2 + 14 + value_width + 4 <= area.width as usize {
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

    // Keep the active row in view instead of changing an off-screen setting.
    let selected_row = selected_index.saturating_add(1); // top padding
    let scroll = selected_row
        .saturating_sub(area.height.saturating_sub(1) as usize)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);
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

    let mut buttons = Vec::new();

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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{buffer::Buffer, widgets::Widget};

    #[test]
    fn loss_marker_survives_zero_and_sub_resolution_throughput() {
        // With the graph's 100 Mbps floor and three rows, a rate below
        // ~4.2 Mbps has no eighth-block to draw. Loss must remain visible
        // at the baseline even when the throughput bar disappears.
        let area = Rect::new(0, 0, 1, 3);
        for name in Theme::list() {
            let theme = Theme::by_name(name);
            for mbps in [0.0, 1.0, 50.0] {
                for rate in [None, Some(0.0), Some(0.5), Some(5.0)] {
                    let mut buf = Buffer::empty(area);
                    Sparkline::new(&[mbps])
                        .max(100.0)
                        .styles(&[loss_severity_style(rate, &theme)])
                        .render(area, &mut buf);

                    assert_eq!(
                        buf[(0, 2)].modifier.contains(Modifier::UNDERLINED),
                        rate.is_some_and(|rate| rate > 0.0),
                        "theme {name}, {mbps} Mbps, loss {rate:?}: the baseline must carry loss"
                    );
                    // A loss marker must not invent throughput at zero or
                    // below the graph's resolution. Monochrome's existing
                    // heavy-loss stipple marks empty cells explicitly.
                    if mbps < 4.0 {
                        let expected = if theme.hue_free() && rate == Some(5.0) {
                            "░"
                        } else {
                            " "
                        };
                        assert_eq!(buf[(0, 2)].symbol(), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn loss_severity_survives_without_hue() {
        // Issue #158: the sparkline was the one display where color was
        // the only channel carrying loss. Severity must also ride
        // modifiers so it survives glare, monochrome, and NO_COLOR.
        let theme = Theme::default_theme();

        let clean = loss_severity_style(Some(0.0), &theme);
        assert!(clean.add_modifier.is_empty());
        assert_eq!(clean.fg, Some(theme.graph_primary));

        let unknown = loss_severity_style(None, &theme);
        assert!(unknown.add_modifier.is_empty());

        let light = loss_severity_style(Some(0.5), &theme);
        assert!(light.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(light.fg, Some(theme.warning));

        // Color themes must NOT get REVERSED (#93 follow-up: reverse video
        // on a partial bar glyph marks the empty fraction of the cell and
        // hides the bar — v0.9.24 rendered heavy loss as red fragments
        // floating at the top of the graph). Hue plus underline carries
        // severity there.
        let heavy = loss_severity_style(Some(5.0), &theme);
        assert!(heavy.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!heavy.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(heavy.fg, Some(theme.error));

        // Monochrome has no hue to lean on: heavy loss becomes the
        // REVERSED full-height pillar (the widget fills the column).
        let mono = Theme::monochrome();
        let heavy_mono = loss_severity_style(Some(5.0), &mono);
        assert!(heavy_mono.add_modifier.contains(Modifier::REVERSED));
        let light_mono = loss_severity_style(Some(0.5), &mono);
        assert!(light_mono.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!light_mono.add_modifier.contains(Modifier::REVERSED));
    }
}

#[cfg(test)]
mod small_terminal_tests {
    use super::*;
    use crate::tui::app::{AppState, ThroughputSample};
    use ratatui::{Terminal, backend::TestBackend};

    // Includes the heavy-loss stipple: it is written by the same loop over the
    // same rect, so it escapes the band on exactly the same bugs the bar does.
    const BAR_GLYPHS: [&str; 9] = ["█", "▇", "▆", "▅", "▄", "▃", "▂", "▁", "░"];

    // The subset the progress bar cannot produce. `█` and `░` are shared with
    // the transfer bar's fill and track, so only these prove a *sparkline*
    // glyph landed somewhere it should not. `running_app_with_history` feeds
    // 42 Mbps against a 100 Mbps floor, so an escaped sparkline is always a
    // partial-height bar — keep that fixture mid-scale or this loses its teeth.
    const SPARKLINE_ONLY_GLYPHS: [&str; 7] = ["▇", "▆", "▅", "▄", "▃", "▂", "▁"];

    fn running_app_with_history() -> App {
        let mut app = App::default();
        app.state = AppState::Running;
        for _ in 0..8 {
            app.throughput_history.push_back(ThroughputSample {
                mbps: 42.0,
                lost_packets: 0,
                interval_packets: 10,
            });
        }
        app
    }

    #[test]
    fn settings_shows_update_check_lock_reason() {
        for width in [50, 80] {
            for reason in [
                "not compiled in",
                "locked: environment",
                "locked: --no-update-check",
                "locked: config.toml",
            ] {
                let mut app = App::default();
                app.settings.visible = true;
                app.settings.selected_index = 3;
                app.settings.update_check = false;
                app.settings.update_check_lock_reason = Some(reason);
                let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
                terminal.draw(|f| draw(f, &app)).unwrap();
                let buf = terminal.backend().buffer();
                let text: String = (0..24)
                    .flat_map(|y| (0..width).map(move |x| buf[(x, y)].symbol()))
                    .collect();
                assert!(
                    text.contains(&format!("off ({reason})")),
                    "width {width}: {reason}"
                );
            }
        }
    }

    /// Right does nothing on a locked update-check row, so it must not
    /// offer the change arrows; it reads dimmed and keeps its reason.
    #[test]
    fn locked_update_check_row_has_no_change_arrows() {
        let theme = Theme::default();
        for locked in [false, true] {
            let mut app = App::default();
            app.settings.visible = true;
            app.settings.selected_index = 3;
            app.settings.update_check = false;
            app.settings.update_check_lock_reason = locked.then_some("locked: --no-update-check");
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| draw(f, &app)).unwrap();
            let buf = terminal.backend().buffer();
            let row = (0..24)
                .map(|y| (y, (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>()))
                .find(|(_, row)| row.contains("Update check:"))
                .unwrap();
            assert!(row.1.contains("▶ Update check:"), "{}", row.1);
            assert_eq!(row.1.contains('◀'), !locked, "{}", row.1);
            if locked {
                assert!(row.1.contains("(locked: --no-update-check)"), "{}", row.1);
                let at = row.1.find("off (").unwrap();
                let x = row.1[..at].chars().count() as u16;
                assert_eq!(buf[(x, row.0)].fg, theme.text_dim);
            }
        }
    }

    fn rows(width: u16, height: u16) -> Vec<String> {
        let app = running_app_with_history();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buf = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn compact_client_keeps_rate_and_progress() {
        let mut app = running_app_with_history();
        app.current_throughput_mbps = 42.0;
        for (width, height) in [(40, 8), (40, 16)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| draw(f, &app)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("42.0 Mbps"), "{text}");
            assert!(
                text.contains(if height < 12 { "0s / 10s" } else { "0s/10s" }),
                "{text}"
            );
            assert!(!text.contains("Configuration"));
        }
    }

    #[test]
    fn compact_client_preserves_byte_budget_and_infinite_targets() {
        let mut app = App::default();
        app.byte_budget = Some(1_000_000);
        app.duration = std::time::Duration::ZERO;
        for (budget, expected) in [(Some(1_000_000), "0 B / 976.56 KB"), (None, "0s / ∞")] {
            app.byte_budget = budget;
            let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
            terminal.draw(|f| draw(f, &app)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains(expected), "{text}");
        }
    }

    #[test]
    fn compact_client_keeps_error_reason_and_final_average() {
        let mut app = App::default();
        app.on_error("Connection refused".into());
        for height in [8, 16] {
            let mut terminal = Terminal::new(TestBackend::new(40, height)).unwrap();
            terminal.draw(|f| draw(f, &app)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("Error: Connection refused"), "{text}");
        }
        app.state = AppState::Completed;
        app.average_throughput_mbps = 42.0;
        app.current_throughput_mbps = 99.0;
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Complete  42.0 Mbps"), "{text}");
        assert!(!text.contains("99.0 Mbps"), "{text}");
    }

    #[test]
    fn help_pages_wrap_and_clamp_after_resize() {
        use crossterm::event::KeyCode;
        use ratatui::layout::Size;
        for server in [false, true] {
            let size = Size::new(18, 5);
            let first = help_scroll(0, size, server, Some(KeyCode::PageDown));
            assert_eq!(first, 3);
            assert_eq!(help_scroll(first, size, server, Some(KeyCode::PageUp)), 0);
            let end = help_scroll(0, size, server, Some(KeyCode::End));
            assert!(end > first);
            let mut terminal = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
            terminal
                .draw(|f| draw_help_overlay(f, &Theme::default(), f.area(), end, server))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("Esc        close"), "{text}");
            assert_eq!(help_scroll(end, Size::new(80, 30), server, None), 0);
            assert_eq!(help_scroll(end, size, server, Some(KeyCode::Home)), 0);
        }
    }

    #[test]
    fn help_scroll_bounds_match_wrapped_text_at_every_width() {
        use crossterm::event::KeyCode;
        use ratatui::layout::Size;
        for server in [false, true] {
            let text = if server { SERVER_HELP } else { CLIENT_HELP };
            for width in 0..=80u16 {
                let content_width = usize::from(width.min(40).saturating_sub(2).max(1));
                let rows: usize = text
                    .lines()
                    .map(|line| line.len().div_ceil(content_width))
                    .sum();
                for height in 0..=24u16 {
                    let visible = height.min(if server { 8 } else { 16 }).saturating_sub(2);
                    assert_eq!(
                        help_scroll(0, Size::new(width, height), server, Some(KeyCode::End)),
                        (rows as u16).saturating_sub(visible),
                        "{width}x{height}, server={server}"
                    );
                }
            }
        }
    }

    #[test]
    fn help_hints_scrolling_only_when_it_overflows() {
        use crossterm::event::KeyCode;
        use ratatui::layout::Size;
        let theme = Theme::default();
        let render = |width, height, scroll, server| {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|f| draw_help_overlay(f, &theme, f.area(), scroll, server))
                .unwrap();
            terminal.backend().buffer().clone()
        };
        let text = |buf: &ratatui::buffer::Buffer| -> String {
            buf.content.iter().map(|c| c.symbol()).collect()
        };
        let end = help_scroll(0, Size::new(40, 10), false, Some(KeyCode::End));
        assert!(end > 0);
        let top = text(&render(40, 10, 0, false));
        assert!(
            top.contains("PgDn more") && !top.contains("PgUp back"),
            "{top}"
        );
        let bottom = text(&render(40, 10, end, false));
        assert!(bottom.contains("PgUp back") && !bottom.contains("PgDn more"));
        for (width, height, server) in [(80, 24, false), (40, 8, true)] {
            let fits = text(&render(width, height, 0, server));
            assert!(!fits.contains("more") && !fits.contains("back"), "{fits}");
        }
        // Keys keep the accent colour; descriptions use body text and start
        // in one column, past the widest key.
        for server in [false, true] {
            let buf = render(80, 24, 0, server);
            let width = usize::from(buf.area.width);
            // Cell index of a needle: every cell here holds one char.
            let cell = |needle: &str| {
                let all = text(&buf);
                all[..all.find(needle).unwrap()].chars().count()
            };
            let column = |needle: &str| {
                let at = cell(needle);
                assert_eq!(buf.content[at].fg, theme.text, "{needle}");
                at % width
            };
            let quit = column("quit");
            for desc in ["toggle help", "page help", "first/last page", "close"] {
                assert_eq!(column(desc), quit, "{desc}, server={server}");
            }
            let q = cell("q   ");
            assert_eq!(buf.content[q].fg, theme.accent);
        }
    }

    #[test]
    fn ultra_small_client_and_help_do_not_panic() {
        let mut app = App::default();
        let mut terminal = Terminal::new(TestBackend::new(1, 1)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        app.show_help = true;
        terminal.draw(|f| draw(f, &app)).unwrap();
    }

    #[test]
    fn bidir_udp_quality_renders_unknown_upload_and_measured_zero_download() {
        let mut app = App::default();
        app.protocol = crate::protocol::Protocol::Udp;
        app.direction = crate::protocol::Direction::Bidir;
        app.udp_recv.progress = Some(crate::protocol::UdpIntervalProgress {
            packets_received: 100,
            packets_lost: 0,
        });
        app.udp_recv.jitter_ms = Some(0.0);
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_realtime_stats(frame, &app, &app.theme, area)
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = (0..10)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        assert!(
            rows.iter()
                .any(|row| row.contains("Upload: loss --% jitter --ms")),
            "{rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("Download: loss 0.0% jitter 0.00ms")),
            "{rows:?}"
        );
    }

    #[test]
    fn default_theme_headline_numbers_use_terminal_palette_colors() {
        // Fixed RGB is tuned for one background; on the default theme the
        // numbers people read must use named colors the terminal remaps.
        let mut app = running_app_with_history();
        app.current_throughput_mbps = 42.0;
        app.elapsed = app.duration / 2;
        assert_eq!(app.theme.name(), "default");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buf = terminal.backend().buffer();
        let rows: Vec<String> = (0..24)
            .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let cell_of = |needle: &str, nth: usize| {
            rows.iter()
                .enumerate()
                .flat_map(|(y, row)| {
                    row.match_indices(needle)
                        .map(move |(i, _)| (row[..i].chars().count() as u16, y as u16))
                })
                .nth(nth)
                .unwrap_or_else(|| panic!("{needle:?} #{nth} not rendered: {rows:#?}"))
        };
        let current = cell_of("Current Speed: ", 0);
        let cells = [
            ("headline", cell_of("42.0 Mbps", 0)),
            ("current speed", (current.0 + 15, current.1)),
            // Last filled cell before the track; the sparkline has no `░`.
            ("transfer bar", cell_of("█░", 0)),
            ("sparkline", cell_of("▂", 0)),
        ];
        for (role, (x, y)) in cells {
            let fg = buf[(x, y)].fg;
            assert_eq!(fg, app.theme.graph_primary, "{role} at ({x},{y})");
            assert!(
                !matches!(fg, Color::Rgb(..) | Color::Indexed(_)),
                "{role}: {fg:?}"
            );
        }
    }

    #[test]
    fn stream_bars_use_the_theme_dim_color() {
        let mut app = App::default();
        app.theme = Theme::dracula();
        app.streams[0].throughput_mbps = 8.0;
        let mut terminal = Terminal::new(TestBackend::new(80, 4)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_streams(frame, &app, &app.theme, area)
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row: Vec<_> = (0..80).map(|x| &buffer[(x, 1)]).collect();
        let label = row.iter().find(|c| c.symbol() == "[").expect("label");
        let track = row.iter().find(|c| c.symbol() == "─").expect("empty track");
        assert_eq!(label.fg, app.theme.text_dim);
        assert_eq!(track.fg, app.theme.text_dim);
    }

    #[test]
    fn stream_panel_renders_upload_and_download_rates() {
        let mut app = App::default();
        app.streams[0].throughput_mbps = 8.0;
        app.streams[0].throughput_send_mbps = Some(3.0);
        app.streams[0].throughput_recv_mbps = Some(5.0);
        let mut terminal = Terminal::new(TestBackend::new(80, 4)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_streams(frame, &app, &app.theme, area)
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row: String = (0..80).map(|x| buffer[(x, 1)].symbol()).collect();
        assert!(row.contains("↑ 3.0 Mbps  ↓ 5.0 Mbps"), "{row}");
    }

    #[test]
    fn compact_settings_keeps_selected_values_and_actions_visible() {
        let categories = [
            (
                SettingsCategory::Display,
                vec![
                    ("Theme:", "tokyo_night"),
                    ("Timestamp:", "iso8601"),
                    ("Units:", "auto"),
                    ("Update check:", "on"),
                ],
            ),
            (
                SettingsCategory::Test,
                vec![
                    ("Streams:", "2"),
                    ("Protocol:", "TCP"),
                    ("Duration:", "10s"),
                    ("Direction:", "Upload"),
                    ("Bitrate:", "100 Mbps"),
                ],
            ),
        ];
        for width in [30, 40, 58] {
            for height in [5, 6, 8, 10, 12, 14, 19] {
                for (category, items) in &categories {
                    for (index, (label, value)) in items.iter().enumerate() {
                        let mut app = App::default();
                        app.settings.visible = true;
                        app.settings.category = *category;
                        app.settings.selected_index = index;
                        app.settings.theme_index = 9; // tokyo_night
                        app.settings.timestamp_format =
                            super::super::settings::TimestampFormat::Iso8601;
                        app.settings.streams = 2;
                        app.settings.bitrate = Some(100_000_000);
                        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                        terminal.draw(|f| draw(f, &app)).unwrap();
                        let buf = terminal.backend().buffer();
                        let rows: Vec<String> = (0..height)
                            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect())
                            .collect();
                        let context = format!(
                            "{width}x{height}, {category:?}, {label}\n{}",
                            rows.join("\n")
                        );
                        let selected = rows
                            .iter()
                            .find(|row| row.contains(label) && row.contains('▶'));
                        assert!(selected.is_some(), "selected setting is hidden: {context}");
                        assert!(
                            selected.unwrap().contains(value),
                            "selected value is clipped: {context}"
                        );
                        assert!(
                            rows.iter().any(|row| row.contains("[Apply & Restart]")),
                            "apply action is hidden: {context}"
                        );
                        assert!(
                            rows.iter().any(|row| row.contains("[Esc]")),
                            "close action is hidden: {context}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn settings_degrades_without_panicking_below_the_compact_layout() {
        let mut app = App::default();
        app.settings.visible = true;
        for (width, height) in [(1, 1), (5, 3), (10, 4), (20, 5)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| draw(f, &app)).unwrap();
        }
    }

    /// Regression: the throughput sparkline was rendered into a hand-built
    /// Rect with a hardcoded `height: 3`. On a terminal too short for the full
    /// layout, ratatui shrinks that band to 0-2 rows, and the fixed height
    /// spilled bars over the Real-time Stats panel's bottom border — visible
    /// on anything under ~16 rows.
    #[test]
    fn sparkline_never_paints_outside_its_band() {
        for height in 8..=20u16 {
            for row in rows(60, height) {
                // The bug was "writes outside the allotted chunk", not "writes
                // on a border" — the border was just where it showed up first.
                // A spill one row shorter lands on the Transfer line instead,
                // which is inside the same panel and would pass a
                // borders-only check.
                let is_border_row = row.starts_with('\u{2514}') || row.starts_with('\u{250c}');
                let is_transfer_row = row.contains("Transfer:");
                if !is_border_row && !is_transfer_row {
                    continue;
                }
                // The transfer line draws its own progress bar out of `█` and
                // `░`, so those two prove nothing there; a border row has no
                // business carrying any of them.
                let glyphs: &[&str] = if is_border_row {
                    &BAR_GLYPHS
                } else {
                    &SPARKLINE_ONLY_GLYPHS
                };
                for glyph in glyphs {
                    assert!(
                        !row.contains(glyph),
                        "height {height}: sparkline glyph {glyph:?} escaped its band onto {row:?}"
                    );
                }
            }
        }
    }

    /// Regression: the footer laid its key hints and Status out as a single
    /// line running ~107 columns. On the 80-column default (macOS Terminal,
    /// tmux splits, the Linux console) ratatui clipped the right edge, and
    /// the casualty was Status — the one field down there that changes.
    #[test]
    fn footer_keeps_status_when_the_hints_do_not_fit() {
        // 94 is the boundary: the full hint list still fits beside Status
        // there, so it only has to prove Status survives.
        for width in [60, 80, 94] {
            let frame = rows(width, 24);
            let footer = frame.last().expect("footer row").clone();
            assert!(
                footer.contains("Status:"),
                "width {width}: footer lost its Status label: {footer:?}"
            );
            assert!(
                footer.contains("Running"),
                "width {width}: footer lost the status value: {footer:?}"
            );
        }

        // Below that, the hints are what absorb the loss, not Status.
        for width in [60, 80] {
            let frame = rows(width, 24);
            let footer = frame.last().expect("footer row").clone();
            assert!(
                footer.contains("q:Quit"),
                "width {width}: expected compact hints: {footer:?}"
            );
        }
    }

    /// ...and a wide terminal still gets the roomy labels.
    #[test]
    fn footer_uses_full_hints_when_there_is_room() {
        let frame = rows(140, 24);
        let footer = frame.last().expect("footer row");
        assert!(
            footer.contains("[q] Quit"),
            "expected full hints: {footer:?}"
        );
        assert!(footer.contains("Status:"), "footer lost Status: {footer:?}");
    }

    /// Hints drop from the right, so help leads and survives narrow widths.
    #[test]
    fn footer_keeps_help_hint_at_narrow_widths() {
        for (width, height) in [(40, 24), (40, 10), (60, 24), (80, 24)] {
            let frame = rows(width, height);
            let footer = frame.last().expect("footer row");
            assert!(footer.contains("?:Help"), "{width}x{height}: {footer:?}");
            assert!(footer.contains("Status: Running"), "{footer:?}");
        }
    }

    /// At 20x5 the footer kept "Status: Running..." and lost the help hint,
    /// and the compact headline cut the rate to "4.00 Gbp".
    #[test]
    fn tiny_client_keeps_help_hint_and_whole_rate() {
        let mut app = running_app_with_history();
        app.current_throughput_mbps = 4000.0;
        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buf = terminal.backend().buffer();
        let rows: Vec<String> = (0..5)
            .map(|y| (0..20).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        assert!(rows[1].starts_with("Running  4.00 Gbps"), "{rows:?}");
        assert!(rows[4].starts_with("?:Help"), "{rows:?}");
        assert!(rows[4].trim_end().ends_with(" Running..."), "{rows:?}");
        // Narrower still, the state label goes before the rate is cut.
        let mut terminal = Terminal::new(TestBackend::new(12, 5)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buf = terminal.backend().buffer();
        let headline: String = (0..12).map(|x| buf[(x, 1)].symbol()).collect();
        assert_eq!(headline.trim_end(), "4.00 Gbps");
        let footer: String = (0..12).map(|x| buf[(x, 4)].symbol()).collect();
        assert_eq!(footer.trim_end(), "?:Help");
    }

    /// The compact view already says "Paused"; the pause box would hide its rate.
    #[test]
    fn compact_pause_keeps_the_rate_visible() {
        let mut app = running_app_with_history();
        app.state = AppState::Paused;
        app.current_throughput_mbps = 94.0;
        for (height, boxed) in [(8, false), (12, true)] {
            let mut terminal = Terminal::new(TestBackend::new(40, height)).unwrap();
            terminal.draw(|f| draw(f, &app)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert_eq!(text.contains("PAUSED"), boxed, "{text}");
            if !boxed {
                assert!(text.contains("Paused  94.0 Mbps"), "{text}");
            }
        }
    }

    /// Tiny terminals must degrade, not panic.
    #[test]
    fn renders_without_panicking_at_tiny_sizes() {
        for (w, h) in [(1, 1), (5, 3), (10, 5), (20, 8), (40, 12), (80, 24)] {
            let _ = rows(w, h);
        }
    }
}

/// Keep help paging aligned with the overlay's actual inner height.
pub fn help_scroll(
    scroll: u16,
    size: ratatui::layout::Size,
    server: bool,
    key: Option<crossterm::event::KeyCode>,
) -> u16 {
    use crossterm::event::KeyCode;
    let visible = size
        .height
        .min(if server { 8 } else { 16 })
        .saturating_sub(2);
    let max = (help_rows(size.width.min(40).saturating_sub(2), server).count() as u16)
        .saturating_sub(visible);
    let scroll = scroll.min(max);
    match key {
        Some(KeyCode::PageDown) => scroll.saturating_add(visible.max(1)).min(max),
        Some(KeyCode::PageUp) => scroll.saturating_sub(visible.max(1)),
        Some(KeyCode::Home) => 0,
        Some(KeyCode::End) => max,
        _ => scroll,
    }
}
