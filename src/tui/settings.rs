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
    /// Target bitrate in bits/sec (`None` = unlimited). Primarily for UDP.
    pub bitrate: Option<u64>,

    // Original values to detect changes
    original_streams: u8,
    original_protocol: Protocol,
    original_duration_secs: u64,
    original_direction: Direction,
    original_bitrate: Option<u64>,
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
            bitrate: None,

            original_streams: 1,
            original_protocol: Protocol::Tcp,
            original_duration_secs: 10,
            original_direction: Direction::Upload,
            original_bitrate: None,
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
        bitrate: Option<u64>,
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
            bitrate,

            original_streams: streams,
            original_protocol: protocol,
            original_duration_secs: duration_secs,
            original_direction: direction,
            original_bitrate: bitrate,
        }
    }

    /// Commit the current test parameters as the new baseline so that
    /// [`test_params_dirty`](Self::test_params_dirty) returns false until the
    /// user changes something again. Call this right after a settings-driven
    /// restart has applied the new values.
    pub fn mark_applied(&mut self) {
        self.original_streams = self.streams;
        self.original_protocol = self.protocol;
        self.original_duration_secs = self.duration_secs;
        self.original_direction = self.direction;
        self.original_bitrate = self.bitrate;
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
            || self.bitrate != self.original_bitrate
    }

    /// Number of items in current category
    fn items_in_category(&self) -> usize {
        match self.category {
            SettingsCategory::Display => 3, // Theme, Timestamp, Units
            SettingsCategory::Test => 5,    // Streams, Protocol, Duration, Direction, Bitrate
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
                2 => self.duration_secs = duration_next(self.duration_secs),
                3 => self.direction = self.direction.next(),
                4 => self.bitrate = bitrate_next(self.bitrate),
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
                2 => self.duration_secs = duration_prev(self.duration_secs),
                3 => self.direction = self.direction.prev(),
                4 => self.bitrate = bitrate_prev(self.bitrate),
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

/// Step duration up by 5s, snapping to a clean multiple of 5 (min 5s, max 3600s).
fn duration_next(secs: u64) -> u64 {
    let next = (secs / 5 + 1) * 5;
    next.min(3600)
}

/// Step duration down by 5s, snapping to a clean multiple of 5 (min 5s).
fn duration_prev(secs: u64) -> u64 {
    // Round down to the previous multiple of 5 below the current value.
    let prev = secs.saturating_sub(1) / 5 * 5;
    prev.max(5)
}

/// Result of TUI loop - either exit or restart with new params
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // Exit carries the full TestResult; Restart is rare.
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
        bitrate: Option<u64>,
        prefs: crate::prefs::Prefs,
    },
}

/// Discrete bitrate steps cycled in the settings modal (bits/sec). `None`
/// means unlimited (no rate cap).
const BITRATE_STEPS: [Option<u64>; 6] = [
    None,
    Some(1_000_000),
    Some(10_000_000),
    Some(100_000_000),
    Some(1_000_000_000),
    Some(10_000_000_000),
];

fn bitrate_index(current: Option<u64>) -> usize {
    BITRATE_STEPS
        .iter()
        .position(|&b| b == current)
        .unwrap_or(0)
}

/// Cycle to the next (higher) bitrate step, wrapping around to unlimited.
pub fn bitrate_next(current: Option<u64>) -> Option<u64> {
    let i = bitrate_index(current);
    BITRATE_STEPS[(i + 1) % BITRATE_STEPS.len()]
}

/// Cycle to the previous (lower) bitrate step, wrapping around to the top.
pub fn bitrate_prev(current: Option<u64>) -> Option<u64> {
    let i = bitrate_index(current);
    let prev = if i == 0 {
        BITRATE_STEPS.len() - 1
    } else {
        i - 1
    };
    BITRATE_STEPS[prev]
}

/// Human-readable label for a bitrate value used in the settings modal.
pub fn bitrate_display(bitrate: Option<u64>) -> String {
    match bitrate {
        None => "unlimited".to_string(),
        Some(bps) if bps % 1_000_000_000 == 0 => format!("{} Gbps", bps / 1_000_000_000),
        Some(bps) if bps % 1_000_000 == 0 => format!("{} Mbps", bps / 1_000_000),
        Some(bps) if bps % 1_000 == 0 => format!("{} Kbps", bps / 1_000),
        Some(bps) => format!("{} bps", bps),
    }
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
