//! Update notification system
//!
//! Checks GitHub releases for newer versions and notifies the user in the TUI.

#[cfg(feature = "update-check")]
use std::time::Duration;
#[cfg(feature = "update-check")]
use update_informer::{Check, registry::GitHub};

/// Whether update checking was compiled in (the `update-check` feature, on by
/// default). Package builds can drop it with `--no-default-features`.
pub const ENABLED: bool = cfg!(feature = "update-check");

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
#[cfg(feature = "update-check")]
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

/// Update checking compiled out (`--no-default-features`): always "no update".
#[cfg(not(feature = "update-check"))]
pub fn check_for_update() -> Option<String> {
    None
}

/// True when the user opted out of the update check via environment:
/// `DO_NOT_TRACK` (the cross-tool standard, <https://consoledonottrack.com>)
/// or `XFR_NO_UPDATE_CHECK`.
pub fn env_opt_out() -> bool {
    ["DO_NOT_TRACK", "XFR_NO_UPDATE_CHECK"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| flag_is_truthy(&v.to_string_lossy())))
}

/// An environment flag counts as "set" unless it is empty, `0`, or `false`.
fn flag_is_truthy(v: &str) -> bool {
    !matches!(v.trim(), "" | "0" | "false")
}

#[cfg(test)]
mod tests {
    use super::flag_is_truthy;

    #[test]
    fn env_flag_truthiness() {
        for on in ["1", "true", "yes", "on"] {
            assert!(flag_is_truthy(on), "{on:?} should count as opt-out");
        }
        for off in ["", "  ", "0", "false"] {
            assert!(!flag_is_truthy(off), "{off:?} should not opt out");
        }
    }
}
