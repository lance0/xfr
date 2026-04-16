//! xfr - Modern network bandwidth testing with TUI

use std::io::{self, Write};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tracing::info;
use tracing_subscriber::EnvFilter;

use xfr::client::{Client, ClientConfig, TestProgress};
use xfr::config::Config;
use xfr::diff::{DiffConfig, run_diff};
use xfr::output::{output_csv, output_json, output_plain};

/// Output format options
struct OutputOptions {
    json: bool,
    json_stream: bool,
    csv: bool,
    quiet: bool,
    omit_secs: u64,
    interval_secs: f64,
    timestamp_format: TimestampFormat,
}
use xfr::client::PauseResult;
use xfr::protocol::{DEFAULT_PORT, Direction, Protocol, TimestampFormat};
use xfr::serve::{Server, ServerConfig};
use xfr::tui::app::AppState;
use xfr::tui::server::{ServerApp, ServerEvent, draw as server_draw};
use xfr::tui::settings::SettingsAction;
use xfr::tui::{App, draw};

/// Initialize logging with optional file output
/// Returns the WorkerGuard if file logging is enabled - must be held until program exit
/// When suppress_console is true, no console output is produced (for TUI mode)
fn init_logging(
    log_file: Option<&str>,
    log_level: Option<&str>,
    suppress_console: bool,
) -> anyhow::Result<Option<tracing_appender::non_blocking::WorkerGuard>> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let level = log_level.unwrap_or("info");
    let env_filter = EnvFilter::from_default_env().add_directive(format!("xfr={}", level).parse()?);

    match (log_file, suppress_console) {
        (Some(file_path), true) => {
            // TUI mode with file logging: file only, no console
            let expanded_path = expand_log_path(file_path);
            if let Some(parent) = expanded_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file_appender = tracing_appender::rolling::daily(
                expanded_path
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new(".")),
                expanded_path
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("xfr.log")),
            );
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
            let file_layer = tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_writer(non_blocking)
                .with_ansi(false);
            tracing_subscriber::registry()
                .with(env_filter)
                .with(file_layer)
                .init();
            Ok(Some(guard))
        }
        (Some(file_path), false) => {
            // Normal mode with file logging: console + file
            let expanded_path = expand_log_path(file_path);
            if let Some(parent) = expanded_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file_appender = tracing_appender::rolling::daily(
                expanded_path
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new(".")),
                expanded_path
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("xfr.log")),
            );
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
            let file_layer = tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_writer(non_blocking)
                .with_ansi(false);
            let console_layer = tracing_subscriber::fmt::layer()
                .with_target(false)
                .without_time()
                .with_writer(std::io::stderr);
            tracing_subscriber::registry()
                .with(env_filter)
                .with(console_layer)
                .with(file_layer)
                .init();
            Ok(Some(guard))
        }
        (None, true) => {
            // TUI mode without file logging: no logging at all (silent)
            Ok(None)
        }
        (None, false) => {
            // Normal mode: console logging only (to stderr)
            let console_layer = tracing_subscriber::fmt::layer()
                .with_target(false)
                .without_time()
                .with_writer(std::io::stderr);
            tracing_subscriber::registry()
                .with(env_filter)
                .with(console_layer)
                .init();
            Ok(None)
        }
    }
}

fn expand_log_path(file_path: &str) -> PathBuf {
    if file_path.starts_with("~/") {
        dirs::home_dir()
            .map(|home| home.join(&file_path[2..]))
            .unwrap_or_else(|| PathBuf::from(file_path))
    } else {
        PathBuf::from(file_path)
    }
}

#[derive(Parser)]
#[command(name = "xfr")]
#[command(author, version, about = "Modern network bandwidth testing with TUI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Generate shell completions
    #[arg(long, value_name = "SHELL", value_parser = ["bash", "zsh", "fish", "powershell", "elvish"])]
    completions: Option<String>,

    /// Target host for client mode
    #[arg(value_name = "HOST")]
    host: Option<String>,

    /// Server/client port
    #[arg(short, long, default_value_t = DEFAULT_PORT, env = "XFR_PORT")]
    port: u16,

    /// Test duration (use 0 for infinite)
    #[arg(short = 't', long, default_value = "10s", value_parser = parse_test_duration, env = "XFR_DURATION")]
    time: Duration,

    /// UDP mode
    #[arg(short = 'u', long, conflicts_with = "quic")]
    udp: bool,

    /// QUIC mode (encrypted, multiplexed streams)
    #[arg(short = 'Q', long, conflicts_with = "udp")]
    quic: bool,

    /// Target bitrate (e.g., 1G, 100M). Applies to TCP and UDP. 0 = unlimited.
    #[arg(short = 'b', long, value_parser = parse_bitrate)]
    bitrate: Option<u64>,

    /// Number of parallel streams (1-128)
    #[arg(short = 'P', long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=128))]
    parallel: u8,

    /// Reverse direction (server sends to client)
    #[arg(short = 'R', long, conflicts_with = "bidir")]
    reverse: bool,

    /// Bidirectional test
    #[arg(long, conflicts_with = "reverse")]
    bidir: bool,

    /// JSON output
    #[arg(long, conflicts_with = "csv")]
    json: bool,

    /// JSON streaming output (one object per line)
    #[arg(long, conflicts_with = "csv")]
    json_stream: bool,

    /// CSV output
    #[arg(long, conflicts_with_all = ["json", "json_stream"])]
    csv: bool,

    /// Quiet mode - suppress interval output, show only summary
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Output file
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,

    /// Disable TUI
    #[arg(long)]
    no_tui: bool,

    /// Color theme (default, kawaii, cyber, dracula, monochrome, matrix, nord, gruvbox, catppuccin, tokyo_night, solarized)
    #[arg(long, default_value = "default")]
    theme: String,

    /// Report interval in seconds
    #[arg(short = 'i', long, default_value = "1.0")]
    interval: f64,

    /// Omit first N seconds from interval output (TCP ramp-up)
    #[arg(long)]
    omit: Option<u64>,

    /// Disable Nagle algorithm
    #[arg(long)]
    tcp_nodelay: bool,

    /// TCP congestion control algorithm (e.g. cubic, bbr, reno)
    #[arg(long = "congestion", value_name = "ALGO")]
    congestion: Option<String>,

    /// DSCP/TOS marking: raw TOS byte (0-255) or DSCP name (EF, AF11, CS1, etc.)
    #[arg(long, value_name = "VALUE")]
    dscp: Option<String>,

    /// TCP window size (e.g., 512K, 1M)
    #[arg(long, value_parser = parse_size)]
    window: Option<usize>,

    /// Timestamp format for interval output (relative, iso8601, unix)
    #[arg(long, value_parser = parse_timestamp_format, env = "XFR_TIMESTAMP_FORMAT")]
    timestamp_format: Option<TimestampFormat>,

    /// Log file path (e.g., "~/.config/xfr/xfr.log")
    #[arg(long, env = "XFR_LOG_FILE")]
    log_file: Option<String>,

    /// Log level (error, warn, info, debug, trace)
    #[arg(long, env = "XFR_LOG_LEVEL")]
    log_level: Option<String>,

    /// Pre-shared key for server authentication
    #[arg(long, env = "XFR_PSK")]
    psk: Option<String>,

    /// Read PSK from file
    #[arg(long)]
    psk_file: Option<PathBuf>,

    /// Force IPv4 only
    #[arg(short = '4', long = "ipv4")]
    ipv4_only: bool,

    /// Force IPv6 only
    #[arg(short = '6', long = "ipv6")]
    ipv6_only: bool,

    /// Local address to bind to (e.g., 192.168.1.100 or 192.168.1.100:0)
    #[arg(long, value_name = "ADDR")]
    bind: Option<String>,

    /// Client source port for firewall traversal (UDP/QUIC/TCP data streams)
    #[arg(long, value_name = "PORT")]
    cport: Option<u16>,

    /// MPTCP mode (Multi-Path TCP, Linux 5.6+)
    #[arg(long, conflicts_with_all = ["udp", "quic"])]
    mptcp: bool,

    /// Use random payload data (default for TCP/UDP client-sent traffic)
    #[arg(long, conflicts_with = "zeros")]
    random: bool,

    /// Use zero-filled payload data instead of random bytes
    #[arg(long, conflicts_with = "random")]
    zeros: bool,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Start server mode
    Serve {
        /// Server port
        #[arg(short, long, default_value_t = DEFAULT_PORT, env = "XFR_PORT")]
        port: u16,

        /// Exit after one test
        #[arg(long)]
        one_off: bool,

        /// Enable TUI dashboard
        #[arg(long)]
        tui: bool,

        /// Maximum test duration (server-side limit)
        #[arg(long, value_parser = parse_duration)]
        max_duration: Option<Duration>,

        /// Prometheus metrics port
        #[cfg(feature = "prometheus")]
        #[arg(long)]
        prometheus: Option<u16>,

        /// Prometheus push gateway URL (e.g., http://pushgateway:9091)
        #[arg(long, env = "XFR_PUSH_GATEWAY")]
        push_gateway: Option<String>,

        /// Log file path (e.g., "~/.config/xfr/xfr.log")
        #[arg(long, env = "XFR_LOG_FILE")]
        log_file: Option<String>,

        /// Log level (error, warn, info, debug, trace)
        #[arg(long, env = "XFR_LOG_LEVEL")]
        log_level: Option<String>,

        /// Pre-shared key for authentication
        #[arg(long, env = "XFR_PSK")]
        psk: Option<String>,

        /// Read PSK from file
        #[arg(long)]
        psk_file: Option<PathBuf>,

        /// Max concurrent tests per IP (rate limiting)
        #[arg(long)]
        rate_limit: Option<u32>,

        /// Rate limit time window
        #[arg(long, default_value = "60s", value_parser = parse_duration)]
        rate_limit_window: Duration,

        /// Allow IP/subnet (can be repeated)
        #[arg(long = "allow", action = clap::ArgAction::Append)]
        allow: Vec<String>,

        /// Deny IP/subnet (can be repeated)
        #[arg(long = "deny", action = clap::ArgAction::Append)]
        deny: Vec<String>,

        /// ACL rules file
        #[arg(long)]
        acl_file: Option<PathBuf>,

        /// Bind to specific address (e.g., 192.168.1.1, ::1)
        #[arg(short = 'B', long)]
        bind: Option<IpAddr>,

        /// Force IPv4 only
        #[arg(short = '4', long = "ipv4")]
        ipv4_only: bool,

        /// Force IPv6 only
        #[arg(short = '6', long = "ipv6")]
        ipv6_only: bool,

        /// Disable mDNS service registration
        #[arg(long)]
        no_mdns: bool,
    },

    /// Compare two test results
    Diff {
        /// Baseline result file
        baseline: PathBuf,

        /// Current result file
        current: PathBuf,

        /// Regression threshold percentage
        #[arg(long, default_value = "0")]
        threshold: f64,
    },

    /// Discover xfr servers on LAN
    Discover {
        /// Discovery timeout
        #[arg(long, default_value = "5s", value_parser = parse_duration)]
        timeout: Duration,
    },
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    humantime::parse_duration(s).map_err(|e| e.to_string())
}

fn parse_test_duration(s: &str) -> Result<Duration, String> {
    let duration = humantime::parse_duration(s).map_err(|e| e.to_string())?;
    // Allow 0 for infinite duration, otherwise require at least 1 second
    if duration == Duration::ZERO {
        return Ok(Duration::ZERO);
    }
    if duration < Duration::from_secs(1) {
        return Err("Minimum test duration is 1 second (use 0 for infinite)".to_string());
    }
    Ok(duration)
}

fn parse_bitrate(s: &str) -> Result<u64, String> {
    let s = s.to_uppercase();
    let (num, suffix) = if s.ends_with('G') {
        (s.trim_end_matches('G'), 1_000_000_000u64)
    } else if s.ends_with('M') {
        (s.trim_end_matches('M'), 1_000_000u64)
    } else if s.ends_with('K') {
        (s.trim_end_matches('K'), 1_000u64)
    } else {
        (s.as_str(), 1u64)
    };

    num.parse::<u64>().map_err(|e| e.to_string()).and_then(|n| {
        n.checked_mul(suffix)
            .ok_or_else(|| format!("Bitrate overflow: {}", s))
    })
}

fn parse_size(s: &str) -> Result<usize, String> {
    let s = s.to_uppercase();
    let (num, suffix) = if s.ends_with('G') {
        (s.trim_end_matches('G'), 1024 * 1024 * 1024usize)
    } else if s.ends_with('M') {
        (s.trim_end_matches('M'), 1024 * 1024usize)
    } else if s.ends_with('K') {
        (s.trim_end_matches('K'), 1024usize)
    } else {
        (s.as_str(), 1usize)
    };

    num.parse::<usize>()
        .map_err(|e| e.to_string())
        .and_then(|n| {
            n.checked_mul(suffix)
                .ok_or_else(|| format!("Size overflow: {}", s))
        })
}

fn parse_timestamp_format(s: &str) -> Result<TimestampFormat, String> {
    s.parse::<TimestampFormat>()
}

fn parse_dscp(s: &str) -> Result<u8, String> {
    // Try numeric first (0-255)
    if let Ok(n) = s.parse::<u32>() {
        if n > 255 {
            return Err(format!("TOS value {} exceeds 255", n));
        }
        return Ok(n as u8);
    }
    // DSCP name → TOS byte (DSCP value << 2)
    match s.to_uppercase().as_str() {
        "CS0" => Ok(0),
        "CS1" => Ok(32),
        "CS2" => Ok(64),
        "CS3" => Ok(96),
        "CS4" => Ok(128),
        "CS5" => Ok(160),
        "CS6" => Ok(192),
        "CS7" => Ok(224),
        "AF11" => Ok(40),
        "AF12" => Ok(48),
        "AF13" => Ok(56),
        "AF21" => Ok(72),
        "AF22" => Ok(80),
        "AF23" => Ok(88),
        "AF31" => Ok(104),
        "AF32" => Ok(112),
        "AF33" => Ok(120),
        "AF41" => Ok(136),
        "AF42" => Ok(144),
        "AF43" => Ok(152),
        "EF" => Ok(184),
        "VA" => Ok(172),
        _ => Err(format!(
            "Unknown DSCP name '{}'. Use a number (0-255) or a name: EF, AF11-AF43, CS0-CS7",
            s
        )),
    }
}

/// Parse a bind address string (can be "IP" or "IP:port")
fn parse_bind_address(s: &str) -> anyhow::Result<std::net::SocketAddr> {
    use std::net::SocketAddr;

    // Try parsing as full socket address first (IP:port or [IPv6]:port)
    if let Ok(addr) = s.parse::<SocketAddr>() {
        return Ok(addr);
    }

    // Try parsing as IP only (use port 0 for auto-assign)
    if let Ok(ip) = s.parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, 0));
    }

    // Handle bracketed IPv6 without port: [::1] -> ::1
    if s.starts_with('[') && s.ends_with(']') {
        let inner = &s[1..s.len() - 1];
        if let Ok(ip) = inner.parse::<std::net::IpAddr>() {
            return Ok(SocketAddr::new(ip, 0));
        }
    }

    anyhow::bail!(
        "Invalid bind address: {}. Use IP or IP:port format (for IPv6: [::1] or [::1]:port).",
        s
    )
}

/// Validate bind address compatibility with protocol and stream count
fn validate_bind_address(
    bind_addr: Option<std::net::SocketAddr>,
    protocol: xfr::protocol::Protocol,
    streams: u8,
    has_cport: bool,
) -> anyhow::Result<()> {
    if let Some(addr) = bind_addr
        && addr.port() != 0
    {
        // TCP: explicit bind port conflicts with control/data unless it came from --cport,
        // which uses the port only for data streams.
        if protocol == xfr::protocol::Protocol::Tcp && !has_cport {
            anyhow::bail!(
                "Cannot use explicit bind port {} with TCP (control and data connections conflict). \
                 Use just the IP address (e.g., --bind {}) to let the OS assign ports, \
                 or use --bind {} --cport {} to pin TCP data source ports.",
                addr.port(),
                addr.ip(),
                addr.ip(),
                addr.port()
            );
        }
        // UDP/QUIC with multiple streams: --cport uses sequential ports, --bind does not
        if streams > 1 && !has_cport {
            anyhow::bail!(
                "Cannot use explicit bind port {} with multiple streams (-P {}). \
                 Use just the IP address (e.g., --bind {}) to let the OS assign ports, \
                 or use --cport {} for sequential port assignment.",
                addr.port(),
                streams,
                addr.ip(),
                addr.port()
            );
        }
    }
    Ok(())
}

fn effective_random_payload(cli: &Cli) -> bool {
    debug_assert!(!(cli.random && cli.zeros));
    !cli.zeros
}

fn interval_retransmits(progress: &TestProgress, last_retransmits: &mut u64) -> Option<u64> {
    if let Some(cumulative) = progress.total_retransmits {
        let delta = cumulative.saturating_sub(*last_retransmits);
        *last_retransmits = cumulative;
        Some(delta)
    } else {
        let has_any = progress.streams.iter().any(|s| s.retransmits.is_some());
        if has_any {
            Some(progress.streams.iter().filter_map(|s| s.retransmits).sum())
        } else {
            None
        }
    }
}

fn random_payload_notice(
    protocol: Protocol,
    direction: Direction,
    random_enabled: bool,
) -> Option<&'static str> {
    if protocol == Protocol::Quic {
        if random_enabled {
            return Some("random payloads are ignored with QUIC (payload is already encrypted)");
        }
        // QUIC encrypts payload regardless, so --zeros is irrelevant
        return None;
    }
    // When --zeros is used with reverse/bidir, warn that the server's payload mode
    // is not negotiated — the client cannot control what the server sends.
    if !random_enabled && matches!(direction, Direction::Download | Direction::Bidir) {
        return Some(
            "--zeros only affects client-sent traffic; server payload mode is not negotiated",
        );
    }
    None
}

fn generate_completions(shell: &str) {
    use clap::CommandFactory;
    use clap_complete::{Shell, generate};

    let mut cmd = Cli::command();
    let shell = match shell {
        "bash" => Shell::Bash,
        "zsh" => Shell::Zsh,
        "fish" => Shell::Fish,
        "powershell" => Shell::PowerShell,
        "elvish" => Shell::Elvish,
        _ => {
            eprintln!("Unknown shell: {}", shell);
            std::process::exit(1);
        }
    };
    generate(shell, &mut cmd, "xfr", &mut std::io::stdout());
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle shell completions early (before logging init)
    if let Some(ref shell) = cli.completions {
        generate_completions(shell);
        return Ok(());
    }

    // Load config file (falls back to defaults if not found)
    let file_config = Config::load().unwrap_or_default();

    // Determine logging config based on command (server vs client)
    let (effective_log_file, effective_log_level, tui_enabled) = match &cli.command {
        Some(Commands::Serve {
            log_file,
            log_level,
            tui: server_tui,
            ..
        }) => {
            let file = log_file
                .as_ref()
                .or(file_config.server.log_file.as_ref())
                .or(cli.log_file.as_ref())
                .or(file_config.client.log_file.as_ref());
            let level = log_level
                .as_ref()
                .or(file_config.server.log_level.as_ref())
                .or(cli.log_level.as_ref())
                .or(file_config.client.log_level.as_ref());
            (file, level, *server_tui)
        }
        Some(Commands::Diff { .. }) | Some(Commands::Discover { .. }) => {
            let file = cli
                .log_file
                .as_ref()
                .or(file_config.client.log_file.as_ref());
            let level = cli
                .log_level
                .as_ref()
                .or(file_config.client.log_level.as_ref());
            (file, level, false)
        }
        _ => {
            // Client mode: TUI enabled by default unless --no-tui
            let file = cli
                .log_file
                .as_ref()
                .or(file_config.client.log_file.as_ref());
            let level = cli
                .log_level
                .as_ref()
                .or(file_config.client.log_level.as_ref());
            let no_tui = cli.no_tui || file_config.client.no_tui.unwrap_or(false);
            (file, level, !no_tui)
        }
    };
    // Hold the logging guard until program exit to ensure all logs are flushed
    // Suppress console logging when TUI is active to prevent log messages from corrupting the display
    let _log_guard = init_logging(
        effective_log_file.map(|s| s.as_str()),
        effective_log_level.map(|s| s.as_str()),
        tui_enabled,
    )?;

    match cli.command {
        Some(Commands::Serve {
            port,
            one_off,
            tui: server_tui,
            max_duration,
            #[cfg(feature = "prometheus")]
            prometheus,
            push_gateway,
            log_file: _serve_log_file,
            log_level: _serve_log_level,
            psk,
            psk_file,
            rate_limit,
            rate_limit_window,
            allow,
            deny,
            acl_file,
            bind,
            ipv4_only,
            ipv6_only,
            no_mdns,
        }) => {
            // Use CLI values, falling back to config file, then defaults
            let server_port = if port != DEFAULT_PORT {
                port
            } else {
                file_config.server.port.unwrap_or(DEFAULT_PORT)
            };

            let server_one_off = one_off || file_config.server.one_off.unwrap_or(false);

            #[cfg(feature = "prometheus")]
            let prom_port = prometheus.or(file_config.server.prometheus_port);

            let push_gateway_url = push_gateway.or_else(|| file_config.server.push_gateway.clone());

            // Build PSK (CLI > file > config)
            let effective_psk = if let Some(psk_path) = psk_file {
                Some(xfr::auth::read_psk_file(&psk_path)?)
            } else {
                psk.or_else(|| file_config.server.psk.clone())
            };

            // Validate PSK if provided
            if let Some(ref psk_value) = effective_psk {
                xfr::auth::validate_psk(psk_value)?;
            }

            // Build security configs
            let auth_config = xfr::auth::AuthConfig { psk: effective_psk };

            let acl_config = xfr::acl::AclConfig {
                allow: if allow.is_empty() {
                    file_config.server.allow.clone().unwrap_or_default()
                } else {
                    allow
                },
                deny: if deny.is_empty() {
                    file_config.server.deny.clone().unwrap_or_default()
                } else {
                    deny
                },
                file: acl_file
                    .map(|p| p.to_string_lossy().to_string())
                    .or_else(|| file_config.server.acl_file.clone()),
            };

            let rate_limit_config = xfr::rate_limit::RateLimitConfig {
                max_per_ip: rate_limit.or(file_config.server.rate_limit),
                window_secs: rate_limit_window.as_secs(),
            };

            // Determine address family
            // When --bind is specified, derive family from the IP and validate
            // against -4/-6 to prevent contradictions (e.g., --bind 127.0.0.1 -6)
            let address_family = if let Some(ip) = bind {
                if ip.is_unspecified() {
                    anyhow::bail!(
                        "--bind {} is an unspecified address; use -4/-6 instead to control address family",
                        ip
                    );
                }
                let derived = if ip.is_ipv4() {
                    xfr::net::AddressFamily::V4Only
                } else {
                    xfr::net::AddressFamily::V6Only
                };
                if ipv4_only && ip.is_ipv6() {
                    anyhow::bail!("--bind {} is IPv6 but -4/--ipv4 forces IPv4", ip);
                }
                if ipv6_only && ip.is_ipv4() {
                    anyhow::bail!("--bind {} is IPv4 but -6/--ipv6 forces IPv6", ip);
                }
                derived
            } else if ipv4_only {
                xfr::net::AddressFamily::V4Only
            } else if ipv6_only {
                xfr::net::AddressFamily::V6Only
            } else if let Some(ref af) = file_config.server.address_family {
                af.parse().unwrap_or_default()
            } else {
                xfr::net::AddressFamily::default()
            };

            let server_no_mdns = no_mdns || file_config.server.no_mdns.unwrap_or(false);

            let config = ServerConfig {
                port: server_port,
                one_off: server_one_off,
                max_duration,
                #[cfg(feature = "prometheus")]
                prometheus_port: prom_port,
                push_gateway_url,
                auth: auth_config,
                acl: acl_config,
                rate_limit: rate_limit_config,
                address_family,
                bind_addr: bind,
                tui_tx: None,
                enable_quic: true,
                no_mdns: server_no_mdns,
                ..Default::default()
            };

            if server_tui {
                run_server_tui(config).await?;
            } else {
                let server = Server::new(config);
                server.run().await?;
            }
        }

        Some(Commands::Diff {
            baseline,
            current,
            threshold,
        }) => {
            let config = DiffConfig {
                threshold_percent: threshold,
            };
            let result = run_diff(&baseline, &current, &config)?;
            println!("{}", result.format_plain());

            if result.is_regression {
                std::process::exit(1);
            }
        }

        Some(Commands::Discover { timeout }) => {
            println!("Searching for xfr servers...\n");

            let servers = xfr::discover::discover(timeout).await?;

            if servers.is_empty() {
                println!("No xfr servers found.");
            } else {
                println!("Found {} server(s):", servers.len());
                for server in servers {
                    println!("  {}", server);
                }
            }
        }

        None => {
            // Client mode
            let Some(ref host) = cli.host else {
                eprintln!("Error: HOST argument required for client mode");
                eprintln!("Usage: xfr <HOST> [OPTIONS]");
                eprintln!("       xfr serve [OPTIONS]");
                std::process::exit(1);
            };

            let direction = if cli.bidir {
                Direction::Bidir
            } else if cli.reverse {
                Direction::Download
            } else {
                Direction::Upload
            };

            let protocol = if cli.quic {
                Protocol::Quic
            } else if cli.udp {
                Protocol::Udp
            } else {
                Protocol::Tcp
            };

            let random_payload = effective_random_payload(&cli);

            if let Some(msg) = random_payload_notice(protocol, direction, random_payload) {
                eprintln!("Warning: {msg}");
            }

            if cli.dscp.is_some() && protocol == xfr::protocol::Protocol::Quic {
                eprintln!("Warning: --dscp is ignored with QUIC (QUIC manages its own socket)");
            }

            #[cfg(not(unix))]
            if cli.dscp.is_some() && protocol != xfr::protocol::Protocol::Quic {
                eprintln!("Warning: --dscp is not supported on this platform");
            }

            if cli.mptcp {
                xfr::net::validate_mptcp().map_err(|e| anyhow::anyhow!("{}", e))?;
            }

            // Apply config file defaults where CLI didn't override
            let duration = if cli.time != Duration::from_secs(10) {
                cli.time
            } else {
                file_config
                    .client
                    .duration_secs
                    .map(Duration::from_secs)
                    .unwrap_or(cli.time)
            };

            let streams = if cli.parallel != 1 {
                cli.parallel
            } else {
                file_config.client.parallel_streams.unwrap_or(cli.parallel)
            };

            let tcp_nodelay = cli.tcp_nodelay || file_config.client.tcp_nodelay.unwrap_or(false);

            let window_size = cli.window.or_else(|| {
                file_config
                    .client
                    .window_size
                    .as_ref()
                    .and_then(|s| parse_size(s).ok())
            });

            let json_output = cli.json || file_config.client.json_output.unwrap_or(false);
            let no_tui = cli.no_tui || file_config.client.no_tui.unwrap_or(false);

            // Determine timestamp format (CLI > config > default)
            let timestamp_format = cli
                .timestamp_format
                .or(file_config.client.timestamp_format)
                .unwrap_or_default();

            // Build PSK (CLI > file > config)
            let client_psk = if let Some(ref psk_path) = cli.psk_file {
                Some(xfr::auth::read_psk_file(psk_path)?)
            } else {
                cli.psk.clone().or_else(|| file_config.client.psk.clone())
            };

            // Validate PSK if provided
            if let Some(ref psk_value) = client_psk {
                xfr::auth::validate_psk(psk_value)?;
            }

            // Determine address family
            let client_address_family = if cli.ipv4_only {
                xfr::net::AddressFamily::V4Only
            } else if cli.ipv6_only {
                xfr::net::AddressFamily::V6Only
            } else if let Some(ref af) = file_config.client.address_family {
                af.parse().unwrap_or_default()
            } else {
                xfr::net::AddressFamily::default()
            };

            // Parse bind address (can be "IP" or "IP:port")
            let mut bind_addr = if let Some(ref bind_str) = cli.bind {
                Some(parse_bind_address(bind_str)?)
            } else {
                None
            };

            // Merge --cport into bind_addr
            let cport = cli.cport;
            if let Some(port) = cport {
                if port == 0 {
                    anyhow::bail!("--cport must be a non-zero port number");
                }
                // Validate port range for sequential multi-stream TCP/UDP.
                // QUIC multiplexes on a single socket, so only one local port is used.
                if protocol != xfr::protocol::Protocol::Quic && streams > 1 {
                    let max_port = port as u32 + streams as u32 - 1;
                    if max_port > 65535 {
                        anyhow::bail!(
                            "--cport {} with -P {} requires ports {}-{}, which exceeds 65535",
                            port,
                            streams,
                            port,
                            max_port
                        );
                    }
                }
                if let Some(ref addr) = bind_addr {
                    if addr.port() != 0 {
                        anyhow::bail!(
                            "Cannot use --cport with --bind {}:{} (conflicting port). \
                             Use --bind {} --cport {} instead.",
                            addr.ip(),
                            addr.port(),
                            addr.ip(),
                            port
                        );
                    }
                    // --bind IP + --cport PORT → combine
                    bind_addr = Some(std::net::SocketAddr::new(addr.ip(), port));
                } else {
                    // --cport only → use unspecified IP with the given port
                    let ip = if client_address_family == xfr::net::AddressFamily::V6Only {
                        std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
                    } else {
                        std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
                    };
                    bind_addr = Some(std::net::SocketAddr::new(ip, port));
                }
            }

            // Validate bind address compatibility with protocol and stream count
            validate_bind_address(bind_addr, protocol, streams, cport.is_some())?;

            let config = ClientConfig {
                host: host.clone(),
                port: cli.port,
                protocol,
                streams,
                duration,
                direction,
                bitrate: cli.bitrate,
                tcp_nodelay,
                tcp_congestion: cli.congestion.clone(),
                window_size,
                psk: client_psk,
                address_family: client_address_family,
                bind_addr,
                sequential_ports: protocol != Protocol::Quic && cport.is_some() && streams > 1,
                mptcp: cli.mptcp,
                random_payload,
                dscp: cli
                    .dscp
                    .as_ref()
                    .map(|s| parse_dscp(s).map_err(|e| anyhow::anyhow!(e)))
                    .transpose()?,
            };

            // Determine output format
            let output_opts = OutputOptions {
                json: json_output,
                json_stream: cli.json_stream,
                csv: cli.csv,
                quiet: cli.quiet,
                omit_secs: cli
                    .omit
                    .unwrap_or_else(|| file_config.client.omit_secs.unwrap_or(0)),
                interval_secs: cli.interval,
                timestamp_format,
            };

            if no_tui || json_output || cli.json_stream || cli.csv || cli.quiet {
                // Plain/JSON/CSV mode
                run_client_plain(config, output_opts, cli.output).await?;
            } else {
                // TUI mode - load prefs with overrides
                // Priority: CLI flag > config file > saved prefs > default
                let prefs = xfr::prefs::Prefs::load()
                    .with_overrides(Some(&cli.theme), file_config.client.theme.as_deref());

                let final_prefs =
                    run_client_tui(config, cli.output, timestamp_format, prefs.clone()).await?;

                // Save prefs if changed
                if final_prefs.theme != prefs.theme {
                    let _ = final_prefs.save();
                }
            }
        }
    }

    Ok(())
}

async fn run_client_plain(
    config: ClientConfig,
    opts: OutputOptions,
    output: Option<PathBuf>,
) -> Result<()> {
    let client = Arc::new(Client::new(config.clone()));

    let (tx, mut rx) = mpsc::channel::<TestProgress>(100);

    // Print CSV header if needed
    if opts.csv && !opts.quiet {
        print!(
            "{}",
            xfr::output::csv::csv_interval_header(&opts.timestamp_format)
        );
        let _ = io::stdout().flush();
    }

    let omit_secs = opts.omit_secs;
    let quiet = opts.quiet;
    let json_stream = opts.json_stream;
    let csv = opts.csv;
    let interval_secs = opts.interval_secs;
    let timestamp_format = opts.timestamp_format;
    let test_start = std::time::Instant::now();
    let test_start_system = std::time::SystemTime::now();

    // Cumulative stats for fallback summary on signal interrupt.
    // TestProgress fields (total_bytes, streams[*].bytes) are per-interval deltas,
    // so we must accumulate totals across intervals ourselves.
    let cumulative_bytes: Arc<std::sync::atomic::AtomicU64> =
        Arc::new(std::sync::atomic::AtomicU64::new(0));
    let cumulative_elapsed_ms: Arc<std::sync::atomic::AtomicU64> =
        Arc::new(std::sync::atomic::AtomicU64::new(0));
    let cumulative_bytes_writer = cumulative_bytes.clone();
    let cumulative_elapsed_writer = cumulative_elapsed_ms.clone();

    // Print intervals in a separate task
    let print_handle = tokio::spawn(async move {
        let mut last_printed_interval: i64 = -1;
        let mut last_retransmits: u64 = 0;

        while let Some(progress) = rx.recv().await {
            // Accumulate cumulative totals for fallback summary
            cumulative_bytes_writer
                .fetch_add(progress.total_bytes, std::sync::atomic::Ordering::Relaxed);
            cumulative_elapsed_writer
                .store(progress.elapsed_ms, std::sync::atomic::Ordering::Relaxed);

            let elapsed_secs = progress.elapsed_ms as f64 / 1000.0;
            let retransmits = interval_retransmits(&progress, &mut last_retransmits);

            // Skip omitted seconds
            if elapsed_secs <= omit_secs as f64 {
                continue;
            }

            // Skip if quiet mode
            if quiet {
                continue;
            }

            // Only print at configured intervals (e.g., every 2 seconds if interval_secs=2.0)
            let current_interval = (elapsed_secs / interval_secs).floor() as i64;
            if current_interval <= last_printed_interval {
                continue;
            }
            last_printed_interval = current_interval;
            // Aggregate jitter (average) and lost (sum) across all streams
            let jitter_ms = {
                let jitters: Vec<f64> = progress
                    .streams
                    .iter()
                    .filter_map(|s| s.jitter_ms)
                    .collect();
                if jitters.is_empty() {
                    None
                } else {
                    Some(jitters.iter().sum::<f64>() / jitters.len() as f64)
                }
            };
            let lost = {
                let has_any = progress.streams.iter().any(|s| s.lost.is_some());
                if has_any {
                    Some(progress.streams.iter().filter_map(|s| s.lost).sum())
                } else {
                    None
                }
            };
            let rtt_us = progress.rtt_us;
            let cwnd = progress.cwnd;

            let now = std::time::Instant::now();
            let timestamp = timestamp_format.format(test_start, now, test_start_system);

            let interval_output = if json_stream {
                format!(
                    "{}\n",
                    xfr::output::json::output_interval_json(
                        &timestamp,
                        elapsed_secs,
                        progress.throughput_mbps,
                        progress.total_bytes,
                        retransmits,
                        jitter_ms,
                        lost,
                        rtt_us,
                        cwnd,
                    )
                )
            } else if csv {
                xfr::output::csv::output_interval_csv(
                    &timestamp,
                    elapsed_secs,
                    progress.throughput_mbps,
                    progress.total_bytes,
                    retransmits,
                    jitter_ms,
                    lost,
                    rtt_us,
                    cwnd,
                )
            } else {
                xfr::output::plain::output_interval_plain(
                    &timestamp,
                    elapsed_secs,
                    progress.throughput_mbps,
                    progress.total_bytes,
                    retransmits,
                    jitter_ms,
                    lost,
                    rtt_us,
                )
            };
            print!("{}", interval_output);
            let _ = io::stdout().flush();
        }
    });

    // Spawn the test as a task so we can select between it and Ctrl+C
    let client_run = client.clone();
    let mut test_handle = tokio::spawn(async move { client_run.run(Some(tx)).await });

    // Signal handler: first Ctrl+C = graceful cancel, second = force exit
    let sigint = Arc::new(tokio::sync::Notify::new());
    let sigint_bg = sigint.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        sigint_bg.notify_one();
        // Second Ctrl+C = force exit
        let _ = tokio::signal::ctrl_c().await;
        std::process::exit(130);
    });

    // Wait for either test completion or Ctrl+C
    let (result, interrupted) = tokio::select! {
        r = &mut test_handle => {
            (r??, false)
        }
        _ = sigint.notified() => {
            eprintln!("\nInterrupted, waiting for summary...");
            let _ = client.cancel();
            // Wait for the test task to finish (client now waits for server Result after cancel)
            match tokio::time::timeout(Duration::from_secs(5), &mut test_handle).await {
                Ok(Ok(Ok(result))) => (result, false), // Got real server result after cancel
                Ok(Ok(Err(_))) | Ok(Err(_)) | Err(_) => {
                    // Server didn't respond or task failed — abort and build fallback
                    test_handle.abort();
                    let bytes = cumulative_bytes.load(std::sync::atomic::Ordering::Relaxed);
                    let elapsed_ms = cumulative_elapsed_ms.load(std::sync::atomic::Ordering::Relaxed);
                    if bytes == 0 {
                        eprintln!("Test interrupted before any data was collected.");
                        std::process::exit(130);
                    }
                    (build_fallback_result(bytes, elapsed_ms), true)
                }
            }
        }
    };

    // Wait for print task to finish (test task is done or aborted, so tx is dropped)
    let _ = print_handle.await;

    // Output result
    if interrupted {
        eprintln!("Warning: partial results (server did not respond to cancel)");
    }
    let output_str = if opts.json || opts.json_stream {
        output_json(&result)
    } else if opts.csv {
        output_csv(&result)
    } else {
        output_plain(&result, config.mptcp)
    };

    println!("{}", output_str);

    // Don't save incomplete fallback results to file — only save real server results
    if !interrupted && let Some(path) = output {
        xfr::output::json::save_json(&result, &path)?;
        info!("Results saved to {}", path.display());
    }

    if interrupted {
        std::process::exit(130);
    }

    Ok(())
}

/// Build a fallback TestResult from cumulative counters.
/// Used when the server doesn't respond after cancel.
/// Only provides aggregate totals — no per-stream breakdown is available.
fn build_fallback_result(cumulative_bytes: u64, elapsed_ms: u64) -> xfr::protocol::TestResult {
    use xfr::protocol::TestResult;

    let throughput_mbps = if elapsed_ms > 0 {
        (cumulative_bytes as f64 * 8.0) / (elapsed_ms as f64 / 1000.0) / 1_000_000.0
    } else {
        0.0
    };

    TestResult {
        id: String::new(),
        bytes_total: cumulative_bytes,
        duration_ms: elapsed_ms,
        throughput_mbps,
        streams: vec![],
        tcp_info: None,
        udp_stats: None,
    }
}

async fn run_client_tui(
    config: ClientConfig,
    output: Option<PathBuf>,
    timestamp_format: TimestampFormat,
    prefs: xfr::prefs::Prefs,
) -> Result<xfr::prefs::Prefs> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_tui_loop(&mut terminal, config.clone(), timestamp_format, prefs).await;

    // Restore terminal
    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;

    match result {
        Ok((test_result, final_prefs, print_json)) => {
            if let Some(ref test_result) = test_result {
                // Print JSON if requested via 'j' key
                if print_json {
                    println!("{}", output_json(test_result));
                } else {
                    println!("{}", output_plain(test_result, config.mptcp));
                }

                if let Some(path) = output {
                    xfr::output::json::save_json(test_result, &path)?;
                    println!("Results saved to {}", path.display());
                }
            } else {
                println!("Test cancelled.");
            }
            Ok(final_prefs)
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            Err(e)
        }
    }
}

async fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: ClientConfig,
    timestamp_format: TimestampFormat,
    mut prefs: xfr::prefs::Prefs,
) -> Result<(Option<xfr::protocol::TestResult>, xfr::prefs::Prefs, bool)> {
    // Spawn background update check
    let (update_tx, update_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = xfr::update::check_for_update();
        let _ = update_tx.send(result);
    });

    let mut print_json_on_exit = false;
    let mut app = App::with_theme_name(
        config.host.clone(),
        config.port,
        config.protocol,
        config.direction,
        config.streams,
        config.duration,
        config.bitrate,
        timestamp_format,
        prefs.theme_name(),
    );

    // Track if we've received the update check result
    let mut update_check_done = false;

    let client = Arc::new(Client::new(config));
    let client_for_task = client.clone();
    let (progress_tx, mut progress_rx) = mpsc::channel::<TestProgress>(100);

    // Start the test
    let test_handle = tokio::spawn(async move { client_for_task.run(Some(progress_tx)).await });

    app.on_connected();

    // Track cancel state: when user presses 'q' or Ctrl+C during a running test,
    // we cancel and wait briefly for the server Result before exiting.
    let mut cancel_deadline: Option<std::time::Instant> = None;

    loop {
        // If we're waiting for cancel result and deadline expired, exit now
        if let Some(deadline) = cancel_deadline
            && deadline.elapsed() > Duration::ZERO
        {
            return Ok((app.result, prefs, print_json_on_exit));
        }

        // Check for update result if not already received
        if !update_check_done {
            match update_rx.try_recv() {
                Ok(Some(version)) => {
                    app.update_available = Some(version);
                    update_check_done = true;
                }
                Ok(None) => {
                    // No update available
                    update_check_done = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Still checking, will try again next loop
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Channel closed, stop checking
                    update_check_done = true;
                }
            }
        }

        // Draw UI
        terminal.draw(|f| draw(f, &app))?;

        // Handle events with timeout
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            // Settings modal takes priority when visible
            if app.settings.visible {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('s') => {
                        app.settings.close();
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.settings.move_up();
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.settings.move_down();
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        app.settings.value_prev();
                        // Apply theme change immediately
                        app.set_theme_index(app.settings.theme_index);
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        app.settings.value_next();
                        // Apply theme change immediately
                        app.set_theme_index(app.settings.theme_index);
                    }
                    KeyCode::Tab => {
                        app.settings.switch_tab();
                    }
                    KeyCode::Enter => {
                        let action = app.settings.on_enter();
                        match action {
                            SettingsAction::Restart => {
                                // Cancel current test and signal restart
                                let _ = client.cancel();
                                prefs.theme = Some(app.theme_name().to_string());
                                // TODO: Return restart signal with new params
                                // For now, just close - restart requires refactoring
                                app.log("Settings changed. Restart not yet implemented.");
                            }
                            SettingsAction::Close => {}
                            SettingsAction::None => {}
                        }
                    }
                    _ => {}
                }
                continue;
            }

            // Ctrl+C or 'q': cancel and wait for server Result
            let is_quit = key.code == KeyCode::Char('q')
                || (key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL));
            if is_quit {
                if app.state == AppState::Completed || cancel_deadline.is_some() {
                    // Already completed or already cancelling — exit immediately
                    prefs.theme = Some(app.theme_name().to_string());
                    return Ok((app.result, prefs, print_json_on_exit));
                }
                let _ = client.cancel();
                cancel_deadline = Some(std::time::Instant::now() + Duration::from_secs(3));
                app.log("Cancelling, waiting for summary...");
                continue;
            }

            match key.code {
                KeyCode::Char('p') => {
                    match client.pause() {
                        PauseResult::Applied => {
                            app.toggle_pause();
                        }
                        PauseResult::Unsupported => {
                            // UI-only: server doesn't support pause/resume
                            match app.state {
                                AppState::Running => {
                                    app.state = AppState::Paused;
                                    app.log(
                                        "Display paused (server does not support pause/resume)",
                                    );
                                }
                                AppState::Paused => {
                                    app.state = AppState::Running;
                                    app.log(
                                        "Display resumed (server does not support pause/resume)",
                                    );
                                }
                                _ => {}
                            }
                        }
                        PauseResult::NotReady => {
                            // Test not running yet or already finished — ignore
                        }
                    }
                }
                KeyCode::Char('t') => {
                    app.cycle_theme();
                }
                KeyCode::Char('s') => {
                    app.settings.toggle();
                }
                KeyCode::Char('?') | KeyCode::F(1) => {
                    app.toggle_help();
                }
                KeyCode::Char('d') => {
                    app.toggle_streams();
                }
                KeyCode::Esc if app.show_help => {
                    app.show_help = false;
                }
                KeyCode::Char('j') if app.result.is_some() => {
                    // Set flag to print JSON after TUI closes
                    print_json_on_exit = true;
                    app.log("JSON output queued for display on exit.");
                }
                KeyCode::Char('u') if app.update_available.is_some() => {
                    // Dismiss update notification
                    app.update_available = None;
                }
                _ => {}
            }
        }

        // Check for progress updates
        while let Ok(progress) = progress_rx.try_recv() {
            app.on_progress(progress);
        }

        // Check if test completed
        if test_handle.is_finished() {
            match test_handle.await? {
                Ok(result) => {
                    app.on_result(result);

                    // If we were waiting for cancel to complete, exit now with result
                    if cancel_deadline.is_some() {
                        prefs.theme = Some(app.theme_name().to_string());
                        return Ok((app.result, prefs, print_json_on_exit));
                    }

                    // Show final result for a moment
                    terminal.draw(|f| draw(f, &app))?;

                    // Wait for quit
                    loop {
                        // Check for update result if not already received
                        if !update_check_done {
                            match update_rx.try_recv() {
                                Ok(Some(version)) => {
                                    app.update_available = Some(version);
                                    update_check_done = true;
                                }
                                Ok(None) => update_check_done = true,
                                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                    update_check_done = true;
                                }
                            }
                        }

                        terminal.draw(|f| draw(f, &app))?;

                        if event::poll(Duration::from_millis(100))?
                            && let Event::Key(key) = event::read()?
                            && key.kind == KeyEventKind::Press
                        {
                            // Settings modal handling
                            if app.settings.visible {
                                match key.code {
                                    KeyCode::Esc | KeyCode::Char('s') => {
                                        app.settings.close();
                                    }
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        app.settings.move_up();
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        app.settings.move_down();
                                    }
                                    KeyCode::Left | KeyCode::Char('h') => {
                                        app.settings.value_prev();
                                        app.set_theme_index(app.settings.theme_index);
                                    }
                                    KeyCode::Right | KeyCode::Char('l') => {
                                        app.settings.value_next();
                                        app.set_theme_index(app.settings.theme_index);
                                    }
                                    KeyCode::Tab => {
                                        app.settings.switch_tab();
                                    }
                                    KeyCode::Enter => {
                                        let _ = app.settings.on_enter();
                                    }
                                    _ => {}
                                }
                                continue;
                            }

                            // Ctrl+C in completed state: exit
                            if key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                prefs.theme = Some(app.theme_name().to_string());
                                return Ok((app.result, prefs, print_json_on_exit));
                            }

                            match key.code {
                                KeyCode::Char('q') => {
                                    prefs.theme = Some(app.theme_name().to_string());
                                    return Ok((app.result, prefs, print_json_on_exit));
                                }
                                KeyCode::Esc => {
                                    if app.show_help {
                                        app.show_help = false;
                                    } else {
                                        prefs.theme = Some(app.theme_name().to_string());
                                        return Ok((app.result, prefs, print_json_on_exit));
                                    }
                                }
                                KeyCode::Char('t') => {
                                    app.cycle_theme();
                                }
                                KeyCode::Char('s') => {
                                    app.settings.toggle();
                                }
                                KeyCode::Char('?') | KeyCode::F(1) => {
                                    app.toggle_help();
                                }
                                KeyCode::Char('d') => {
                                    app.toggle_streams();
                                }
                                KeyCode::Char('j') => {
                                    print_json_on_exit = true;
                                    app.log("JSON output queued for display on exit.");
                                }
                                KeyCode::Char('u') if app.update_available.is_some() => {
                                    app.update_available = None;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Err(e) => {
                    // If we were waiting for cancel to complete, just exit
                    if cancel_deadline.is_some() {
                        prefs.theme = Some(app.theme_name().to_string());
                        return Ok((app.result, prefs, print_json_on_exit));
                    }

                    app.on_error(e.to_string());
                    terminal.draw(|f| draw(f, &app))?;

                    // Wait for quit
                    loop {
                        if event::poll(Duration::from_millis(100))?
                            && let Event::Key(key) = event::read()?
                            && key.kind == KeyEventKind::Press
                            && (key.code == KeyCode::Char('q')
                                || (key.code == KeyCode::Char('c')
                                    && key.modifiers.contains(KeyModifiers::CONTROL)))
                        {
                            prefs.theme = Some(app.theme_name().to_string());
                            return Err(anyhow::anyhow!(app.error.unwrap_or_default()));
                        }
                    }
                }
            }
        }
    }
}

async fn run_server_tui(mut config: ServerConfig) -> Result<()> {
    // Create channel for server events
    let (tx, mut rx) = mpsc::channel::<ServerEvent>(100);
    config.tui_tx = Some(tx);

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let server = Server::new(config);
    let server_handle = tokio::spawn(async move { server.run().await });

    let mut app = ServerApp::new();

    loop {
        // Draw UI
        terminal.draw(|f| server_draw(f, &app))?;

        // Handle keyboard events with timeout
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') => {
                    break;
                }
                KeyCode::Char('?') | KeyCode::F(1) => {
                    app.toggle_help();
                }
                KeyCode::Esc if app.show_help => {
                    app.show_help = false;
                }
                _ => {}
            }
        }

        // Process server events
        while let Ok(event) = rx.try_recv() {
            match event {
                ServerEvent::TestStarted(info) => {
                    app.add_test(info);
                }
                ServerEvent::TestUpdated {
                    id,
                    bytes,
                    throughput_mbps,
                } => {
                    app.update_test(&id, bytes, throughput_mbps);
                }
                ServerEvent::TestCompleted { id, bytes } => {
                    app.remove_test(&id, bytes);
                }
                ServerEvent::ConnectionBlocked => {
                    app.record_blocked();
                }
                ServerEvent::AuthFailure => {
                    app.record_auth_failure();
                }
            }
        }

        // Update bandwidth history
        app.update_bandwidth();

        // Check if server task ended
        if server_handle.is_finished() {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bitrate_basic() {
        assert_eq!(parse_bitrate("100").unwrap(), 100);
        assert_eq!(parse_bitrate("100K").unwrap(), 100_000);
        assert_eq!(parse_bitrate("100M").unwrap(), 100_000_000);
        assert_eq!(parse_bitrate("1G").unwrap(), 1_000_000_000);
    }

    #[test]
    fn test_parse_bitrate_case_insensitive() {
        assert_eq!(parse_bitrate("100k").unwrap(), 100_000);
        assert_eq!(parse_bitrate("100m").unwrap(), 100_000_000);
        assert_eq!(parse_bitrate("1g").unwrap(), 1_000_000_000);
    }

    #[test]
    fn test_parse_bitrate_overflow() {
        // This should overflow u64
        assert!(parse_bitrate("999999999999999999999G").is_err());
    }

    #[test]
    fn test_parse_size_basic() {
        assert_eq!(parse_size("100").unwrap(), 100);
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn test_parse_size_overflow() {
        // This should overflow usize
        assert!(parse_size("999999999999999999999G").is_err());
    }

    #[test]
    fn test_parse_test_duration_minimum() {
        // 1 second should work
        assert!(parse_test_duration("1s").is_ok());
        assert!(parse_test_duration("10s").is_ok());

        // 0 for infinite should work
        assert!(parse_test_duration("0s").is_ok());
        assert_eq!(parse_test_duration("0s").unwrap(), Duration::ZERO);

        // Sub-second (but not zero) should fail
        assert!(parse_test_duration("500ms").is_err());
    }

    #[test]
    fn test_cli_conflicts() {
        use clap::Parser;

        // --quic and --udp should conflict
        let result = Cli::try_parse_from(["xfr", "host", "--quic", "--udp"]);
        assert!(result.is_err());

        // --bidir and --reverse should conflict
        let result = Cli::try_parse_from(["xfr", "host", "--bidir", "--reverse"]);
        assert!(result.is_err());

        // --json and --csv should conflict
        let result = Cli::try_parse_from(["xfr", "host", "--json", "--csv"]);
        assert!(result.is_err());

        // --random and --zeros should conflict
        let result = Cli::try_parse_from(["xfr", "host", "--random", "--zeros"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_parallel_range() {
        use clap::Parser;

        // -P 0 should fail
        let result = Cli::try_parse_from(["xfr", "host", "-P", "0"]);
        assert!(result.is_err());

        // -P 1 should work
        let result = Cli::try_parse_from(["xfr", "host", "-P", "1"]);
        assert!(result.is_ok());

        // -P 128 should work
        let result = Cli::try_parse_from(["xfr", "host", "-P", "128"]);
        assert!(result.is_ok());

        // -P 129 should fail
        let result = Cli::try_parse_from(["xfr", "host", "-P", "129"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_payload_defaults() {
        use clap::Parser;

        let cli = Cli::try_parse_from(["xfr", "host"]).unwrap();
        assert!(effective_random_payload(&cli));

        let cli = Cli::try_parse_from(["xfr", "host", "--random"]).unwrap();
        assert!(effective_random_payload(&cli));

        let cli = Cli::try_parse_from(["xfr", "host", "--zeros"]).unwrap();
        assert!(!effective_random_payload(&cli));
    }

    #[test]
    fn test_validate_bind_address_tcp_explicit_port() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        use xfr::protocol::Protocol;

        // TCP + explicit port should fail (control + data conflict)
        let addr = Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            5000,
        ));
        let result = validate_bind_address(addr, Protocol::Tcp, 1, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("TCP"));

        // TCP + port 0 (OS-assigned) should work
        let addr = Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            0,
        ));
        let result = validate_bind_address(addr, Protocol::Tcp, 1, false);
        assert!(result.is_ok());

        // TCP + no bind should work
        let result = validate_bind_address(None, Protocol::Tcp, 4, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_bind_address_udp_multi_stream() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        use xfr::protocol::Protocol;

        // UDP + explicit port + multiple streams should fail
        let addr = Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            5000,
        ));
        let result = validate_bind_address(addr, Protocol::Udp, 4, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("multiple streams"));

        // UDP + explicit port + single stream should work
        let result = validate_bind_address(addr, Protocol::Udp, 1, false);
        assert!(result.is_ok());

        // UDP + port 0 + multiple streams should work
        let addr = Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            0,
        ));
        let result = validate_bind_address(addr, Protocol::Udp, 4, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_bind_address_quic_multi_stream() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        use xfr::protocol::Protocol;

        // QUIC + explicit port + multiple streams - currently rejected (stricter than necessary)
        let addr = Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            5000,
        ));
        let result = validate_bind_address(addr, Protocol::Quic, 4, false);
        assert!(result.is_err());

        // QUIC + explicit port + single stream should work
        let result = validate_bind_address(addr, Protocol::Quic, 1, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_bind_address_cport() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        use xfr::protocol::Protocol;

        let addr = Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 5300));

        // --cport + TCP single stream → ok
        let result = validate_bind_address(addr, Protocol::Tcp, 1, true);
        assert!(result.is_ok());

        // --cport + TCP multi-stream → ok (sequential ports)
        let result = validate_bind_address(addr, Protocol::Tcp, 4, true);
        assert!(result.is_ok());

        // --cport + UDP single stream → ok
        let result = validate_bind_address(addr, Protocol::Udp, 1, true);
        assert!(result.is_ok());

        // --cport + UDP multi-stream → ok (sequential ports)
        let result = validate_bind_address(addr, Protocol::Udp, 4, true);
        assert!(result.is_ok());

        // --cport + QUIC single stream → ok
        let result = validate_bind_address(addr, Protocol::Quic, 1, true);
        assert!(result.is_ok());

        // --cport + QUIC multi-stream → ok (QUIC multiplexes on one socket)
        let result = validate_bind_address(addr, Protocol::Quic, 4, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_bind_address() {
        // IPv4 without port
        let addr = parse_bind_address("192.168.1.1").unwrap();
        assert_eq!(addr.ip().to_string(), "192.168.1.1");
        assert_eq!(addr.port(), 0);

        // IPv4 with port
        let addr = parse_bind_address("192.168.1.1:5000").unwrap();
        assert_eq!(addr.ip().to_string(), "192.168.1.1");
        assert_eq!(addr.port(), 5000);

        // IPv6 without brackets or port
        let addr = parse_bind_address("::1").unwrap();
        assert_eq!(addr.ip().to_string(), "::1");
        assert_eq!(addr.port(), 0);

        // IPv6 with brackets, no port
        let addr = parse_bind_address("[::1]").unwrap();
        assert_eq!(addr.ip().to_string(), "::1");
        assert_eq!(addr.port(), 0);

        // IPv6 with brackets and port
        let addr = parse_bind_address("[::1]:5000").unwrap();
        assert_eq!(addr.ip().to_string(), "::1");
        assert_eq!(addr.port(), 5000);

        // Invalid address should fail
        assert!(parse_bind_address("invalid").is_err());
        assert!(parse_bind_address("[invalid]").is_err());
    }

    #[test]
    fn test_random_payload_notice() {
        // Default (random enabled): no warnings for TCP/UDP in any direction
        assert_eq!(
            random_payload_notice(Protocol::Tcp, Direction::Upload, true),
            None
        );
        assert_eq!(
            random_payload_notice(Protocol::Tcp, Direction::Download, true),
            None
        );
        assert_eq!(
            random_payload_notice(Protocol::Tcp, Direction::Bidir, true),
            None
        );
        assert_eq!(
            random_payload_notice(Protocol::Udp, Direction::Upload, true),
            None
        );
        assert_eq!(
            random_payload_notice(Protocol::Udp, Direction::Download, true),
            None
        );
        assert_eq!(
            random_payload_notice(Protocol::Udp, Direction::Bidir, true),
            None
        );

        // QUIC + random: payload is already encrypted
        assert_eq!(
            random_payload_notice(Protocol::Quic, Direction::Upload, true),
            Some("random payloads are ignored with QUIC (payload is already encrypted)")
        );
        assert!(random_payload_notice(Protocol::Quic, Direction::Download, true).is_some());

        // --zeros + upload: no warning (client controls its own sends)
        assert_eq!(
            random_payload_notice(Protocol::Tcp, Direction::Upload, false),
            None
        );
        assert_eq!(
            random_payload_notice(Protocol::Udp, Direction::Upload, false),
            None
        );

        // --zeros + reverse/bidir: warn that server still sends random
        assert!(random_payload_notice(Protocol::Tcp, Direction::Download, false).is_some());
        assert!(random_payload_notice(Protocol::Tcp, Direction::Bidir, false).is_some());
        assert!(random_payload_notice(Protocol::Udp, Direction::Download, false).is_some());
        assert!(random_payload_notice(Protocol::Udp, Direction::Bidir, false).is_some());

        // --zeros + QUIC reverse: no warning (QUIC encrypts regardless)
        assert_eq!(
            random_payload_notice(Protocol::Quic, Direction::Download, false),
            None
        );
    }

    fn make_progress(
        total_retransmits: Option<u64>,
        stream_retransmits: &[Option<u64>],
    ) -> TestProgress {
        TestProgress {
            elapsed_ms: 1000,
            total_bytes: 0,
            throughput_mbps: 0.0,
            streams: stream_retransmits
                .iter()
                .enumerate()
                .map(|(i, retransmits)| xfr::protocol::StreamInterval {
                    id: i as u8,
                    bytes: 0,
                    retransmits: *retransmits,
                    jitter_ms: None,
                    lost: None,
                    error: None,
                    rtt_us: None,
                    cwnd: None,
                })
                .collect(),
            rtt_us: None,
            cwnd: None,
            total_retransmits,
        }
    }

    #[test]
    fn test_interval_retransmits_uses_sender_side_deltas() {
        let mut last = 0;
        let first = make_progress(Some(5), &[]);
        let second = make_progress(Some(9), &[]);

        assert_eq!(interval_retransmits(&first, &mut last), Some(5));
        assert_eq!(last, 5);
        assert_eq!(interval_retransmits(&second, &mut last), Some(4));
        assert_eq!(last, 9);
    }

    #[test]
    fn test_interval_retransmits_uses_server_stream_deltas() {
        let mut last = 99;
        let progress = make_progress(None, &[Some(2), None, Some(3)]);

        assert_eq!(interval_retransmits(&progress, &mut last), Some(5));
        // Receiver-side mode does not use last_retransmits at all.
        assert_eq!(last, 99);
    }

    #[test]
    fn test_interval_retransmits_none_when_unavailable() {
        let mut last = 7;
        let progress = make_progress(None, &[None, None]);

        assert_eq!(interval_retransmits(&progress, &mut last), None);
        assert_eq!(last, 7);
    }

    #[test]
    fn test_parse_dscp_numeric() {
        assert_eq!(parse_dscp("0").unwrap(), 0);
        assert_eq!(parse_dscp("46").unwrap(), 46);
        assert_eq!(parse_dscp("255").unwrap(), 255);
        assert!(parse_dscp("256").is_err());
    }

    #[test]
    fn test_parse_dscp_names() {
        // CS values: DSCP << 2
        assert_eq!(parse_dscp("CS0").unwrap(), 0);
        assert_eq!(parse_dscp("CS1").unwrap(), 32);
        assert_eq!(parse_dscp("CS7").unwrap(), 224);
        // AF values
        assert_eq!(parse_dscp("AF11").unwrap(), 40); // DSCP 10 << 2
        assert_eq!(parse_dscp("AF43").unwrap(), 152); // DSCP 38 << 2
        // EF
        assert_eq!(parse_dscp("EF").unwrap(), 184); // DSCP 46 << 2
        // VA (VOICE-ADMIT)
        assert_eq!(parse_dscp("VA").unwrap(), 172); // DSCP 44 << 2
        // Case insensitive
        assert_eq!(parse_dscp("ef").unwrap(), 184);
        assert_eq!(parse_dscp("af21").unwrap(), 72);
        // Unknown name
        assert!(parse_dscp("BOGUS").is_err());
    }
}
