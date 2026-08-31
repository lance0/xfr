//! User preferences persistence.
//!
//! Saves runtime preferences (like the last used theme) as `xfr/prefs.toml`
//! under the platform configuration directory returned by
//! `dirs::config_dir()`. This is separate from `config.toml`, which is for
//! explicit user configuration.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// User preferences (auto-saved state)
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Prefs {
    /// Last used theme name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,

    /// Last used server host
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_server: Option<String>,

    /// Preferred number of streams
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streams: Option<u8>,

    /// Show help overlay on first run
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_help_on_start: Option<bool>,

    /// User disabled the update check via the TUI settings toggle.
    /// `None` = never toggled (follows the default / config).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_update_check: Option<bool>,
}

impl Prefs {
    /// Get the preferences path under the platform configuration directory.
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("xfr").join("prefs.toml"))
    }

    /// Load preferences from disk (returns default if missing/invalid)
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Save preferences to disk (best effort)
    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(path) = Self::path() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, toml::to_string_pretty(self)?)?;
        }
        Ok(())
    }

    /// Merge CLI/config overrides into prefs
    /// CLI values take precedence, then config, then saved prefs
    pub fn with_overrides(mut self, cli_theme: Option<&str>, config_theme: Option<&str>) -> Self {
        // Theme priority: CLI > config > saved
        if let Some(t) = cli_theme.filter(|t| *t != "default") {
            self.theme = Some(t.to_string());
        } else if let Some(t) = config_theme {
            self.theme = Some(t.to_string());
        }
        // If neither CLI nor config specified, keep saved pref
        self
    }

    /// Get effective theme name.
    ///
    /// With no explicit choice anywhere (CLI, config, saved pref), a set
    /// `NO_COLOR` environment variable (<https://no-color.org>) selects the
    /// monochrome theme: crossterm already strips the color escapes under
    /// NO_COLOR, so without this the RGB-palette themes degrade by
    /// accident instead of by design. An explicit theme choice outranks
    /// NO_COLOR — the convention governs *default* output, and an explicit
    /// flag or saved pref is an explicit request. Because `self.theme`
    /// stays `None`, the env-induced choice is never persisted to
    /// prefs.toml.
    pub fn theme_name(&self) -> &str {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        self.effective_theme_name(no_color)
    }

    fn effective_theme_name(&self, no_color: bool) -> &str {
        match self.theme.as_deref() {
            Some(t) => t,
            None if no_color => "monochrome",
            None => "default",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefs_default() {
        let prefs = Prefs::default();
        assert!(prefs.theme.is_none());
        assert!(prefs.last_server.is_none());
        assert!(prefs.streams.is_none());
    }

    #[test]
    fn test_prefs_serialization() {
        let prefs = Prefs {
            theme: Some("dracula".to_string()),
            last_server: Some("192.168.1.1".to_string()),
            streams: Some(4),
            show_help_on_start: Some(false),
            disable_update_check: None,
        };
        let toml_str = toml::to_string_pretty(&prefs).unwrap();
        assert!(toml_str.contains("theme = \"dracula\""));
        assert!(toml_str.contains("last_server"));
        assert!(toml_str.contains("streams = 4"));

        let loaded: Prefs = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.theme, Some("dracula".to_string()));
        assert_eq!(loaded.streams, Some(4));
    }

    #[test]
    fn test_prefs_skip_none_fields() {
        let prefs = Prefs {
            theme: Some("default".to_string()),
            last_server: None,
            streams: None,
            show_help_on_start: None,
            disable_update_check: None,
        };
        let toml_str = toml::to_string_pretty(&prefs).unwrap();
        assert!(toml_str.contains("theme"));
        assert!(!toml_str.contains("last_server"));
        assert!(!toml_str.contains("streams"));
    }

    #[test]
    fn test_with_overrides_cli_wins() {
        let prefs = Prefs {
            theme: Some("saved".to_string()),
            ..Default::default()
        };
        let result = prefs.with_overrides(Some("cli"), Some("config"));
        assert_eq!(result.theme, Some("cli".to_string()));
    }

    #[test]
    fn test_with_overrides_config_wins_over_saved() {
        let prefs = Prefs {
            theme: Some("saved".to_string()),
            ..Default::default()
        };
        let result = prefs.with_overrides(Some("default"), Some("config"));
        assert_eq!(result.theme, Some("config".to_string()));
    }

    #[test]
    fn test_with_overrides_keeps_saved() {
        let prefs = Prefs {
            theme: Some("saved".to_string()),
            ..Default::default()
        };
        let result = prefs.with_overrides(Some("default"), None);
        assert_eq!(result.theme, Some("saved".to_string()));
    }

    #[test]
    fn explicit_theme_name_ignores_environment() {
        let prefs = Prefs {
            theme: Some("dracula".to_string()),
            ..Default::default()
        };
        assert_eq!(prefs.theme_name(), "dracula");
    }

    #[test]
    fn no_color_defaults_to_monochrome_but_never_outranks_a_choice() {
        // (Tested via the injectable inner fn — the env var itself is
        // process-global and would race parallel tests.)
        let prefs = Prefs::default();
        assert_eq!(prefs.effective_theme_name(true), "monochrome");
        assert_eq!(prefs.effective_theme_name(false), "default");

        // An explicit choice (CLI/config/saved pref all land in `theme`)
        // outranks NO_COLOR: the convention governs default output only.
        let prefs = Prefs {
            theme: Some("dracula".to_string()),
            ..Default::default()
        };
        assert_eq!(prefs.effective_theme_name(true), "dracula");
    }
}
