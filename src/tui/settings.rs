//! Settings modal for TUI

use std::time::Duration;

use crate::protocol::{Direction, Protocol};

use super::theme::Theme;

/// Settings category tabs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsCategory {
    #[default]
    Display,
    Test,
}

impl SettingsCategory {
    pub fn next(&self) -> Self {
        match self {
            Self::Display => Self::Test,
            Self::Test => Self::Display,
        }
    }
}

/// Timestamp format for display
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimestampFormat {
    #[default]
    Relative,
    Iso8601,
    Unix,
}

impl TimestampFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Relative => "relative",
            Self::Iso8601 => "iso8601",
            Self::Unix => "unix",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Relative => Self::Iso8601,
            Self::Iso8601 => Self::Unix,
            Self::Unix => Self::Relative,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::Relative => Self::Unix,
            Self::Iso8601 => Self::Relative,
            Self::Unix => Self::Iso8601,
        }
    }
}

/// Display units preference
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Units {
    #[default]
    Auto,
    Bits,
    Bytes,
}

impl Units {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Bits => "bits",
            Self::Bytes => "bytes",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Auto => Self::Bits,
            Self::Bits => Self::Bytes,
            Self::Bytes => Self::Auto,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::Auto => Self::Bytes,
            Self::Bits => Self::Auto,
            Self::Bytes => Self::Bits,
        }
    }
}

/// State for the settings modal
#[derive(Debug, Clone)]
pub struct SettingsState {
    /// Whether the modal is visible
    pub visible: bool,
    /// Current category tab
    pub category: SettingsCategory,
    /// Selected item index within category
    pub selected_index: usize,
    /// Whether a button is focused (vs a setting)
    pub button_focused: bool,
    /// Which button is selected (0 = Apply & Restart, 1 = Close)
    pub button_index: usize,

    // Display settings (live-editable, auto-persist)
    pub theme_index: usize,
    pub timestamp_format: TimestampFormat,
    pub units: Units,

    // Test settings (require restart)
    pub streams: u8,
    pub protocol: Protocol,
    pub duration_secs: u64,
    pub direction: Direction,

    // Original values to detect changes
    original_streams: u8,
    original_protocol: Protocol,
    original_duration_secs: u64,
    original_direction: Direction,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            visible: false,
            category: SettingsCategory::Display,
            selected_index: 0,
            button_focused: false,
            button_index: 0,

            theme_index: 0,
            timestamp_format: TimestampFormat::Relative,
            units: Units::Auto,

            streams: 1,
            protocol: Protocol::Tcp,
            duration_secs: 10,
            direction: Direction::Upload,

            original_streams: 1,
            original_protocol: Protocol::Tcp,
            original_duration_secs: 10,
            original_direction: Direction::Upload,
        }
    }
}

impl SettingsState {
    /// Create settings state from current app state
    pub fn new(
        theme_index: usize,
        streams: u8,
        protocol: Protocol,
        duration: Duration,
        direction: Direction,
    ) -> Self {
        let duration_secs = duration.as_secs();
        Self {
            visible: false,
            category: SettingsCategory::Display,
            selected_index: 0,
            button_focused: false,
            button_index: 0,

            theme_index,
            timestamp_format: TimestampFormat::Relative,
            units: Units::Auto,

            streams,
            protocol,
            duration_secs,
            direction,

            original_streams: streams,
            original_protocol: protocol,
            original_duration_secs: duration_secs,
            original_direction: direction,
        }
    }

    /// Toggle visibility
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            // Reset to first item when opening
            self.selected_index = 0;
            self.button_focused = false;
        }
    }

    /// Close the modal
    pub fn close(&mut self) {
        self.visible = false;
    }

    /// Check if test parameters have changed
    pub fn test_params_dirty(&self) -> bool {
        self.streams != self.original_streams
            || self.protocol != self.original_protocol
            || self.duration_secs != self.original_duration_secs
            || self.direction != self.original_direction
    }

    /// Number of items in current category
    fn items_in_category(&self) -> usize {
        match self.category {
            SettingsCategory::Display => 3, // Theme, Timestamp, Units
            SettingsCategory::Test => 4,    // Streams, Protocol, Duration, Direction
        }
    }

    /// Move selection up (or to buttons if at top)
    pub fn move_up(&mut self) {
        if self.button_focused {
            // Move from buttons to last item
            self.button_focused = false;
            self.selected_index = self.items_in_category().saturating_sub(1);
        } else if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    /// Move selection down (or to buttons if at bottom)
    pub fn move_down(&mut self) {
        if self.button_focused {
            // Stay on buttons
            return;
        }

        if self.selected_index < self.items_in_category().saturating_sub(1) {
            self.selected_index += 1;
        } else {
            // Move to buttons
            self.button_focused = true;
            // Select first visible button (Apply if dirty, else Close)
            self.button_index = if self.test_params_dirty() { 0 } else { 1 };
        }
    }

    /// Switch category tab
    pub fn switch_tab(&mut self) {
        self.category = self.category.next();
        self.selected_index = 0;
        self.button_focused = false;
    }

    /// Cycle selected value to next
    pub fn value_next(&mut self) {
        if self.button_focused {
            // Only cycle if both buttons are visible
            if self.test_params_dirty() {
                self.button_index = (self.button_index + 1) % 2;
            }
            return;
        }

        match self.category {
            SettingsCategory::Display => match self.selected_index {
                0 => {
                    // Theme
                    let theme_list = Theme::list();
                    self.theme_index = (self.theme_index + 1) % theme_list.len();
                }
                1 => self.timestamp_format = self.timestamp_format.next(),
                2 => self.units = self.units.next(),
                _ => {}
            },
            SettingsCategory::Test => match self.selected_index {
                0 => self.streams = self.streams.saturating_add(1).min(128),
                1 => self.protocol = self.protocol.next(),
                2 => self.duration_secs = self.duration_secs.saturating_add(5).min(3600),
                3 => self.direction = self.direction.next(),
                _ => {}
            },
        }
    }

    /// Cycle selected value to previous
    pub fn value_prev(&mut self) {
        if self.button_focused {
            // Only cycle if both buttons are visible
            if self.test_params_dirty() {
                self.button_index = if self.button_index == 0 { 1 } else { 0 };
            }
            return;
        }

        match self.category {
            SettingsCategory::Display => match self.selected_index {
                0 => {
                    // Theme
                    let theme_list = Theme::list();
                    self.theme_index = if self.theme_index == 0 {
                        theme_list.len() - 1
                    } else {
                        self.theme_index - 1
                    };
                }
                1 => self.timestamp_format = self.timestamp_format.prev(),
                2 => self.units = self.units.prev(),
                _ => {}
            },
            SettingsCategory::Test => match self.selected_index {
                0 => self.streams = self.streams.saturating_sub(1).max(1),
                1 => self.protocol = self.protocol.prev(),
                2 => self.duration_secs = self.duration_secs.saturating_sub(5).max(1),
                3 => self.direction = self.direction.prev(),
                _ => {}
            },
        }
    }

    /// Get current theme name
    pub fn theme_name(&self) -> &str {
        let list = Theme::list();
        list.get(self.theme_index).copied().unwrap_or("default")
    }

    /// Handle Enter key - returns true if should restart test
    pub fn on_enter(&mut self) -> SettingsAction {
        if !self.button_focused {
            // Move to buttons when pressing enter on a setting
            self.button_focused = true;
            // Select first visible button (Apply if dirty, else Close)
            self.button_index = if self.test_params_dirty() { 0 } else { 1 };
            return SettingsAction::None;
        }

        match self.button_index {
            0 => {
                // Apply & Restart
                if self.test_params_dirty() {
                    self.close();
                    SettingsAction::Restart
                } else {
                    self.close();
                    SettingsAction::Close
                }
            }
            1 => {
                // Close
                self.close();
                SettingsAction::Close
            }
            _ => SettingsAction::None,
        }
    }
}

/// Action to take after settings interaction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsAction {
    None,
    Close,
    Restart,
}

/// Result of TUI loop - either exit or restart with new params
#[derive(Debug, Clone)]
pub enum TuiLoopResult {
    Exit {
        result: Option<crate::protocol::TestResult>,
        prefs: crate::prefs::Prefs,
        print_json: bool,
    },
    Restart {
        streams: u8,
        protocol: Protocol,
        duration: Duration,
        direction: Direction,
        prefs: crate::prefs::Prefs,
    },
}

// Add helper methods to Protocol and Direction for cycling
impl Protocol {
    fn next(&self) -> Self {
        match self {
            Self::Tcp => Self::Udp,
            Self::Udp => Self::Quic,
            Self::Quic => Self::Tcp,
        }
    }

    fn prev(&self) -> Self {
        match self {
            Self::Tcp => Self::Quic,
            Self::Udp => Self::Tcp,
            Self::Quic => Self::Udp,
        }
    }
}

impl Direction {
    fn next(&self) -> Self {
        match self {
            Self::Upload => Self::Download,
            Self::Download => Self::Bidir,
            Self::Bidir => Self::Upload,
        }
    }

    fn prev(&self) -> Self {
        match self {
            Self::Upload => Self::Bidir,
            Self::Download => Self::Upload,
            Self::Bidir => Self::Download,
        }
    }
}
