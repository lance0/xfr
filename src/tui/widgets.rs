//! Custom TUI widgets

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use crate::stats::mbps_to_human;

// Use 8-level block characters for sparklines
const SPARKLINE_CHARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// A sparkline widget that can render multiple rows for taller graphs
pub struct Sparkline<'a> {
    data: &'a [f64],
    max: Option<f64>,
    style: Style,
}

impl<'a> Sparkline<'a> {
    pub fn new(data: &'a [f64]) -> Self {
        Self {
            data,
            max: None,
            style: Style::default().fg(Color::Green),
        }
    }

    pub fn max(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
}

impl Widget for Sparkline<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if self.data.is_empty() || area.width == 0 || area.height == 0 {
            return;
        }

        let max = self
            .max
            .unwrap_or_else(|| self.data.iter().cloned().fold(0.0f64, f64::max).max(1.0));

        let data_len = self.data.len();
        let width = area.width as usize;
        let height = area.height as usize;

        // Take the last `width` values
        let start = data_len.saturating_sub(width);
        let visible_data = &self.data[start..];

        // For multi-row sparklines, we divide the value range across rows
        // Bottom row shows lowest portion, top row shows highest
        for (i, &value) in visible_data.iter().enumerate() {
            let normalized = (value / max).clamp(0.0, 1.0);

            // Calculate how many "eighth-blocks" this value represents across all rows
            let total_eighths = (normalized * (height * 8) as f64) as usize;

            let x = area.x + i as u16;

            // Render from bottom to top
            for row in 0..height {
                let y = area.y + (height - 1 - row) as u16;
                let eighths_for_row = total_eighths.saturating_sub(row * 8).min(8);

                if eighths_for_row > 0 {
                    let ch = SPARKLINE_CHARS[eighths_for_row - 1];
                    buf[(x, y)].set_char(ch).set_style(self.style);
                }
            }
        }
    }
}

/// A simple progress bar using block characters
pub struct ProgressBar {
    pub progress: f64, // 0.0 to 1.0
    pub style: Style,
    pub filled_style: Style,
}

impl ProgressBar {
    pub fn new(progress: f64) -> Self {
        Self {
            progress: progress.clamp(0.0, 1.0),
            style: Style::default().fg(Color::DarkGray),
            filled_style: Style::default().fg(Color::Green),
        }
    }

    #[allow(dead_code)]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn filled_style(mut self, style: Style) -> Self {
        self.filled_style = style;
        self
    }
}

impl Widget for ProgressBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let filled_width = (self.progress * area.width as f64) as u16;

        for x in 0..area.width {
            let ch = if x < filled_width { '━' } else { '─' };
            let style = if x < filled_width {
                self.filled_style
            } else {
                self.style
            };
            buf[(area.x + x, area.y)].set_char(ch).set_style(style);
        }
    }
}

/// A bar showing per-stream throughput with retransmit count
pub struct StreamBar {
    pub stream_id: u8,
    pub throughput_mbps: f64,
    pub max_throughput: f64,
    pub retransmits: u64,
    pub bar_color: Color,
    pub text_color: Color,
}

impl StreamBar {
    pub fn new(stream_id: u8, throughput_mbps: f64, max_throughput: f64, retransmits: u64) -> Self {
        Self {
            stream_id,
            throughput_mbps,
            max_throughput,
            retransmits,
            bar_color: Color::Green,
            text_color: Color::White,
        }
    }

    pub fn bar_color(mut self, color: Color) -> Self {
        self.bar_color = color;
        self
    }

    pub fn text_color(mut self, color: Color) -> Self {
        self.text_color = color;
        self
    }
}

impl Widget for StreamBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 20 || area.height == 0 {
            return;
        }

        // Format: [0] ████████████────  35.2 Gbps  rtx: 0
        let label = format!("[{}] ", self.stream_id);
        let throughput_str = mbps_to_human(self.throughput_mbps);
        let stats = if self.retransmits > 0 {
            format!(" {}  rtx: {}", throughput_str, self.retransmits)
        } else {
            format!(" {}", throughput_str)
        };

        let label_width = label.len() as u16;
        let stats_width = stats.len() as u16;
        let bar_width = area.width.saturating_sub(label_width + stats_width);

        // Render label
        buf.set_string(area.x, area.y, &label, Style::default().fg(Color::DarkGray));

        // Render bar using line characters for cleaner look
        let progress = if self.max_throughput > 0.0 {
            (self.throughput_mbps / self.max_throughput).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let filled = (progress * bar_width as f64) as u16;

        for x in 0..bar_width {
            let ch = if x < filled { '━' } else { '─' };
            let style = if x < filled {
                Style::default().fg(self.bar_color)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            buf[(area.x + label_width + x, area.y)]
                .set_char(ch)
                .set_style(style);
        }

        // Render stats
        buf.set_string(
            area.x + label_width + bar_width,
            area.y,
            &stats,
            Style::default().fg(self.text_color),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sparkline_chars() {
        assert_eq!(SPARKLINE_CHARS.len(), 8);
    }

    #[test]
    fn test_progress_bar_clamp() {
        let bar = ProgressBar::new(1.5);
        assert_eq!(bar.progress, 1.0);

        let bar = ProgressBar::new(-0.5);
        assert_eq!(bar.progress, 0.0);
    }
}
