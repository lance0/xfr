//! Update notification system
//!
//! Checks GitHub releases for newer versions and notifies the user in the TUI.

use std::time::Duration;
use update_informer::{Check, registry::GitHub};

/// How the user installed xfr - used to show the appropriate update command
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Homebrew,
    Cargo,
    Binary,
}

impl InstallMethod {
    /// Detect installation method based on executable path
    pub fn detect() -> Self {
        let exe_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok());

        if let Some(path) = exe_path {
            let path_str = path.to_string_lossy().to_lowercase();
            if path_str.contains("homebrew") || path_str.contains("cellar") {
                return Self::Homebrew;
            }
            if path_str.contains(".cargo/bin") {
                return Self::Cargo;
            }
        }
        Self::Binary
    }

    /// Get the command to update xfr
    pub fn update_command(&self) -> &'static str {
        match self {
            Self::Homebrew => "brew upgrade xfr",
            Self::Cargo => "cargo install xfr",
            Self::Binary => "github.com/lance0/xfr/releases",
        }
    }
}

/// Check GitHub for a newer version of xfr
///
/// Returns Some(version) if a newer version is available, None otherwise.
/// Uses a 1-hour cache to avoid excessive GitHub API calls.
pub fn check_for_update() -> Option<String> {
    // Cache for 1 hour to avoid GitHub rate limits
    let informer = update_informer::new(GitHub, "lance0/xfr", env!("CARGO_PKG_VERSION"))
        .interval(Duration::from_secs(3600));

    informer
        .check_version()
        .ok()
        .flatten()
        .map(|v| v.to_string())
}
