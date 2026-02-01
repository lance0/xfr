//! Custom TUI widgets

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

const SPARKLINE_CHARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

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

        // Take the last `width` values
        let start = data_len.saturating_sub(width);
        let visible_data = &self.data[start..];

        for (i, &value) in visible_data.iter().enumerate() {
            let normalized = (value / max).clamp(0.0, 1.0);
            let char_index = ((normalized * 7.0) as usize).min(7);
            let ch = SPARKLINE_CHARS[char_index];

            let x = area.x + i as u16;
            let y = area.y;

            if x < area.x + area.width {
                buf[(x, y)].set_char(ch).set_style(self.style);
            }
        }
    }
}

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
            let ch = if x < filled_width { '█' } else { '░' };
            let style = if x < filled_width {
                self.filled_style
            } else {
                self.style
            };
            buf[(area.x + x, area.y)].set_char(ch).set_style(style);
        }
    }
}

pub struct StreamBar {
    pub stream_id: u8,
    pub throughput_mbps: f64,
    pub max_throughput: f64,
    pub retransmits: u64,
}

impl StreamBar {
    pub fn new(stream_id: u8, throughput_mbps: f64, max_throughput: f64, retransmits: u64) -> Self {
        Self {
            stream_id,
            throughput_mbps,
            max_throughput,
            retransmits,
        }
    }
}

impl Widget for StreamBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 20 || area.height == 0 {
            return;
        }

        // Format: [0] ████████████░░░░  236 Mbps  rtx: 2
        let label = format!("[{}] ", self.stream_id);
        let stats = format!(
            " {:.0} Mbps  rtx: {}",
            self.throughput_mbps, self.retransmits
        );

        let label_width = label.len() as u16;
        let stats_width = stats.len() as u16;
        let bar_width = area.width.saturating_sub(label_width + stats_width);

        // Render label
        buf.set_string(area.x, area.y, &label, Style::default().fg(Color::Cyan));

        // Render bar
        let progress = if self.max_throughput > 0.0 {
            self.throughput_mbps / self.max_throughput
        } else {
            0.0
        };
        let filled = (progress * bar_width as f64) as u16;

        for x in 0..bar_width {
            let ch = if x < filled { '█' } else { '░' };
            let style = if x < filled {
                Style::default().fg(Color::Green)
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
            Style::default().fg(Color::Yellow),
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
