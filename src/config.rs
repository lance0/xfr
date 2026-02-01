//! Configuration file support
//!
//! Loads configuration from ~/.config/xfr/config.toml

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::protocol::TimestampFormat;

/// Root configuration structure
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub client: ClientDefaults,

    #[serde(default)]
    pub server: ServerDefaults,

    #[serde(default)]
    pub presets: Vec<ServerPreset>,
}

/// Default settings for client mode
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientDefaults {
    /// Default test duration in seconds
    pub duration_secs: Option<u64>,

    /// Default number of parallel streams
    pub parallel_streams: Option<u8>,

    /// Enable TCP_NODELAY by default
    pub tcp_nodelay: Option<bool>,

    /// Default TCP window size (e.g., "1M", "512K")
    pub window_size: Option<String>,

    /// Default to JSON output
    pub json_output: Option<bool>,

    /// Disable TUI by default
    pub no_tui: Option<bool>,

    /// Timestamp format for interval output (relative, iso8601, unix)
    #[serde(default)]
    pub timestamp_format: Option<TimestampFormat>,

    /// Log file path (e.g., "~/.config/xfr/xfr.log", null to disable)
    pub log_file: Option<String>,

    /// Log level (error, warn, info, debug, trace)
    pub log_level: Option<String>,

    /// Pre-shared key for authentication
    pub psk: Option<String>,

    /// Enable TLS
    pub tls: Option<bool>,

    /// TLS client certificate path
    pub tls_cert: Option<String>,

    /// TLS client key path
    pub tls_key: Option<String>,

    /// Skip TLS certificate verification
    pub tls_insecure: Option<bool>,

    /// TUI color theme
    pub theme: Option<String>,

    /// Address family preference (ipv4, ipv6, dual)
    pub address_family: Option<String>,
}

/// Default settings for server mode
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerDefaults {
    /// Default port
    pub port: Option<u16>,

    /// Exit after one test by default
    pub one_off: Option<bool>,

    /// Prometheus metrics port
    pub prometheus_port: Option<u16>,

    /// Prometheus push gateway URL (e.g., "http://pushgateway:9091")
    pub push_gateway: Option<String>,

    /// Log file path (e.g., "~/.config/xfr/xfr.log", null to disable)
    pub log_file: Option<String>,

    /// Log level (error, warn, info, debug, trace)
    pub log_level: Option<String>,

    /// Pre-shared key for authentication
    pub psk: Option<String>,

    /// Enable TLS
    pub tls: Option<bool>,

    /// TLS certificate path
    pub tls_cert: Option<String>,

    /// TLS key path
    pub tls_key: Option<String>,

    /// TLS CA for client certificate verification
    pub tls_ca: Option<String>,

    /// Max concurrent tests per IP
    pub rate_limit: Option<u32>,

    /// Rate limit window in seconds
    pub rate_limit_window: Option<u64>,

    /// IP allow list (CIDR notation)
    pub allow: Option<Vec<String>>,

    /// IP deny list (CIDR notation)
    pub deny: Option<Vec<String>>,

    /// ACL file path
    pub acl_file: Option<String>,

    /// Address family preference (ipv4, ipv6, dual)
    pub address_family: Option<String>,
}

/// Server preset for quick configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerPreset {
    /// Preset name (used with --preset flag)
    pub name: String,

    /// Bandwidth limit (e.g., "100M", "1G")
    pub bandwidth_limit: Option<String>,

    /// Allowed client IP addresses/ranges
    pub allowed_clients: Option<Vec<String>>,

    /// Maximum test duration in seconds
    pub max_duration_secs: Option<u64>,
}

impl Config {
    /// Load configuration from the default path.
    /// Returns default config if file doesn't exist.
    pub fn load() -> anyhow::Result<Self> {
        let config_path = Self::config_path();
        if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path)?;
            Ok(toml::from_str(&contents)?)
        } else {
            Ok(Self::default())
        }
    }

    /// Get the default config file path
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("xfr")
            .join("config.toml")
    }

    /// Get a preset by name
    pub fn get_preset(&self, name: &str) -> Option<&ServerPreset> {
        self.presets.iter().find(|p| p.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.presets.is_empty());
        assert!(config.client.duration_secs.is_none());
    }

    #[test]
    fn test_parse_config() {
        let toml = r#"
[client]
duration_secs = 30
parallel_streams = 4
tcp_nodelay = true

[server]
port = 9000
prometheus_port = 9090

[[presets]]
name = "limited"
bandwidth_limit = "100M"
max_duration_secs = 60

[[presets]]
name = "internal"
allowed_clients = ["192.168.1.0/24"]
"#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.client.duration_secs, Some(30));
        assert_eq!(config.client.parallel_streams, Some(4));
        assert_eq!(config.server.port, Some(9000));
        assert_eq!(config.presets.len(), 2);
        assert_eq!(config.presets[0].name, "limited");
        assert_eq!(config.presets[0].bandwidth_limit, Some("100M".to_string()));
    }

    #[test]
    fn test_get_preset() {
        let toml = r#"
[[presets]]
name = "fast"
bandwidth_limit = "1G"

[[presets]]
name = "slow"
bandwidth_limit = "10M"
"#;
        let config: Config = toml::from_str(toml).unwrap();

        let fast = config.get_preset("fast").unwrap();
        assert_eq!(fast.bandwidth_limit, Some("1G".to_string()));

        let slow = config.get_preset("slow").unwrap();
        assert_eq!(slow.bandwidth_limit, Some("10M".to_string()));

        assert!(config.get_preset("nonexistent").is_none());
    }
}
