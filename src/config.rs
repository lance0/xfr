//! Configuration file support
//!
//! Loads the file named by `--config` or `XFR_CONFIG`. Otherwise the first
//! existing file of [`Config::search_paths`] wins: `xfr/config.toml` in the
//! platform configuration directory returned by `dirs::config_dir()`, then
//! `/etc/xfr/config.toml` on Unix. Files are never merged.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

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
    /// Prometheus push gateway URL for completed client results
    pub push_gateway: Option<String>,

    /// Operator labels attached to pushed metrics
    #[serde(default)]
    pub push_labels: BTreeMap<String, String>,

    /// Stable aggregate snapshot job (client only)
    pub push_job: Option<String>,

    /// Default test duration in seconds
    pub duration_secs: Option<u64>,

    /// Default number of parallel streams
    pub parallel_streams: Option<u8>,

    /// Default server port to connect to
    pub port: Option<u16>,

    /// Enable TCP_NODELAY by default
    pub tcp_nodelay: Option<bool>,

    /// Default TCP window size (e.g., "1M", "512K")
    pub window_size: Option<String>,

    /// Default target bitrate (e.g., "100M", "1G")
    pub bitrate: Option<String>,

    /// Default TCP congestion control algorithm (e.g., "cubic", "bbr")
    pub congestion: Option<String>,

    /// Default DSCP/TOS marking (e.g., "EF", "AF11", "46")
    pub dscp: Option<String>,

    /// Default interval between reports in seconds
    pub interval_secs: Option<f64>,

    /// Default to JSON output
    pub json_output: Option<bool>,

    /// Disable TUI by default
    pub no_tui: Option<bool>,

    /// Disable the background check for a newer xfr release
    pub no_update_check: Option<bool>,

    /// Timestamp format for interval output (relative, iso8601, unix)
    #[serde(default)]
    pub timestamp_format: Option<TimestampFormat>,

    /// Log file path (e.g., "~/.config/xfr/xfr.log", null to disable)
    pub log_file: Option<String>,

    /// Log level (error, warn, info, debug, trace)
    pub log_level: Option<String>,

    /// Pre-shared key for authentication
    pub psk: Option<String>,

    /// TUI color theme
    pub theme: Option<String>,

    /// Address family preference (ipv4, ipv6, dual)
    pub address_family: Option<String>,

    /// Local address to bind to (e.g., "192.168.1.100" or "\[::1\]:0")
    pub bind: Option<String>,

    /// Client source port for firewall traversal
    pub cport: Option<u16>,

    /// Omit first N seconds from interval output (TCP ramp-up)
    pub omit_secs: Option<u64>,
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

    /// Operator labels attached to pushed metrics
    #[serde(default)]
    pub push_labels: BTreeMap<String, String>,

    /// Log file path (e.g., "~/.config/xfr/xfr.log", null to disable)
    pub log_file: Option<String>,

    /// Log level (error, warn, info, debug, trace)
    pub log_level: Option<String>,

    /// Pre-shared key for authentication
    pub psk: Option<String>,

    /// Max concurrent tests per IP
    pub rate_limit: Option<u32>,

    /// Rate limit window in whole seconds (must be at least 1)
    pub rate_limit_window: Option<u64>,

    /// IP allow list (CIDR notation)
    pub allow: Option<Vec<String>>,

    /// IP deny list (CIDR notation)
    pub deny: Option<Vec<String>>,

    /// ACL file path
    pub acl_file: Option<String>,

    /// Address family preference (ipv4, ipv6, dual)
    pub address_family: Option<String>,

    /// Disable mDNS service registration
    pub no_mdns: Option<bool>,
}

/// Server preset for quick configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerPreset {
    /// Preset name (used with --preset flag)
    pub name: String,

    /// Reserved bandwidth limit string (e.g., "100M", "1G").
    /// Currently unused; present for forward/backward compatibility with config files.
    pub bandwidth_limit: Option<String>,

    /// Allowed client IP addresses/ranges
    pub allowed_clients: Option<Vec<String>>,

    /// Maximum test duration in seconds
    pub max_duration_secs: Option<u64>,
}

impl Config {
    /// Load configuration from the first existing file of
    /// [`Config::search_paths`]. Returns default config if none exists.
    pub fn load() -> anyhow::Result<Self> {
        Ok(Self::load_with(None)?.0)
    }

    /// Load the explicitly requested `(path, source)`, where `source` names
    /// where the path came from (e.g. `--config`) for the missing-file error.
    /// Without one, load the first existing file of [`Config::search_paths`].
    /// Also returns the file that was loaded, if any.
    pub fn load_with(explicit: Option<(&Path, &str)>) -> anyhow::Result<(Self, Option<PathBuf>)> {
        Self::resolve(explicit, &Self::search_paths())
    }

    fn resolve(
        explicit: Option<(&Path, &str)>,
        candidates: &[PathBuf],
    ) -> anyhow::Result<(Self, Option<PathBuf>)> {
        if let Some((path, source)) = explicit {
            let config = Self::load_from(path)?.with_context(|| {
                format!(
                    "config file {} (from {source}) does not exist",
                    path.display()
                )
            })?;
            return Ok((config, Some(path.to_path_buf())));
        }
        for path in candidates {
            if let Some(config) = Self::load_from(path)? {
                return Ok((config, Some(path.clone())));
            }
        }
        Ok((Self::default(), None))
    }

    /// Returns `None` when the file does not exist.
    fn load_from(config_path: &Path) -> anyhow::Result<Option<Self>> {
        let mut file = match std::fs::File::open(config_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to read config file {}", config_path.display())
                });
            }
        };

        // Capture metadata from the same open handle that supplies the
        // contents, avoiding a path-swap race during the permissions check.
        #[cfg(unix)]
        let metadata = file
            .metadata()
            .with_context(|| format!("failed to inspect config file {}", config_path.display()))?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .with_context(|| format!("failed to read config file {}", config_path.display()))?;

        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse config file {}", config_path.display()))?;

        #[cfg(unix)]
        if config.client.psk.is_some() || config.server.psk.is_some() {
            crate::auth::require_owner_only_permissions(
                config_path,
                &metadata,
                "config file containing a PSK",
            )?;
        }

        Ok(Some(config))
    }

    /// Get the per-user config file path
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("xfr")
            .join("config.toml")
    }

    /// Config files searched, in order, when none is given explicitly: the
    /// per-user path, then `/etc/xfr/config.toml` on Unix.
    pub fn search_paths() -> Vec<PathBuf> {
        let mut paths = vec![Self::config_path()];
        #[cfg(unix)]
        paths.push(PathBuf::from("/etc/xfr/config.toml"));
        paths
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
    fn load_returns_defaults_only_when_config_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("missing").join("config.toml");

        assert!(Config::load_from(&config_path).unwrap().is_none());
        let (config, loaded) = Config::resolve(None, &[config_path]).unwrap();

        assert!(loaded.is_none());
        assert!(config.presets.is_empty());
        assert!(config.client.duration_secs.is_none());
        assert!(config.server.psk.is_none());
    }

    #[test]
    fn explicit_config_wins_and_first_existing_search_path_wins() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.toml");
        let user = dir.path().join("user.toml");
        let system = dir.path().join("system.toml");
        let explicit = dir.path().join("explicit.toml");
        std::fs::write(&system, "[server]\nport = 1\n").unwrap();
        std::fs::write(&explicit, "[server]\nport = 3\n").unwrap();
        let candidates = [missing, user.clone(), system.clone()];

        // The user file is missing, so the system file is used.
        let (config, loaded) = Config::resolve(None, &candidates).unwrap();
        assert_eq!(config.server.port, Some(1));
        assert_eq!(loaded.as_deref(), Some(system.as_path()));

        // An existing user file shadows the system file entirely.
        std::fs::write(&user, "[client]\nport = 2\n").unwrap();
        let (config, loaded) = Config::resolve(None, &candidates).unwrap();
        assert_eq!(config.client.port, Some(2));
        assert_eq!(config.server.port, None);
        assert_eq!(loaded.as_deref(), Some(user.as_path()));

        // An explicit file is used even when search paths exist.
        let (config, loaded) = Config::resolve(Some((&explicit, "--config")), &candidates).unwrap();
        assert_eq!(config.server.port, Some(3));
        assert_eq!(loaded.as_deref(), Some(explicit.as_path()));
    }

    #[test]
    fn missing_explicit_config_is_an_error_naming_path_and_source() {
        let dir = tempfile::tempdir().unwrap();
        let fallback = dir.path().join("config.toml");
        std::fs::write(&fallback, "[server]\nport = 1\n").unwrap();
        let missing = dir.path().join("missing.toml");

        for source in ["--config", "XFR_CONFIG"] {
            let error = Config::resolve(Some((&missing, source)), std::slice::from_ref(&fallback))
                .unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains(missing.to_str().unwrap()), "{message}");
            assert!(message.contains(&format!("(from {source})")), "{message}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn explicit_config_with_psk_requires_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("xfr.toml");
        std::fs::write(&config_path, "[client]\npsk = \"secret\"\n").unwrap();
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let error = Config::resolve(Some((&config_path, "--config")), &[]).unwrap_err();
        assert!(format!("{error:#}").contains("overly broad permissions (644)"));

        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (config, _) = Config::resolve(Some((&config_path, "--config")), &[]).unwrap();
        assert_eq!(config.client.psk.as_deref(), Some("secret"));
    }

    #[test]
    fn load_rejects_malformed_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "[server\npsk = \"secret\"").unwrap();

        let error = Config::load_from(&config_path).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("failed to parse config file"));
        assert!(message.contains(config_path.to_str().unwrap()));
    }

    #[test]
    fn load_rejects_unreadable_existing_config_path() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::create_dir(&config_path).unwrap();

        let error = Config::load_from(&config_path).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("failed to read config file"));
        assert!(message.contains(config_path.to_str().unwrap()));
    }

    #[test]
    fn load_parses_existing_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "[server]\npsk = \"secret\"\nrate_limit = 3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let config = Config::load_from(&config_path).unwrap().unwrap();

        assert_eq!(config.server.psk.as_deref(), Some("secret"));
        assert_eq!(config.server.rate_limit, Some(3));
    }

    #[cfg(unix)]
    #[test]
    fn load_rejects_broad_permissions_when_config_contains_psk() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "[server]\npsk = \"secret\"\n").unwrap();
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let error = Config::load_from(&config_path).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("config file containing a PSK"));
        assert!(message.contains("overly broad permissions (644)"));
        assert!(message.contains(config_path.to_str().unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn load_allows_broad_permissions_when_config_has_no_psk() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "[server]\nport = 9000\n").unwrap();
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let config = Config::load_from(&config_path).unwrap().unwrap();

        assert_eq!(config.server.port, Some(9000));
        assert!(config.server.psk.is_none());
    }

    #[test]
    fn test_parse_config() {
        let toml = r#"
[client]
duration_secs = 30
parallel_streams = 4
port = 5202
tcp_nodelay = true
push_gateway = "http://localhost:8429/api/v1/import/prometheus"

[server]
port = 9000
prometheus_port = 9090

[[presets]]
name = "limited"
# Reserved for future use; not enforced today.
bandwidth_limit = "100M"
max_duration_secs = 60

[[presets]]
name = "internal"
allowed_clients = ["192.168.1.0/24"]
"#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.client.duration_secs, Some(30));
        assert_eq!(config.client.parallel_streams, Some(4));
        assert_eq!(config.client.port, Some(5202));
        assert_eq!(
            config.client.push_gateway.as_deref(),
            Some("http://localhost:8429/api/v1/import/prometheus")
        );
        assert_eq!(config.server.port, Some(9000));
        assert_eq!(config.server.no_mdns, None);
        assert_eq!(config.presets.len(), 2);
        assert_eq!(config.presets[0].name, "limited");
        assert_eq!(config.presets[0].bandwidth_limit, Some("100M".to_string()));
    }

    #[test]
    fn test_parse_client_omit_secs() {
        let toml = r#"
[client]
omit_secs = 3
"#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.client.omit_secs, Some(3));
    }

    #[test]
    fn test_parse_client_transport_defaults() {
        let toml = r#"
[client]
bitrate = "100M"
congestion = "bbr"
dscp = "EF"
interval_secs = 2.5
bind = "192.168.1.10"
cport = 5202
"#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.client.bitrate.as_deref(), Some("100M"));
        assert_eq!(config.client.congestion.as_deref(), Some("bbr"));
        assert_eq!(config.client.dscp.as_deref(), Some("EF"));
        assert_eq!(config.client.interval_secs, Some(2.5));
        assert_eq!(config.client.bind.as_deref(), Some("192.168.1.10"));
        assert_eq!(config.client.cport, Some(5202));
    }

    #[test]
    fn test_parse_server_no_mdns() {
        let toml = r#"
[server]
no_mdns = true
"#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.server.no_mdns, Some(true));
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

    #[test]
    fn test_preset_max_duration_secs_is_parsed() {
        // `max_duration_secs` must round-trip through the config file so the
        // server command can apply it as the default when `--max-duration` is
        // not provided.
        let toml = r#"
[[presets]]
name = "limited"
bandwidth_limit = "100M"
max_duration_secs = 300
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let preset = config.get_preset("limited").unwrap();
        assert_eq!(preset.max_duration_secs, Some(300));
        // `bandwidth_limit` is accepted for compatibility but not enforced.
        assert_eq!(preset.bandwidth_limit, Some("100M".to_string()));
    }
}
