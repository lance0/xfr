//! User preferences persistence.
//!
//! Saves runtime preferences (like last used theme) to ~/.config/xfr/prefs.toml
//! Separate from config.toml which is for explicit user configuration.

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
}

impl Prefs {
    /// Get prefs file path: ~/.config/xfr/prefs.toml
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

    /// Get effective theme name
    pub fn theme_name(&self) -> &str {
        self.theme.as_deref().unwrap_or("default")
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
    fn test_theme_name() {
        let prefs = Prefs::default();
        assert_eq!(prefs.theme_name(), "default");

        let prefs = Prefs {
            theme: Some("dracula".to_string()),
            ..Default::default()
        };
        assert_eq!(prefs.theme_name(), "dracula");
    }
}
