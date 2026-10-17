//! xfr - Modern network bandwidth testing with TUI

use std::io::{self, IsTerminal, Write};
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
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use xfr::client::{Client, ClientConfig, TestProgress, ZerocopyMode};
use xfr::config::Config;
use xfr::diff::{DEFAULT_DIFF_THRESHOLD_PERCENT, DiffConfig, run_diff};
use xfr::net::AddressFamily;
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
use xfr::tui::program_status::{StatusReporter, client_status};
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

    /// Test duration. Bare integers are seconds (`-t 10`), or use unit suffixes (`10s`, `1min`, `500ms`). Use 0 for infinite.
    /// Defaults to 10s unless `--probe-mtu` is used without `-t`, in which case it defaults to 60s.
    #[arg(short = 't', long, value_parser = parse_test_duration, env = "XFR_DURATION")]
    #[arg(help_heading = "Test Options")]
    time: Option<Duration>,

    /// Transfer this many bytes, then stop, instead of running for a fixed time
    /// (`-n 1G`). Binary suffixes: K=KiB, M=MiB, G=GiB. The total is shared across
    /// all streams. Without an explicit `-t` the test runs until the transfer
    /// finishes; with one, `-t` becomes an upper bound. Requires a server that
    /// advertises `byte_budget_v1`.
    #[arg(short = 'n', long = "bytes", value_parser = parse_byte_budget, env = "XFR_BYTES", conflicts_with = "probe_mtu")]
    #[arg(help_heading = "Test Options")]
    bytes: Option<u64>,

    /// UDP mode
    #[arg(short = 'u', long, conflicts_with = "quic")]
    #[arg(help_heading = "Test Options")]
    udp: bool,

    /// UDP payload size in bytes, including the 16-byte xfr header (default: 1400).
    /// Use a smaller size on VPN/tunnel paths; requires server udp_payload_size_v1 support.
    #[arg(short = 'l', long = "length", value_name = "BYTES", requires = "udp", conflicts_with_all = ["probe_mtu", "quic", "mptcp", "zerocopy"], value_parser = clap::value_parser!(u16).range(16..=65507))]
    #[arg(help_heading = "Test Options")]
    udp_payload_size: Option<u16>,

    /// QUIC mode (encrypted, multiplexed streams)
    #[arg(short = 'Q', long, conflicts_with = "udp")]
    #[arg(help_heading = "Test Options")]
    quic: bool,

    /// Target bitrate (e.g., 1G, 100M). Applies to TCP and UDP. 0 = unlimited.
    #[arg(short = 'b', long, value_parser = parse_bitrate)]
    #[arg(help_heading = "Test Options")]
    bitrate: Option<u64>,

    /// Number of parallel streams (1-128; default: config value or 1)
    #[arg(short = 'P', long, value_parser = clap::value_parser!(u8).range(1..=128))]
    #[arg(help_heading = "Test Options")]
    parallel: Option<u8>,

    /// Reverse direction (server sends to client)
    #[arg(short = 'R', long, conflicts_with = "bidir")]
    #[arg(help_heading = "Test Options")]
    reverse: bool,

    /// Bidirectional test
    #[arg(long, conflicts_with = "reverse")]
    #[arg(help_heading = "Test Options")]
    bidir: bool,

    /// JSON output
    #[arg(long, conflicts_with_all = ["csv", "json_stream"])]
    #[arg(help_heading = "Output Options")]
    json: bool,

    /// JSON streaming output (one object per line)
    #[arg(long, conflicts_with_all = ["csv", "json"])]
    #[arg(help_heading = "Output Options")]
    json_stream: bool,

    /// CSV output
    #[arg(long, conflicts_with_all = ["json", "json_stream"])]
    #[arg(help_heading = "Output Options")]
    csv: bool,

    /// Quiet mode - suppress interval output, show only summary
    #[arg(short = 'q', long)]
    #[arg(help_heading = "Output Options")]
    quiet: bool,

    /// Output file
    #[arg(short = 'o', long)]
    #[arg(help_heading = "Output Options")]
    output: Option<PathBuf>,

    /// Disable TUI
    #[arg(long)]
    #[arg(help_heading = "Output Options")]
    no_tui: bool,

    /// Disable the background check for a newer xfr release (also honored via
    /// the DO_NOT_TRACK or XFR_NO_UPDATE_CHECK environment variables)
    #[arg(long)]
    no_update_check: bool,

    /// Color theme (default, kawaii, cyber, dracula, monochrome, matrix, nord, gruvbox, catppuccin, tokyo_night, solarized)
    #[arg(long, global = true)]
    #[arg(help_heading = "Output Options")]
    theme: Option<String>,

    /// Report interval in positive, finite seconds (default: 1.0)
    #[arg(short = 'i', long, value_parser = parse_report_interval)]
    #[arg(help_heading = "Output Options")]
    interval: Option<f64>,

    /// Omit first N seconds from interval output (TCP ramp-up)
    #[arg(long)]
    #[arg(help_heading = "Output Options")]
    omit: Option<u64>,

    /// Server/client port
    #[arg(short, long, default_value_t = DEFAULT_PORT, env = "XFR_PORT")]
    #[arg(help_heading = "Connection Options")]
    port: u16,

    /// Disable Nagle algorithm
    #[arg(long)]
    #[arg(help_heading = "Connection Options")]
    tcp_nodelay: bool,

    /// TCP congestion control algorithm (e.g. cubic, bbr, reno)
    #[arg(long = "congestion", value_name = "ALGO")]
    #[arg(help_heading = "Connection Options")]
    congestion: Option<String>,

    /// DSCP/TOS marking: raw TOS byte (0-255) or DSCP name (EF, AF11, CS1, etc.)
    #[arg(long, value_name = "VALUE")]
    #[arg(help_heading = "Connection Options")]
    dscp: Option<String>,

    /// Socket buffer size for TCP (SO_SNDBUF/SO_RCVBUF) and UDP
    /// (SO_SNDBUF/SO_RCVBUF on both ends), e.g., 512K, 1M, 16M. When
    /// unset, TCP uses kernel autotuning and UDP uses the kernel default.
    #[arg(short = 'w', long, value_parser = parse_size)]
    #[arg(help_heading = "Connection Options")]
    window: Option<usize>,

    /// Timestamp format for interval output (relative, iso8601, unix)
    #[arg(long, value_parser = parse_timestamp_format, env = "XFR_TIMESTAMP_FORMAT")]
    #[arg(help_heading = "Output Options")]
    timestamp_format: Option<TimestampFormat>,

    /// Log file path (e.g., "~/.config/xfr/xfr.log")
    #[arg(long, env = "XFR_LOG_FILE")]
    #[arg(help_heading = "Output Options")]
    log_file: Option<String>,

    /// Log level (error, warn, info, debug, trace)
    #[arg(long, env = "XFR_LOG_LEVEL")]
    #[arg(help_heading = "Output Options")]
    log_level: Option<String>,

    /// Pre-shared key for authentication.
    ///
    /// WARNING: values supplied via `--psk` or the `XFR_PSK` environment
    /// variable are visible to other users through process metadata such as
    /// `ps`, shell history, and `/proc/<pid>/environ`. Prefer `--psk-file`.
    #[arg(long, env = "XFR_PSK")]
    #[arg(help_heading = "Authentication")]
    psk: Option<String>,

    /// Read PSK from file
    #[arg(long)]
    #[arg(help_heading = "Authentication")]
    psk_file: Option<PathBuf>,

    /// Force IPv4 only
    #[arg(short = '4', long = "ipv4", conflicts_with = "ipv6_only")]
    #[arg(help_heading = "Connection Options")]
    ipv4_only: bool,

    /// Force IPv6 only
    #[arg(short = '6', long = "ipv6")]
    #[arg(help_heading = "Connection Options")]
    ipv6_only: bool,

    /// Local address to bind to (e.g., 192.168.1.100 or 192.168.1.100:0)
    #[arg(long, value_name = "ADDR")]
    #[arg(help_heading = "Connection Options")]
    bind: Option<String>,

    /// Client source port for firewall traversal (UDP/QUIC/TCP data streams)
    #[arg(long, value_name = "PORT")]
    #[arg(help_heading = "Connection Options")]
    cport: Option<u16>,

    /// MPTCP mode (Multi-Path TCP, Linux 5.6+)
    #[arg(long, conflicts_with_all = ["udp", "quic"])]
    #[arg(help_heading = "Connection Options")]
    mptcp: bool,

    /// Use random payload data (default for TCP/UDP client-sent traffic)
    #[arg(long, conflicts_with = "zeros")]
    #[arg(help_heading = "Test Options")]
    random: bool,

    /// Use zero-filled payload data instead of random bytes
    #[arg(long, conflicts_with = "random")]
    #[arg(help_heading = "Test Options")]
    zeros: bool,

    /// Zero-copy TCP sends via sendfile(2), like iperf3 -Z (Linux only;
    /// lowers sender CPU overhead). On by default for TCP; passing -Z
    /// explicitly warns when zero-copy cannot take effect instead of
    /// silently falling back to regular writes.
    #[arg(short = 'Z', long, conflicts_with_all = ["udp", "quic"])]
    #[arg(help_heading = "Test Options")]
    zerocopy: bool,

    /// Disable zero-copy TCP sends (use regular buffered writes)
    #[arg(long, conflicts_with = "zerocopy")]
    #[arg(help_heading = "Test Options")]
    no_zerocopy: bool,

    /// Probe the path MTU instead of running a throughput test: walks
    /// UDP payload sizes with the don't-fragment flag set and reports
    /// the largest size surviving in each direction (issue #64).
    /// Implies UDP (a redundant -u is accepted); requires a server with
    /// mtu_probe_v1 support.
    #[arg(
        long,
        conflicts_with_all = ["quic", "reverse", "bidir", "bitrate", "parallel", "zerocopy", "zeros"]
    )]
    #[arg(help_heading = "Test Options")]
    probe_mtu: bool,

    /// Fail if the control connection (TCP connect / QUIC handshake)
    /// takes longer than this (e.g. 5s). Without it, a dead or filtered
    /// server is bounded only by OS defaults, which can be minutes.
    #[arg(long, value_parser = parse_nonzero_duration, value_name = "DURATION")]
    #[arg(help_heading = "Connection Options")]
    connect_timeout: Option<Duration>,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Start server mode
    Serve {
        /// Exit after one test
        #[arg(long)]
        #[arg(help_heading = "Server Options")]
        one_off: bool,

        /// Enable TUI dashboard
        #[arg(long)]
        #[arg(help_heading = "Server Options")]
        tui: bool,

        /// Maximum test duration (server-side limit). Bare integers are seconds (`60`); unit suffixes also accepted (`60s`, `5min`).
        #[arg(long, value_parser = parse_duration)]
        #[arg(help_heading = "Server Options")]
        max_duration: Option<Duration>,

        /// Use a named server preset from the config file.
        ///
        /// Preset `allowed_clients` (if any) become an additional allowlist
        /// enforced with the normal ACL. Preset `max_duration_secs` (if any)
        /// is applied only when `--max-duration` is not provided.
        /// Unknown presets are rejected.
        #[arg(long)]
        #[arg(help_heading = "Server Options")]
        preset: Option<String>,

        /// Prometheus metrics port
        #[cfg(feature = "prometheus")]
        #[arg(long)]
        #[arg(help_heading = "Server Options")]
        prometheus: Option<u16>,

        /// Prometheus push gateway URL (e.g., http://pushgateway:9091)
        #[arg(long, env = "XFR_PUSH_GATEWAY")]
        #[arg(help_heading = "Server Options")]
        push_gateway: Option<String>,

        /// Disable mDNS service registration
        #[arg(long)]
        #[arg(help_heading = "Server Options")]
        no_mdns: bool,

        /// Server port (default: config value or 5201)
        #[arg(short, long, env = "XFR_PORT")]
        #[arg(help_heading = "Connection Options")]
        port: Option<u16>,

        /// Bind to specific address (e.g., 192.168.1.1, ::1)
        #[arg(short = 'B', long)]
        #[arg(help_heading = "Connection Options")]
        bind: Option<IpAddr>,

        /// Force IPv4 only
        #[arg(short = '4', long = "ipv4", conflicts_with = "ipv6_only")]
        #[arg(help_heading = "Connection Options")]
        ipv4_only: bool,

        /// Force IPv6 only
        #[arg(short = '6', long = "ipv6")]
        #[arg(help_heading = "Connection Options")]
        ipv6_only: bool,

        /// Max concurrent tests per IP (rate limiting)
        #[arg(long)]
        #[arg(help_heading = "Access Control")]
        rate_limit: Option<u32>,

        /// Rate limit time window in whole seconds (default: 60s). Bare integers are seconds (`60`); unit suffixes also accepted (`60s`, `5min`). Must be at least 1 second.
        #[arg(long, value_parser = parse_rate_limit_window)]
        #[arg(help_heading = "Access Control")]
        rate_limit_window: Option<u64>,

        /// Allow IP/subnet (can be repeated)
        #[arg(long = "allow", action = clap::ArgAction::Append)]
        #[arg(help_heading = "Access Control")]
        allow: Vec<String>,

        /// Deny IP/subnet (can be repeated)
        #[arg(long = "deny", action = clap::ArgAction::Append)]
        #[arg(help_heading = "Access Control")]
        deny: Vec<String>,

        /// ACL rules file
        #[arg(long)]
        #[arg(help_heading = "Access Control")]
        acl_file: Option<PathBuf>,

        /// Pre-shared key for authentication.
        ///
        /// WARNING: values supplied via `--psk` or the `XFR_PSK` environment
        /// variable are visible to other users through process metadata such as
        /// `ps`, shell history, and `/proc/<pid>/environ`. Prefer `--psk-file`.
        #[arg(long, env = "XFR_PSK")]
        #[arg(help_heading = "Authentication")]
        psk: Option<String>,

        /// Read PSK from file
        #[arg(long)]
        #[arg(help_heading = "Authentication")]
        psk_file: Option<PathBuf>,

        /// Log file path (e.g., "~/.config/xfr/xfr.log")
        #[arg(long, env = "XFR_LOG_FILE")]
        #[arg(help_heading = "Output Options")]
        log_file: Option<String>,

        /// Log level (error, warn, info, debug, trace)
        #[arg(long, env = "XFR_LOG_LEVEL")]
        #[arg(help_heading = "Output Options")]
        log_level: Option<String>,
    },

    /// Compare two test results
    Diff {
        /// Baseline result file
        baseline: PathBuf,

        /// Current result file
        current: PathBuf,

        /// Regression threshold percentage (finite and non-negative)
        #[arg(long, default_value_t = DEFAULT_DIFF_THRESHOLD_PERCENT, value_parser = parse_diff_threshold)]
        threshold: f64,
    },

    /// Discover xfr servers on LAN
    Discover {
        /// Discovery timeout. Bare integers are seconds (`5`); unit suffixes also accepted (`5s`, `500ms`).
        #[arg(long, default_value = "5s", value_parser = parse_duration)]
        timeout: Duration,
    },
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    // Accept bare integers as seconds (iperf-style muscle memory: `-t 10`).
    // Fall back to humantime for unit-suffixed strings like "10s", "1min".
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }
    humantime::parse_duration(s).map_err(|e| e.to_string())
}

/// Duration parser for flags where zero is not a valid sentinel.
fn parse_nonzero_duration(s: &str) -> Result<Duration, String> {
    let d = parse_duration(s)?;
    if d.is_zero() {
        return Err("Duration must be greater than 0".to_string());
    }
    Ok(d)
}

fn parse_rate_limit_window(s: &str) -> Result<u64, String> {
    let duration = parse_nonzero_duration(s)?;
    if duration.subsec_nanos() != 0 {
        return Err(
            "Rate limit window must be a whole number of seconds (at least 1 second)".into(),
        );
    }
    Ok(duration.as_secs())
}

/// Resolve the rate limit window without confusing an explicit CLI value
/// with a default. The config and public rate limiter API use whole seconds.
fn resolve_rate_limit_window(
    cli_window_secs: Option<u64>,
    config_window_secs: Option<u64>,
) -> anyhow::Result<u64> {
    let window_secs = cli_window_secs
        .or(config_window_secs)
        .unwrap_or_else(|| xfr::rate_limit::RateLimitConfig::default().window_secs);
    if window_secs == 0 {
        anyhow::bail!("server.rate_limit_window must be at least 1 second");
    }
    Ok(window_secs)
}

fn parse_test_duration(s: &str) -> Result<Duration, String> {
    let duration = parse_duration(s)?;
    // Allow 0 for infinite duration, otherwise require at least 1 second
    if duration == Duration::ZERO {
        return Ok(Duration::ZERO);
    }
    if duration < Duration::from_secs(1) {
        return Err("Minimum test duration is 1 second (use 0 for infinite)".to_string());
    }
    if duration.as_secs() > u64::from(u32::MAX) {
        return Err(format!(
            "Maximum test duration is {} seconds (use 0 for infinite)",
            u32::MAX
        ));
    }
    Ok(duration)
}

fn validate_report_interval(seconds: f64) -> Result<f64, String> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err("Report interval must be finite and greater than zero".into());
    }
    // Stall detection scales the reporting period by 1.9; validate that
    // duration too so a finite but enormous setting cannot panic later.
    if Duration::try_from_secs_f64(seconds.max(1.0) * 1.9).is_err() {
        return Err("Report interval is too large".into());
    }
    Ok(seconds)
}

fn parse_report_interval(value: &str) -> Result<f64, String> {
    validate_report_interval(value.parse::<f64>().map_err(|error| error.to_string())?)
}

/// Resolve the effective server-side max test duration.
/// CLI `--max-duration` always wins; otherwise a preset's
/// `max_duration_secs` (if present) supplies the default.
fn resolve_server_max_duration(
    cli_max_duration: Option<Duration>,
    preset: Option<&xfr::config::ServerPreset>,
) -> Option<Duration> {
    cli_max_duration.or_else(|| preset.and_then(|p| p.max_duration_secs.map(Duration::from_secs)))
}

/// An explicit CLI or environment port wins even when it equals DEFAULT_PORT.
fn resolve_server_port(cli_port: Option<u16>, config_port: Option<u16>) -> u16 {
    cli_port.or(config_port).unwrap_or(DEFAULT_PORT)
}

/// Resolve the effective client test duration.
/// - CLI `-t`/`--time` always wins when present.
/// - `--probe-mtu` without an explicit duration extends to 60s.
/// - Otherwise fall back to the config file, then the 10s default.
fn resolve_client_duration(
    cli_time: Option<Duration>,
    probe_mtu: bool,
    byte_budget: Option<u64>,
    file_config: &xfr::config::Config,
) -> Duration {
    if let Some(duration) = cli_time {
        duration
    } else if probe_mtu {
        Duration::from_secs(60)
    } else if byte_budget.is_some() {
        // `-n` without `-t`: the transfer size is the stopping condition, so
        // there is no clock. Falling back to the 10s default would truncate
        // any transfer that takes longer than that on a slow link — the exact
        // thing `-n` exists to avoid. An explicit `-t` still caps the run.
        Duration::ZERO
    } else {
        file_config
            .client
            .duration_secs
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(10))
    }
}

fn resolve_parallel_streams(cli_parallel: Option<u8>, config_parallel: Option<u8>) -> Result<u8> {
    let streams = cli_parallel.or(config_parallel).unwrap_or(1);
    anyhow::ensure!(
        (1..=128).contains(&streams),
        "client.parallel_streams must be between 1 and 128 (got {streams})"
    );
    Ok(streams)
}

fn resolve_window_size(
    cli_window: Option<usize>,
    config_window: Option<&str>,
) -> Result<Option<usize>> {
    if let Some(window) = cli_window {
        return Ok(Some(window));
    }
    config_window
        .map(|value| {
            parse_size(value)
                .map_err(|error| anyhow::anyhow!("invalid client.window_size {value:?}: {error}"))
        })
        .transpose()
}

#[derive(Debug, PartialEq)]
struct ResolvedClientNetworkOptions {
    bitrate: Option<u64>,
    congestion: Option<String>,
    dscp: Option<String>,
    interval_secs: f64,
    bind: Option<String>,
    cport: Option<u16>,
}

/// Resolve client transport and output defaults with CLI values taking precedence.
fn resolve_client_network_options(
    cli_bitrate: Option<u64>,
    cli_congestion: Option<String>,
    cli_dscp: Option<String>,
    cli_interval: Option<f64>,
    cli_bind: Option<String>,
    cli_cport: Option<u16>,
    defaults: &xfr::config::ClientDefaults,
) -> Result<ResolvedClientNetworkOptions, String> {
    let bitrate = match cli_bitrate {
        Some(bitrate) => Some(bitrate),
        None => defaults
            .bitrate
            .as_deref()
            .map(parse_bitrate)
            .transpose()
            .map_err(|e| format!("bitrate: {e}"))?,
    };

    Ok(ResolvedClientNetworkOptions {
        bitrate,
        congestion: cli_congestion.or_else(|| defaults.congestion.clone()),
        dscp: cli_dscp.or_else(|| defaults.dscp.clone()),
        interval_secs: validate_report_interval(
            cli_interval.or(defaults.interval_secs).unwrap_or(1.0),
        )
        .map_err(|e| format!("interval_secs: {e}"))?,
        bind: cli_bind.or_else(|| defaults.bind.clone()),
        cport: cli_cport.or(defaults.cport),
    })
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

/// Parse a `-n/--bytes` byte count. Binary suffixes (K=KiB, M=MiB, G=GiB),
/// matching `-w/--window` and iperf3's `-n`. Zero is rejected: a test that
/// transfers nothing has no meaning, and silently treating it as "unlimited"
/// (the way `-b 0` works) would be a trap.
fn parse_byte_budget(s: &str) -> Result<u64, String> {
    let upper = s.to_uppercase();
    let (num, suffix) = if let Some(rest) = upper.strip_suffix('G') {
        (rest, 1024 * 1024 * 1024u64)
    } else if let Some(rest) = upper.strip_suffix('M') {
        (rest, 1024 * 1024u64)
    } else if let Some(rest) = upper.strip_suffix('K') {
        (rest, 1024u64)
    } else {
        (upper.as_str(), 1u64)
    };

    let n: u64 = num
        .parse()
        .map_err(|_| format!("Invalid byte count: {}", s))?;
    let total = n
        .checked_mul(suffix)
        .ok_or_else(|| format!("Byte count overflow: {}", s))?;
    if total == 0 {
        return Err("Byte count must be greater than zero".to_string());
    }
    Ok(total)
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

fn resolve_address_family(
    ipv4_only: bool,
    ipv6_only: bool,
    config_value: Option<&str>,
    config_key: &str,
) -> Result<AddressFamily> {
    if ipv4_only {
        return Ok(AddressFamily::V4Only);
    }
    if ipv6_only {
        return Ok(AddressFamily::V6Only);
    }
    match config_value {
        Some(value) => value.parse().map_err(|()| {
            anyhow::anyhow!("invalid {config_key}: {value:?}; expected ipv4, ipv6, or dual")
        }),
        None => Ok(AddressFamily::default()),
    }
}

fn resolve_server_address_family(
    bind: Option<IpAddr>,
    ipv4_only: bool,
    ipv6_only: bool,
    config_value: Option<&str>,
) -> Result<AddressFamily> {
    // A specific bind address selects the family before config defaults.
    if let Some(ip) = bind {
        if ip.is_unspecified() {
            anyhow::bail!(
                "--bind {} is an unspecified address; use -4/-6 instead to control address family",
                ip
            );
        }
        if ipv4_only && ip.is_ipv6() {
            anyhow::bail!("--bind {} is IPv6 but -4/--ipv4 forces IPv4", ip);
        }
        if ipv6_only && ip.is_ipv4() {
            anyhow::bail!("--bind {} is IPv4 but -6/--ipv6 forces IPv6", ip);
        }
        return Ok(if ip.is_ipv4() {
            AddressFamily::V4Only
        } else {
            AddressFamily::V6Only
        });
    }
    resolve_address_family(ipv4_only, ipv6_only, config_value, "server.address_family")
}

/// Parse a bind address string (can be "IP" or "IP:port")
fn parse_bind_address(s: &str) -> anyhow::Result<std::net::SocketAddr> {
    use std::net::SocketAddr;

    // Try parsing as full socket address first (IP:port or [IPv6]:port)
    if let Ok(addr) = s.parse::<SocketAddr>() {
        return Ok(addr);
    }

    // IP only (port 0 = auto-assign), [IPv6]:port with a zone name, or an
    // IPv6 zone on a link-local address (fe80::1%eth0).
    let (host, port) = match s.strip_prefix('[').and_then(|r| r.split_once("]:")) {
        Some((host, port)) => (
            host,
            port.parse::<u16>()
                .map_err(|_| anyhow::anyhow!("Invalid bind port in {}", s))?,
        ),
        None => (s, 0),
    };
    if let Some(addr) = xfr::net::parse_ip_literal(host, port)? {
        return Ok(addr);
    }

    anyhow::bail!(
        "Invalid bind address: {}. Use IP or IP:port format \
         (for IPv6: [::1], [::1]:port, or fe80::1%eth0 for link-local).",
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

fn cache_udp_direction(
    cache: &mut Option<xfr::protocol::UdpDirectionProgress>,
    update: Option<xfr::protocol::UdpDirectionProgress>,
) -> Option<xfr::protocol::UdpDirectionProgress> {
    let mut update = update?;
    if cache
        .as_ref()
        .is_some_and(|previous| !update.progress.is_at_least_as_fresh_as(&previous.progress))
    {
        return None;
    }
    // Upload feedback carries counts without jitter. Keep the last observed
    // jitter while accepting a fresher gap repair from that sideband.
    update.jitter_ms = update
        .jitter_ms
        .or_else(|| cache.as_ref().and_then(|previous| previous.jitter_ms));
    *cache = Some(update);
    Some(update)
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

/// Resolve the zero-copy mode from CLI flags. Zero-copy is on by default
/// for TCP (issue #33 follow-up); an explicit -Z upgrades silent fallback
/// to a warning, and --no-zerocopy opts out. This is the TCP preference,
/// not scoped to the startup protocol: the TUI can switch protocols, and
/// non-TCP runs never use it (the client gates it on TCP, and clap already
/// rejects an explicit -Z with -u/-Q).
fn effective_zerocopy(cli: &Cli) -> ZerocopyMode {
    debug_assert!(!(cli.zerocopy && cli.no_zerocopy));
    if cli.no_zerocopy {
        ZerocopyMode::Off
    } else if cli.zerocopy {
        ZerocopyMode::Requested
    } else {
        ZerocopyMode::Auto
    }
}

/// Warn when zero-copy cannot take effect for client-sent traffic on this
/// platform. Only an explicit -Z warns — the default-on mode falls back
/// silently. Download mode is exempt: the client sends no bulk data there,
/// and the (possibly Linux) server decides its own zero-copy support.
fn zerocopy_notice(zerocopy: ZerocopyMode, direction: Direction) -> Option<&'static str> {
    if zerocopy != ZerocopyMode::Requested || cfg!(target_os = "linux") {
        return None;
    }
    match direction {
        Direction::Download => None,
        Direction::Upload | Direction::Bidir => Some(
            "--zerocopy requires Linux sendfile(2); client-sent traffic will use regular writes",
        ),
    }
}

fn parse_diff_threshold(value: &str) -> Result<f64, String> {
    let threshold = value.parse::<f64>().map_err(|error| error.to_string())?;
    xfr::diff::DiffConfig {
        threshold_percent: threshold,
    }
    .validate()
    .map_err(|error| error.to_string())?;
    Ok(threshold)
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

    // A missing config is fine, but an existing config must be usable. Silently
    // dropping settings such as PSKs, ACLs, or rate limits is unsafe.
    let file_config = Config::load()?;

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
            mut max_duration,
            #[cfg(feature = "prometheus")]
            prometheus,
            push_gateway,
            log_file: _serve_log_file,
            log_level: _serve_log_level,
            psk,
            psk_file,
            preset,
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
            let server_port = resolve_server_port(port, file_config.server.port);

            let server_one_off = one_off || file_config.server.one_off.unwrap_or(false);

            #[cfg(feature = "prometheus")]
            let prom_port = prometheus.or(file_config.server.prometheus_port);

            let push_gateway_url = push_gateway.or_else(|| file_config.server.push_gateway.clone());

            // Build PSK (CLI > file > config)
            let effective_psk = if let Some(psk_path) = psk_file {
                Some(xfr::auth::read_psk_file(&psk_path)?)
            } else {
                psk.as_ref()
                    .cloned()
                    .or_else(|| file_config.server.psk.clone())
            };

            // Validate PSK if provided
            if let Some(ref psk_value) = effective_psk {
                xfr::auth::validate_psk(psk_value)?;
            }

            // PSK supplied on the command line or through the environment leaks
            // through process metadata; warn so users can migrate to --psk-file.
            if psk.is_some() {
                warn!(
                    "Using --psk or XFR_PSK exposes the pre-shared key in process metadata (ps, /proc/<pid>/environ, shell history); prefer --psk-file"
                );
            }

            // Resolve server preset
            let preset = if let Some(name) = preset {
                Some(
                    file_config
                        .get_preset(&name)
                        .ok_or_else(|| anyhow::anyhow!("Unknown server preset: {name}"))?
                        .clone(),
                )
            } else {
                None
            };

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
                window_secs: resolve_rate_limit_window(
                    rate_limit_window,
                    file_config.server.rate_limit_window,
                )?,
            };

            let address_family = resolve_server_address_family(
                bind,
                ipv4_only,
                ipv6_only,
                file_config.server.address_family.as_deref(),
            )?;

            let server_no_mdns = no_mdns || file_config.server.no_mdns.unwrap_or(false);

            max_duration = resolve_server_max_duration(max_duration, preset.as_ref());

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
                preset_allowed_clients: preset.and_then(|p| p.allowed_clients),
                ..Default::default()
            };

            if server_tui {
                // Same precedence as the client TUI: --theme > saved pref >
                // NO_COLOR-derived default. There is no server section for a
                // theme in config.toml, so the saved pref carries a user's
                // choice across from the client.
                let prefs = xfr::prefs::Prefs::load().with_overrides(cli.theme.as_deref(), None);
                run_server_tui(config, prefs.theme_name()).await?;
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
            } else if cli.udp || cli.probe_mtu {
                Protocol::Udp
            } else {
                Protocol::Tcp
            };

            let random_payload = effective_random_payload(&cli);
            let zerocopy = effective_zerocopy(&cli);
            let network_options = resolve_client_network_options(
                cli.bitrate,
                cli.congestion.clone(),
                cli.dscp.clone(),
                cli.interval,
                cli.bind.clone(),
                cli.cport,
                &file_config.client,
            )
            .map_err(|e| anyhow::anyhow!("invalid [client] {e}"))?;

            if let Some(msg) = random_payload_notice(protocol, direction, random_payload) {
                eprintln!("Warning: {msg}");
            }

            if let Some(msg) = zerocopy_notice(zerocopy, direction) {
                eprintln!("Warning: {msg}");
            }

            if network_options.dscp.is_some() && protocol == xfr::protocol::Protocol::Quic {
                eprintln!("Warning: --dscp is ignored with QUIC (QUIC manages its own socket)");
            }

            // The QUIC paths don't implement these knobs (the server-side
            // handler drops them from TestStart); say so instead of letting
            // a rate-limited or buffer-tuned test silently run untuned.
            if protocol == xfr::protocol::Protocol::Quic {
                for (set, msg) in [
                    (
                        cli.window.is_some(),
                        "-w/--window is ignored with QUIC (QUIC flow control manages buffering)",
                    ),
                    (
                        network_options.congestion.is_some(),
                        "--congestion is ignored with QUIC (kernel TCP CC does not apply)",
                    ),
                    (
                        cli.tcp_nodelay,
                        "--tcp-nodelay is ignored with QUIC (no Nagle on UDP transport)",
                    ),
                ] {
                    if set {
                        eprintln!("Warning: {msg}");
                    }
                }
            }

            #[cfg(not(unix))]
            if network_options.dscp.is_some() && protocol != xfr::protocol::Protocol::Quic {
                eprintln!("Warning: --dscp is not supported on this platform");
            }

            if cli.mptcp {
                xfr::net::validate_mptcp().map_err(|e| anyhow::anyhow!("{}", e))?;
            }

            // Apply config file defaults where CLI didn't override.
            // `--probe-mtu` without an explicit `-t` uses a longer default
            // because the duration is only a server-side safety deadline.
            let duration =
                resolve_client_duration(cli.time, cli.probe_mtu, cli.bytes, &file_config);

            let streams =
                resolve_parallel_streams(cli.parallel, file_config.client.parallel_streams)?;

            let tcp_nodelay = cli.tcp_nodelay || file_config.client.tcp_nodelay.unwrap_or(false);

            let window_size =
                resolve_window_size(cli.window, file_config.client.window_size.as_deref())?;

            // An explicit --json-stream or --csv overrides a config-file JSON
            // default; otherwise the summary-JSON mode would suppress the
            // interval rows the user asked to stream.
            let json_output = cli.json
                || (!cli.json_stream
                    && !cli.csv
                    && file_config.client.json_output.unwrap_or(false));
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

            // UDP transfers whole datagrams, so a `-n` smaller than one packet
            // header can't be framed at all and the test would move zero bytes
            // without saying why.
            if protocol == Protocol::Udp
                && let Some(budget) = cli.bytes
                && budget < xfr::udp::UDP_HEADER_SIZE as u64
            {
                warn!(
                    "-n {} is smaller than one UDP packet header ({} bytes); UDP sends whole datagrams, so this test will transfer nothing",
                    budget,
                    xfr::udp::UDP_HEADER_SIZE
                );
            }

            // PSK supplied on the command line or through the environment leaks
            // through process metadata; warn so users can migrate to --psk-file.
            if cli.psk.is_some() {
                warn!(
                    "Using --psk or XFR_PSK exposes the pre-shared key in process metadata (ps, /proc/<pid>/environ, shell history); prefer --psk-file"
                );
            }

            // Determine address family
            let client_address_family = resolve_address_family(
                cli.ipv4_only,
                cli.ipv6_only,
                file_config.client.address_family.as_deref(),
                "client.address_family",
            )?;

            // Parse bind address (can be "IP" or "IP:port")
            let mut bind_addr = if let Some(ref bind_str) = network_options.bind {
                Some(parse_bind_address(bind_str)?)
            } else {
                None
            };

            // Merge --cport into bind_addr
            let cport = network_options.cport;
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
                    bind_addr = Some(xfr::net::with_port(*addr, port));
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
                bitrate: network_options.bitrate,
                tcp_nodelay,
                tcp_congestion: network_options.congestion,
                window_size,
                byte_budget: cli.bytes,
                udp_payload_size: cli.udp_payload_size,
                psk: client_psk,
                address_family: client_address_family,
                bind_addr,
                sequential_ports: protocol != Protocol::Quic && cport.is_some() && streams > 1,
                mptcp: cli.mptcp,
                random_payload,
                zerocopy,
                mtu_probe: cli.probe_mtu,
                connect_timeout: cli.connect_timeout,
                dscp: network_options
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
                interval_secs: network_options.interval_secs,
                timestamp_format,
            };

            // Probe mode always uses plain output: there is no live
            // throughput to graph, the run lasts seconds, and the
            // deliverable is the per-size table.
            if no_tui || json_output || cli.json_stream || cli.csv || cli.quiet || cli.probe_mtu {
                // Plain/JSON/CSV mode
                run_client_plain(config, output_opts, cli.output).await?;
            } else {
                // TUI mode - load prefs with overrides
                // Priority: CLI flag > config file > saved prefs > default
                let prefs = xfr::prefs::Prefs::load()
                    .with_overrides(cli.theme.as_deref(), file_config.client.theme.as_deref());

                // Update check runs unless disabled. Precedence (higher wins):
                // compiled-out > env > CLI flag > config.toml > saved pref.
                let env_opt_out = xfr::update::env_opt_out();
                let update_disabled = xfr::update::check_disabled(
                    env_opt_out,
                    cli.no_update_check,
                    file_config.client.no_update_check,
                    prefs.disable_update_check,
                );
                let update_lock_reason = xfr::update::check_lock_reason(
                    env_opt_out,
                    cli.no_update_check,
                    file_config.client.no_update_check,
                );

                let final_prefs = run_client_tui(
                    config,
                    cli.output,
                    timestamp_format,
                    prefs.clone(),
                    update_disabled,
                    update_lock_reason,
                )
                .await?;

                // Save prefs if changed
                if final_prefs.theme != prefs.theme
                    || final_prefs.disable_update_check != prefs.disable_update_check
                {
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
    // Summary JSON is one document; interval and feedback rows belong to
    // the streaming modes and must not contaminate redirected JSON output.
    let quiet = opts.quiet || opts.json;
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
        // Cache the freshest cumulative UDP loss snapshot from either
        // source (TCP control `Interval.aggregate.udp_progress` or a
        // feedback-only update from the udp_feedback_v1 path). The
        // producer-side filter on `Client::run_test` ensures whichever
        // path provides this is monotonic, so we always cache the
        // latest cumulative value the receiver has reported. On the
        // next full-interval print we derive `lost` as a delta from
        // the previously printed cumulative, which keeps the output
        // line accurate even when feedback arrived between TCP
        // intervals (or when intervals were skipped under control-
        // channel stall, since Skip on the server interval timer can
        // drop a tick while feedback continues to flow over UDP).
        let mut cached_udp_progress: Option<xfr::protocol::UdpIntervalProgress> = None;
        let mut cached_udp_send = None;
        let mut cached_udp_recv = None;
        let mut last_printed_cumulative_lost: u64 = 0;
        // Stall detection for scripted output (issue #70 follow-up, the
        // non-TUI half of issue #93): full-interval rows print on control
        // `Interval` arrival, which stalls under upload saturation. Track
        // when the last real row printed so feedback-only updates can keep
        // loss visible in the meantime.
        let mut last_row_at = std::time::Instant::now();
        let mut last_feedback_row_at = std::time::Instant::now();
        let row_period = std::time::Duration::from_secs_f64(interval_secs.max(1.0));

        while let Some(mut progress) = rx.recv().await {
            progress.udp_send = cache_udp_direction(&mut cached_udp_send, progress.udp_send);
            progress.udp_recv = cache_udp_direction(&mut cached_udp_recv, progress.udp_recv);
            if !progress.udp_feedback_only {
                // Full interval rows use the freshest quality from either
                // channel, including receive-only feedback between reports.
                progress.udp_send = cached_udp_send;
                progress.udp_recv = cached_udp_recv;
            }
            // Producers can enqueue in a different order from their freshness
            // checks. Keep a late TCP/UDP snapshot from undoing a gap repair.
            if let Some(p) = progress.udp_progress
                && cached_udp_progress.is_none_or(|previous| p.is_at_least_as_fresh_as(&previous))
            {
                cached_udp_progress = Some(p);
            }
            // Feedback-only updates have no bytes/throughput to print, and
            // the next full interval will pick up the cached cumulative we
            // just recorded above. But when full intervals have *stalled*
            // (upload saturation clogging the control channel — issue #70
            // follow-up, the scripted half of issue #93), these 1 Hz
            // locally-clocked updates are the only live signal, so emit an
            // explicit feedback event carrying what they actually know:
            // cumulative loss. Plain text prints a stall line; --json-stream
            // emits a marked `udp_feedback` event. CSV keeps its fixed
            // columns and stays interval-only. The printed-loss baseline is
            // NOT advanced, so the next real interval still reports its
            // full delta window.
            if progress.udp_feedback_only {
                let directional = progress.udp_send.is_some() || progress.udp_recv.is_some();
                if !quiet && !csv && (directional || cached_udp_progress.is_some()) {
                    let now = std::time::Instant::now();
                    let elapsed_secs = progress.elapsed_ms as f64 / 1000.0;
                    let stalled = now.duration_since(last_row_at) >= row_period.mul_f64(1.9)
                        && now.duration_since(last_feedback_row_at) >= row_period.mul_f64(0.9)
                        && elapsed_secs > omit_secs as f64;
                    if stalled {
                        let timestamp = timestamp_format.format(test_start, now, test_start_system);
                        let line = if json_stream {
                            let serialized = if directional {
                                xfr::output::json::output_directional_feedback_json(
                                    &timestamp,
                                    elapsed_secs,
                                    progress.udp_send.as_ref(),
                                    progress.udp_recv.as_ref(),
                                )
                            } else {
                                let cum = cached_udp_progress.expect("feedback presence checked");
                                xfr::output::json::output_feedback_json(
                                    &timestamp,
                                    elapsed_secs,
                                    cum.packets_received,
                                    cum.packets_lost,
                                    cum.lost_percent(),
                                )
                            };
                            match serialized {
                                Ok(line) => format!("{}\n", line),
                                Err(e) => {
                                    tracing::error!("Failed to serialize feedback to JSON: {}", e);
                                    continue;
                                }
                            }
                        } else if directional {
                            xfr::output::plain::output_directional_feedback_plain(
                                &timestamp,
                                elapsed_secs,
                                progress.udp_send.as_ref(),
                                progress.udp_recv.as_ref(),
                            )
                        } else {
                            let cum = cached_udp_progress.expect("feedback presence checked");
                            xfr::output::plain::output_feedback_plain(
                                &timestamp,
                                cum.packets_lost,
                                cum.lost_percent(),
                            )
                        };
                        print!("{}", line);
                        let _ = io::stdout().flush();
                        last_feedback_row_at = now;
                    }
                }
                continue;
            }
            // Accumulate cumulative totals for fallback summary
            cumulative_bytes_writer
                .fetch_add(progress.total_bytes, std::sync::atomic::Ordering::Relaxed);
            cumulative_elapsed_writer
                .store(progress.elapsed_ms, std::sync::atomic::Ordering::Relaxed);

            let elapsed_secs = progress.elapsed_ms as f64 / 1000.0;
            let retransmits = interval_retransmits(&progress, &mut last_retransmits);

            // Skip omitted seconds. Advance the cumulative-loss baseline
            // before continuing so loss accumulated during the omit
            // ramp-up doesn't fold into the first printed line as a
            // single jumbo delta — that would defeat the purpose of
            // --omit (suppressing early-test ramp-up artifacts).
            // Intentionally NOT applied to --interval or --quiet skips:
            // those preserve the new "loss between printed lines"
            // semantic so the next line, when it does print, reflects
            // every lost packet since the prior printed line.
            if elapsed_secs <= omit_secs as f64 {
                if let Some(cum) = cached_udp_progress {
                    last_printed_cumulative_lost = cum.packets_lost;
                }
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
            // Prefer the cumulative `udp_progress` cache when available
            // — it folds in any feedback updates received since the last
            // print, which keeps `lost` current under control-channel
            // stalls. Falls back to the per-stream sum for sessions
            // where udp_progress is never sent (paired with a pre-0.9.11
            // server, or non-UDP tests).
            let lost = if let Some(cum) = cached_udp_progress {
                let delta = cum
                    .packets_lost
                    .saturating_sub(last_printed_cumulative_lost);
                last_printed_cumulative_lost = cum.packets_lost;
                Some(delta)
            } else {
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
                match xfr::output::json::output_interval_json_with_udp_directions(
                    &timestamp,
                    elapsed_secs,
                    progress.throughput_mbps,
                    progress.total_bytes,
                    retransmits,
                    jitter_ms,
                    lost,
                    rtt_us,
                    cwnd,
                    progress.udp_send.as_ref(),
                    progress.udp_recv.as_ref(),
                ) {
                    Ok(line) => format!("{}\n", line),
                    Err(e) => {
                        tracing::error!("Failed to serialize interval to JSON: {}", e);
                        continue;
                    }
                }
            } else if csv {
                xfr::output::csv::output_interval_csv_with_udp_directions(
                    &timestamp,
                    elapsed_secs,
                    progress.throughput_mbps,
                    progress.total_bytes,
                    retransmits,
                    jitter_ms,
                    lost,
                    rtt_us,
                    cwnd,
                    progress.bytes_sent,
                    progress.bytes_received,
                    progress.throughput_send_mbps,
                    progress.throughput_recv_mbps,
                    progress.udp_send.as_ref(),
                    progress.udp_recv.as_ref(),
                )
            } else {
                xfr::output::plain::output_interval_plain_with_udp_directions(
                    &timestamp,
                    elapsed_secs,
                    progress.throughput_mbps,
                    progress.total_bytes,
                    retransmits,
                    jitter_ms,
                    lost,
                    rtt_us,
                    progress.udp_send.as_ref(),
                    progress.udp_recv.as_ref(),
                )
            };
            print!("{}", interval_output);
            let _ = io::stdout().flush();
            last_row_at = std::time::Instant::now();
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
    let output_str = if opts.json_stream {
        serde_json::to_string(&result)?
    } else if opts.json {
        output_json(&result)?
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
        udp_stats_send: None,
        udp_stats_recv: None,
        id: String::new(),
        bytes_total: cumulative_bytes,
        duration_ms: elapsed_ms,
        throughput_mbps,
        streams: vec![],
        tcp_info: None,
        udp_stats: None,
        bytes_sent: None,
        bytes_received: None,
        throughput_send_mbps: None,
        throughput_recv_mbps: None,
        mtu_probe: None,
        server_udp_send_errors: None,
        client_udp_send_errors: None,
    }
}

/// RAII guard that restores raw mode and leaves the alternate screen on drop,
/// ensuring terminal recovery even if the TUI event loop panics or exits early.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

async fn run_client_tui(
    mut config: ClientConfig,
    output: Option<PathBuf>,
    timestamp_format: TimestampFormat,
    prefs: xfr::prefs::Prefs,
    update_disabled: bool,
    update_lock_reason: Option<&'static str>,
) -> Result<xfr::prefs::Prefs> {
    // Setup terminal. The guard is armed before entering the alternate screen
    // so a failure there still restores cooked mode on the way out.
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut status = StatusReporter::new(io::stdout().is_terminal());

    // The loop applies Settings edits to `config`, so afterwards it
    // describes the run that produced the final result.
    let result = run_tui_loop(
        &mut terminal,
        &mut status,
        &mut config,
        timestamp_format,
        prefs,
        update_disabled,
        update_lock_reason,
    )
    .await;
    status.finish(terminal.backend_mut());

    // Restore terminal
    drop(_guard);
    match result {
        Ok((test_result, final_prefs, print_json, partial)) => {
            if let Some(ref test_result) = test_result {
                if partial {
                    eprintln!("Warning: partial results (server did not respond to cancel)");
                }
                // Print JSON if requested via 'j' key
                if print_json {
                    println!("{}", output_json(test_result)?);
                } else {
                    let mptcp = tui_run_config(&config).mptcp;
                    println!("{}", output_plain(test_result, mptcp));
                }

                // Don't save incomplete fallback results to file — only real
                // server results (same rule as the --no-tui fallback path).
                if !partial && let Some(path) = output {
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

struct TuiRun {
    client: Arc<Client>,
    handle: tokio::task::JoinHandle<Result<xfr::protocol::TestResult>>,
    progress_rx: mpsc::Receiver<TestProgress>,
}

/// How long the TUI waits for the server's final Result after a quit before
/// giving up and showing a locally-built summary (issue #159). Matches the
/// --no-tui Ctrl+C fallback.
const TUI_CANCEL_WAIT: Duration = Duration::from_secs(5);

/// `(result, prefs, print_json_on_exit, partial)`; `partial` marks a
/// fallback summary built from local counters because the server never
/// answered the cancel.
type TuiLoopExit = (
    Option<xfr::protocol::TestResult>,
    xfr::prefs::Prefs,
    bool,
    bool,
);

/// Fallback summary for a quit the server never acknowledged, from locally
/// accumulated counters. `None` when nothing was transferred — there is no
/// summary worth showing.
fn tui_fallback_result(bytes: u64, elapsed_ms: u64) -> Option<xfr::protocol::TestResult> {
    (bytes > 0).then(|| build_fallback_result(bytes, elapsed_ms))
}

fn spawn_tui_run(config: &ClientConfig) -> TuiRun {
    let client = Arc::new(Client::new(tui_run_config(config)));
    let client_for_task = client.clone();
    let (progress_tx, progress_rx) = mpsc::channel::<TestProgress>(100);
    let handle = tokio::spawn(async move { client_for_task.run(Some(progress_tx)).await });
    TuiRun {
        client,
        handle,
        progress_rx,
    }
}

fn tui_run_config(config: &ClientConfig) -> ClientConfig {
    // Keep the user's transport-specific preferences in the settings
    // config so switching back restores them. Only the active run drops
    // options that do not apply to the newly selected protocol.
    let mut active = config.clone();
    // Fixed data ports need one socket per TCP/UDP stream. Recompute this
    // after either protocol or stream-count edits; QUIC uses one endpoint.
    active.sequential_ports = active.protocol != Protocol::Quic
        && active.streams > 1
        && active.bind_addr.is_some_and(|addr| addr.port() != 0);
    if active.protocol != Protocol::Udp {
        active.udp_payload_size = None;
    }
    if active.protocol != Protocol::Tcp {
        active.mptcp = false;
        active.zerocopy = ZerocopyMode::Off;
    }
    active
}

async fn finish_tui_run(run: TuiRun) -> Result<xfr::protocol::TestResult> {
    let TuiRun { handle, .. } = run;
    handle.await?
}

async fn cancel_tui_run(
    run: TuiRun,
    timeout: Duration,
) -> Option<Result<xfr::protocol::TestResult>> {
    let _ = run.client.cancel();
    let TuiRun { mut handle, .. } = run;
    match tokio::time::timeout(timeout, &mut handle).await {
        Ok(joined) => Some(joined.map_err(anyhow::Error::from).and_then(|inner| inner)),
        Err(_) => {
            handle.abort();
            let _ = handle.await;
            None
        }
    }
}

async fn abort_tui_run(run: TuiRun) {
    let TuiRun { handle, .. } = run;
    handle.abort();
    let _ = handle.await;
}

fn poll_update_check(
    app: &mut App,
    update_rx: &std::sync::mpsc::Receiver<Option<String>>,
    update_check_done: &mut bool,
) {
    if *update_check_done {
        return;
    }

    match update_rx.try_recv() {
        Ok(Some(version)) => {
            app.update_available = Some(version);
            *update_check_done = true;
        }
        Ok(None) => {
            *update_check_done = true;
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => {}
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            *update_check_done = true;
        }
    }
}

fn remember_tui_theme(prefs: &mut xfr::prefs::Prefs, app: &App) {
    if let Some(theme) = app.changed_theme_name() {
        prefs.theme = Some(theme.to_string());
    }
}

fn handle_tui_settings_key(app: &mut App, key: crossterm::event::KeyEvent) -> SettingsAction {
    match key.code {
        KeyCode::Esc | KeyCode::Char('s') => {
            app.settings.close();
            SettingsAction::Close
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.settings.move_up();
            SettingsAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.settings.move_down();
            SettingsAction::None
        }
        KeyCode::Left | KeyCode::Char('h') => {
            app.settings.value_prev();
            app.set_theme_index(app.settings.theme_index);
            app.set_timestamp_format(app.settings.timestamp_format);
            SettingsAction::None
        }
        KeyCode::Right | KeyCode::Char('l') => {
            app.settings.value_next();
            app.set_theme_index(app.settings.theme_index);
            app.set_timestamp_format(app.settings.timestamp_format);
            SettingsAction::None
        }
        KeyCode::Tab => {
            app.settings.switch_tab();
            SettingsAction::None
        }
        KeyCode::Enter => app.settings.on_enter(),
        _ => SettingsAction::None,
    }
}

fn apply_tui_settings(config: &mut ClientConfig, app: &mut App) {
    config.streams = app.settings.streams;
    config.protocol = app.settings.protocol;
    config.duration = Duration::from_secs(app.settings.duration_secs);
    config.direction = app.settings.direction;
    config.bitrate = app.settings.bitrate;

    app.apply_test_params(
        config.protocol,
        config.direction,
        config.streams,
        config.duration,
        config.bitrate,
    );
}

fn log_tui_quic_ignored_options(app: &mut App, config: &ClientConfig) {
    if config.protocol != Protocol::Quic {
        return;
    }

    for (set, msg) in [
        (
            config.window_size.is_some(),
            "-w/--window is ignored with QUIC (QUIC flow control manages buffering)",
        ),
        (
            config.tcp_congestion.is_some(),
            "--congestion is ignored with QUIC (kernel TCP CC does not apply)",
        ),
        (
            config.tcp_nodelay,
            "--tcp-nodelay is ignored with QUIC (no Nagle on UDP transport)",
        ),
        (
            config.dscp.is_some(),
            "--dscp is ignored with QUIC (QUIC manages its own socket)",
        ),
    ] {
        if set {
            app.log(format!("Warning: {msg}"));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TuiQuitAction {
    Noop,
    ExitOk,
    ExitError,
    StartCancel,
    ForceExit,
    Ignore,
}

fn is_ctrl_c(key: &crossterm::event::KeyEvent) -> bool {
    (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        || key.code == KeyCode::Char('\x03')
}

/// Settings owns every key while open, except interrupts, which must still
/// reach the normal cancel path.
fn settings_captures_key(settings_visible: bool, key: &crossterm::event::KeyEvent) -> bool {
    settings_visible && !is_ctrl_c(key)
}

fn server_tui_quit_key(key: &crossterm::event::KeyEvent) -> bool {
    key.code == KeyCode::Char('q') || is_ctrl_c(key)
}

fn tui_quit_action(
    key: &crossterm::event::KeyEvent,
    state: AppState,
    cancel_pending: bool,
    run_active: bool,
) -> TuiQuitAction {
    let is_ctrl_c = is_ctrl_c(key);
    let is_quit = key.code == KeyCode::Char('q') || is_ctrl_c;

    if !is_quit {
        return TuiQuitAction::Noop;
    }

    if state == AppState::Error {
        return TuiQuitAction::ExitError;
    }

    if state == AppState::Completed || !run_active {
        return TuiQuitAction::ExitOk;
    }

    if cancel_pending {
        if is_ctrl_c {
            TuiQuitAction::ForceExit
        } else {
            TuiQuitAction::Ignore
        }
    } else {
        TuiQuitAction::StartCancel
    }
}

async fn restart_tui_run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    run: &mut Option<TuiRun>,
    config: &ClientConfig,
) -> Result<()> {
    if let Some(current) = run.take() {
        app.log("Restarting, waiting for current test to stop...");
        terminal.draw(|f| draw(f, app))?;
        match cancel_tui_run(current, Duration::from_secs(3)).await {
            Some(Ok(_)) => {}
            Some(Err(e)) => {
                app.log(format!("Previous test ended while restarting: {}", e));
            }
            None => {
                app.log("Previous test did not stop in time; aborted.");
            }
        }
    }

    app.reset_for_restart();
    log_tui_quic_ignored_options(app, config);
    *run = Some(spawn_tui_run(config));
    app.on_connected();
    Ok(())
}

async fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    status: &mut StatusReporter,
    config: &mut ClientConfig,
    timestamp_format: TimestampFormat,
    mut prefs: xfr::prefs::Prefs,
    update_disabled: bool,
    update_lock_reason: Option<&'static str>,
) -> Result<TuiLoopExit> {
    // Spawn the background update check unless it's disabled (flag/env/config/
    // pref) or compiled out. `update_rx` still exists so the poller is a no-op.
    let (update_tx, update_rx) = std::sync::mpsc::channel();
    if !update_disabled {
        std::thread::spawn(move || {
            let _ = update_tx.send(xfr::update::check_for_update());
        });
    }

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
    // A `-n` run measures progress in bytes, not seconds.
    app.byte_budget = config.byte_budget;
    // Reflect the resolved update-check state in the Settings toggle.
    app.settings.update_check = !update_disabled;
    app.settings.update_check_lock_reason = update_lock_reason;

    // Track if we've received the update check result (start "done" when the
    // check is disabled so the poller never blocks on the empty channel).
    let mut update_check_done = update_disabled;

    log_tui_quic_ignored_options(&mut app, config);
    let mut run = Some(spawn_tui_run(config));
    app.on_connected();

    // Cumulative counters mirroring the --no-tui fallback path: if the server
    // never answers a cancel (issue #159), these still let us show a summary.
    let mut fallback_bytes: u64 = 0;
    let mut fallback_elapsed_ms: u64 = 0;

    // Cancel state lives in `app.cancel_deadline` so the footer can render the
    // remaining wait: when user presses 'q' or Ctrl+C during a running test,
    // we cancel and wait briefly for the server Result before exiting.

    // Fingerprint of the last frame drawn (LAN-1226). Every mutation of `app`
    // happens in this loop before the next draw, so one comparison right
    // before `terminal.draw` covers key handling, progress, tick, result and
    // error paths alike; an unchanged fingerprint skips the frame rebuild.
    let mut last_drawn: Option<u64> = None;

    loop {
        // Preserve only explicit palette edits, including on cancel timeout.
        remember_tui_theme(&mut prefs, &app);

        // If we're waiting for cancel result and deadline expired, exit now
        if let Some(deadline) = app.cancel_deadline
            && deadline.elapsed() > Duration::ZERO
        {
            if let Some(current) = run.take() {
                abort_tui_run(current).await;
            }
            let partial = app.result.is_none();
            if partial {
                app.result = tui_fallback_result(fallback_bytes, fallback_elapsed_ms);
            }
            return Ok((app.result, prefs, print_json_on_exit, partial));
        }

        poll_update_check(&mut app, &update_rx, &mut update_check_done);

        // Persist an explicit update-check toggle from the Settings screen.
        if app.settings.update_check_touched {
            prefs.disable_update_check = Some(!app.settings.update_check);
        }

        // Refresh the wall-clock-driven parts of app state (elapsed counter) so
        // the UI stays live even when the server's Interval progress messages
        // are delayed by packet loss. Fixes issue #62.
        app.tick();

        // Capture the server's advertised version once it shows up. The
        // ServerHello races the TUI loop start; we poll rather than add a new
        // channel since this is a one-time one-line copy.
        if app.server_version.is_none()
            && let Some(active) = run.as_ref()
            && let Some(v) = active.client.server_version()
        {
            // Route through `set_server_version` so the string is sanitized —
            // it comes from the peer and lands in the terminal.
            app.set_server_version(v);
        }

        // Draw only when something on screen changed. The terminal size is
        // part of the fingerprint: `terminal.draw`'s autoresize no longer
        // runs every tick, and the Key-only event match below discards the
        // Resize event, so a resize must show up here to re-flow the layout.
        if app.show_help {
            app.help_scroll =
                xfr::tui::ui::help_scroll(app.help_scroll, terminal.size()?, false, None);
        }
        let fingerprint = app.render_fingerprint(terminal.size()?);
        if last_drawn != Some(fingerprint) {
            terminal.draw(|f| draw(f, &app))?;
            last_drawn = Some(fingerprint);
        }
        let (state, progress, msg) = client_status(&app);
        status.update(terminal.backend_mut(), state, progress, Some(&msg));

        // Handle events with timeout
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if settings_captures_key(app.settings.visible, &key) {
                if handle_tui_settings_key(&mut app, key) == SettingsAction::Restart {
                    apply_tui_settings(config, &mut app);
                    app.cancel_deadline = None;
                    fallback_bytes = 0;
                    fallback_elapsed_ms = 0;
                    print_json_on_exit = false;
                    restart_tui_run(terminal, &mut app, &mut run, config).await?;
                    // `restart_tui_run` may draw a transient "restarting"
                    // frame directly. Its replacement must not be skipped
                    // against the fingerprint from before that draw.
                    last_drawn = None;
                }
                continue;
            }

            if app.show_help {
                app.help_scroll = xfr::tui::ui::help_scroll(
                    app.help_scroll,
                    terminal.size()?,
                    false,
                    Some(key.code),
                );
            }

            match tui_quit_action(
                &key,
                app.state,
                app.cancel_deadline.is_some(),
                run.is_some(),
            ) {
                TuiQuitAction::Noop => {}
                TuiQuitAction::ExitError => {
                    return Err(anyhow::anyhow!(app.error.clone().unwrap_or_default()));
                }
                TuiQuitAction::ExitOk => {
                    return Ok((app.result, prefs, print_json_on_exit, false));
                }
                TuiQuitAction::ForceExit => {
                    if let Some(current) = run.take() {
                        abort_tui_run(current).await;
                    }
                    return Ok((app.result, prefs, print_json_on_exit, false));
                }
                TuiQuitAction::Ignore => {
                    continue;
                }
                TuiQuitAction::StartCancel => {
                    if let Some(active) = run.as_ref() {
                        let _ = active.client.cancel();
                    }
                    app.cancel_deadline = Some(std::time::Instant::now() + TUI_CANCEL_WAIT);
                    // An interrupt from Settings must not leave its restart
                    // action one Enter away from overriding the cancel.
                    app.settings.close();
                    app.log(format!(
                        "Cancelling, waiting up to {}s for the server summary...",
                        TUI_CANCEL_WAIT.as_secs()
                    ));
                    continue;
                }
            }

            match key.code {
                KeyCode::Char('p') => {
                    if let Some(active) = run.as_ref() {
                        match active.client.pause() {
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
                }
                KeyCode::Char('r') => {
                    app.cancel_deadline = None;
                    fallback_bytes = 0;
                    fallback_elapsed_ms = 0;
                    print_json_on_exit = false;
                    restart_tui_run(terminal, &mut app, &mut run, config).await?;
                    last_drawn = None;
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
                KeyCode::Esc if matches!(app.state, AppState::Completed) => {
                    return Ok((app.result, prefs, print_json_on_exit, false));
                }
                KeyCode::Esc if matches!(app.state, AppState::Error) => {
                    return Err(anyhow::anyhow!(app.error.clone().unwrap_or_default()));
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
        if let Some(active) = run.as_mut() {
            while let Ok(progress) = active.progress_rx.try_recv() {
                // Same accumulation as the --no-tui print loop: interval byte
                // deltas summed, last server-reported elapsed kept. Feedback-only
                // updates carry sentinel byte/elapsed fields — skip them.
                if !progress.udp_feedback_only {
                    fallback_bytes += progress.total_bytes;
                    fallback_elapsed_ms = progress.elapsed_ms;
                }
                app.on_progress(progress);
            }
        }

        // Check if test completed
        if run
            .as_ref()
            .is_some_and(|active| active.handle.is_finished())
        {
            let finished_run = run.take().expect("run exists after is_finished check");
            match finish_tui_run(finished_run).await {
                Ok(result) => {
                    app.on_result(result);

                    // If we were waiting for cancel to complete, exit now with result
                    if app.cancel_deadline.is_some() {
                        return Ok((app.result, prefs, print_json_on_exit, false));
                    }
                    // The next iteration draws the result screen: the
                    // fingerprint changed.
                }
                Err(e) => {
                    // If we were waiting for cancel to complete, exit with a
                    // locally-built summary — on a dead link the run errors out
                    // ("Timeout waiting for server response") well before the
                    // cancel deadline, and the stats shouldn't be lost with it.
                    if app.cancel_deadline.is_some() {
                        let partial = app.result.is_none();
                        if partial {
                            app.result = tui_fallback_result(fallback_bytes, fallback_elapsed_ms);
                        }
                        return Ok((app.result, prefs, print_json_on_exit, partial));
                    }

                    app.on_error(e.to_string());
                }
            }
        }
    }
}

async fn run_server_tui(mut config: ServerConfig, theme_name: &str) -> Result<()> {
    // Create channel for server events
    let (tx, mut rx) = mpsc::channel::<ServerEvent>(100);
    config.tui_tx = Some(tx);

    // The dashboard used to hardcode its palette, which made it unreadable on
    // a light terminal and deaf to `--theme`. Resolve it the way the client
    // TUI does so both honour the same flag, saved pref, and NO_COLOR.
    let theme = xfr::tui::theme::Theme::by_name(theme_name);

    // Setup terminal. The guard is armed before entering the alternate screen
    // so a failure there still restores cooked mode on the way out.
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut status = StatusReporter::new(io::stdout().is_terminal());

    let server = Server::new(config);
    let server_handle = tokio::spawn(async move { server.run().await });

    let mut app = ServerApp::new();
    // The dashboard's uptime and active-test timers render at one-second
    // resolution. Keep the 100 ms event poll for responsive input, but skip
    // frame construction until visible state (or terminal size) changes.
    let mut last_drawn: Option<u64> = None;

    loop {
        if app.show_help {
            app.help_scroll =
                xfr::tui::ui::help_scroll(app.help_scroll, terminal.size()?, true, None);
        }
        let fingerprint = app.render_fingerprint(terminal.size()?);
        if last_drawn != Some(fingerprint) {
            terminal.draw(|f| server_draw(f, &app, &theme))?;
            last_drawn = Some(fingerprint);
        }
        match app.active_tests.len() {
            0 => status.update(terminal.backend_mut(), "idle", None, None),
            n => status.update(
                terminal.backend_mut(),
                "working",
                None,
                Some(&format!("{n} active test(s)")),
            ),
        }

        // Handle keyboard events with timeout
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if server_tui_quit_key(&key) {
                break;
            }
            if app.show_help {
                app.help_scroll = xfr::tui::ui::help_scroll(
                    app.help_scroll,
                    terminal.size()?,
                    true,
                    Some(key.code),
                );
            }
            match key.code {
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
                ServerEvent::TestFailed { id, bytes } => {
                    app.fail_test(&id, bytes);
                }
                ServerEvent::ConnectionBlocked => {
                    app.record_blocked();
                }
                ServerEvent::AuthFailure => {
                    app.record_auth_failure();
                }
            }
        }

        // Check if server task ended
        if server_handle.is_finished() {
            break;
        }
    }
    status.finish(terminal.backend_mut());

    // Restore terminal
    drop(_guard);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_thresholds_reject_invalid_cli_values() {
        for value in ["NaN", "inf", "-inf", "-1"] {
            assert!(parse_diff_threshold(value).is_err(), "{value}");
            assert!(
                Cli::try_parse_from([
                    "xfr",
                    "diff",
                    "before.json",
                    "after.json",
                    "--threshold",
                    value
                ])
                .is_err(),
                "{value}"
            );
        }
        for value in ["0", "5", "12.5", "100"] {
            let cli = Cli::try_parse_from([
                "xfr",
                "diff",
                "before.json",
                "after.json",
                "--threshold",
                value,
            ])
            .unwrap();
            match cli.command.unwrap() {
                Commands::Diff { threshold, .. } => {
                    assert_eq!(threshold, value.parse::<f64>().unwrap())
                }
                _ => panic!("expected diff command"),
            }
        }
    }

    fn test_key(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn settings_timestamp_keys_update_the_history_format() {
        let mut app = App::default();
        app.settings.toggle();
        app.settings.selected_index = 1; // Timestamp
        app.log("Before changing the format");

        assert_eq!(
            handle_tui_settings_key(&mut app, test_key(KeyCode::Right)),
            SettingsAction::None
        );
        assert_eq!(app.timestamp_format, TimestampFormat::Iso8601);
        assert!(
            chrono::DateTime::parse_from_rfc3339(&app.history.front().unwrap().timestamp).is_ok()
        );

        handle_tui_settings_key(&mut app, test_key(KeyCode::Char('l')));
        assert_eq!(app.timestamp_format, TimestampFormat::Unix);
        assert!(
            app.history
                .front()
                .unwrap()
                .timestamp
                .parse::<f64>()
                .is_ok()
        );

        handle_tui_settings_key(&mut app, test_key(KeyCode::Left));
        assert_eq!(app.timestamp_format, TimestampFormat::Iso8601);
        handle_tui_settings_key(&mut app, test_key(KeyCode::Char('h')));
        assert_eq!(app.timestamp_format, TimestampFormat::Relative);
        assert_eq!(app.history.front().unwrap().timestamp, "00:00:00");
        assert!(!app.settings.test_params_dirty());
    }

    #[test]
    fn resolve_server_max_duration_cli_overrides_preset() {
        let preset = xfr::config::ServerPreset {
            name: "limited".into(),
            bandwidth_limit: None,
            allowed_clients: None,
            max_duration_secs: Some(60),
        };
        assert_eq!(
            resolve_server_max_duration(Some(Duration::from_secs(120)), Some(&preset)),
            Some(Duration::from_secs(120))
        );
    }

    #[test]
    fn resolve_server_max_duration_uses_preset_when_cli_unset() {
        let preset = xfr::config::ServerPreset {
            name: "limited".into(),
            bandwidth_limit: None,
            allowed_clients: None,
            max_duration_secs: Some(300),
        };
        assert_eq!(
            resolve_server_max_duration(None, Some(&preset)),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn resolve_server_max_duration_none_when_both_unset() {
        assert_eq!(resolve_server_max_duration(None, None), None);
    }

    #[test]
    fn byte_budget_without_explicit_time_has_no_clock() {
        // The whole point of `-n` is a size-defined test. Falling back to the
        // 10s default would truncate any transfer slower than that.
        let cfg = xfr::config::Config::default();
        assert_eq!(
            resolve_client_duration(None, false, Some(1024), &cfg),
            Duration::ZERO
        );
    }

    #[test]
    fn explicit_time_still_caps_a_byte_budget() {
        let cfg = xfr::config::Config::default();
        assert_eq!(
            resolve_client_duration(Some(Duration::from_secs(5)), false, Some(1024), &cfg),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn byte_budget_beats_the_config_file_duration() {
        // A duration in config.toml is a default for timed tests; it must not
        // silently cap a size-defined one the user asked for on the CLI.
        let mut cfg = xfr::config::Config::default();
        cfg.client.duration_secs = Some(25);
        assert_eq!(
            resolve_client_duration(None, false, Some(1024), &cfg),
            Duration::ZERO
        );
    }

    #[test]
    fn parse_byte_budget_accepts_binary_suffixes() {
        assert_eq!(parse_byte_budget("1024"), Ok(1024));
        assert_eq!(parse_byte_budget("1K"), Ok(1024));
        assert_eq!(parse_byte_budget("1k"), Ok(1024));
        assert_eq!(parse_byte_budget("64M"), Ok(64 * 1024 * 1024));
        assert_eq!(parse_byte_budget("2G"), Ok(2 * 1024 * 1024 * 1024));
    }

    #[test]
    fn parse_byte_budget_rejects_nonsense() {
        // Zero is rejected rather than silently meaning "unlimited" the way
        // `-b 0` does — a test that transfers nothing is never intended.
        assert!(parse_byte_budget("0").is_err());
        assert!(parse_byte_budget("0M").is_err());
        assert!(parse_byte_budget("5X").is_err());
        assert!(parse_byte_budget("").is_err());
        assert!(parse_byte_budget("-1").is_err());
        // 999999 GiB overflows u64 only after multiplication, so this checks
        // the checked_mul rather than the parse.
        assert!(parse_byte_budget("99999999999G").is_err());
    }

    #[test]
    fn resolve_client_duration_explicit_time_wins() {
        let cfg = xfr::config::Config::default();
        assert_eq!(
            resolve_client_duration(Some(Duration::from_secs(10)), true, None, &cfg),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn resolve_client_duration_probe_mtu_uses_long_default_when_unset() {
        let cfg = xfr::config::Config::default();
        assert_eq!(
            resolve_client_duration(None, true, None, &cfg),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn resolve_client_duration_uses_config_file_default() {
        let mut cfg = xfr::config::Config::default();
        cfg.client.duration_secs = Some(25);
        assert_eq!(
            resolve_client_duration(None, false, None, &cfg),
            Duration::from_secs(25)
        );
    }

    #[test]
    fn resolve_client_duration_falls_back_to_10s_default() {
        let cfg = xfr::config::Config::default();
        assert_eq!(
            resolve_client_duration(None, false, None, &cfg),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn resolve_client_network_options_uses_config_defaults() {
        let defaults = xfr::config::ClientDefaults {
            bitrate: Some("100M".into()),
            congestion: Some("bbr".into()),
            dscp: Some("EF".into()),
            interval_secs: Some(2.5),
            bind: Some("192.168.1.10".into()),
            cport: Some(5202),
            ..Default::default()
        };

        assert_eq!(
            resolve_client_network_options(None, None, None, None, None, None, &defaults).unwrap(),
            ResolvedClientNetworkOptions {
                bitrate: Some(100_000_000),
                congestion: Some("bbr".into()),
                dscp: Some("EF".into()),
                interval_secs: 2.5,
                bind: Some("192.168.1.10".into()),
                cport: Some(5202),
            }
        );
    }

    #[test]
    fn resolve_client_network_options_cli_overrides_config() {
        let defaults = xfr::config::ClientDefaults {
            bitrate: Some("100M".into()),
            congestion: Some("bbr".into()),
            dscp: Some("EF".into()),
            interval_secs: Some(2.5),
            bind: Some("192.168.1.10".into()),
            cport: Some(5202),
            ..Default::default()
        };

        assert_eq!(
            resolve_client_network_options(
                Some(1_000_000_000),
                Some("cubic".into()),
                Some("AF11".into()),
                Some(1.0),
                Some("10.0.0.10".into()),
                Some(5203),
                &defaults,
            )
            .unwrap(),
            ResolvedClientNetworkOptions {
                bitrate: Some(1_000_000_000),
                congestion: Some("cubic".into()),
                dscp: Some("AF11".into()),
                interval_secs: 1.0,
                bind: Some("10.0.0.10".into()),
                cport: Some(5203),
            }
        );
    }

    #[test]
    fn resolve_client_network_options_rejects_invalid_config_bitrate() {
        let defaults = xfr::config::ClientDefaults {
            bitrate: Some("fast".into()),
            ..Default::default()
        };
        assert!(
            resolve_client_network_options(None, None, None, None, None, None, &defaults).is_err()
        );
    }

    #[test]
    fn test_cli_interval_is_only_set_when_explicit() {
        use clap::Parser;

        assert!(
            Cli::try_parse_from(["xfr", "host"])
                .unwrap()
                .interval
                .is_none()
        );
        assert_eq!(
            Cli::try_parse_from(["xfr", "host", "--interval", "2.5"])
                .unwrap()
                .interval,
            Some(2.5)
        );
    }

    #[test]
    fn report_intervals_reject_invalid_cli_values() {
        for value in ["NaN", "inf", "-inf", "0", "-1", "1e308", "1e19"] {
            assert!(parse_report_interval(value).is_err(), "{value}");
            assert!(
                Cli::try_parse_from(["xfr", "host", "--interval", value]).is_err(),
                "{value}"
            );
        }
        for value in ["0.25", "1", "2.5"] {
            assert_eq!(
                Cli::try_parse_from(["xfr", "host", "--interval", value])
                    .unwrap()
                    .interval,
                Some(value.parse().unwrap())
            );
        }
    }

    #[test]
    fn report_intervals_validate_config_and_preserve_cli_precedence() {
        for interval in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.0,
            -1.0,
            1e308,
            1e19,
        ] {
            let defaults = xfr::config::ClientDefaults {
                interval_secs: Some(interval),
                ..Default::default()
            };
            assert!(
                resolve_client_network_options(None, None, None, None, None, None, &defaults)
                    .is_err()
            );
            let options =
                resolve_client_network_options(None, None, None, Some(1.0), None, None, &defaults)
                    .unwrap();
            assert_eq!(options.interval_secs, 1.0);
        }
    }

    #[test]
    fn test_cli_probe_mtu_default_time_unset() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["xfr", "host", "--probe-mtu"]).unwrap();
        assert!(cli.probe_mtu);
        assert!(cli.time.is_none());
    }

    #[test]
    fn test_cli_probe_mtu_explicit_time_is_preserved() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["xfr", "host", "--probe-mtu", "-t", "10s"]).unwrap();
        assert!(cli.probe_mtu);
        assert_eq!(cli.time, Some(Duration::from_secs(10)));
    }

    fn ctrl_c_key() -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    #[test]
    fn tui_interrupts_preserve_cancel_and_exit_actions() {
        for key in [ctrl_c_key(), test_key(KeyCode::Char('\x03'))] {
            assert!(is_ctrl_c(&key));
            for (state, pending, active, expected) in [
                (AppState::Running, false, true, TuiQuitAction::StartCancel),
                (AppState::Paused, false, true, TuiQuitAction::StartCancel),
                (AppState::Running, true, true, TuiQuitAction::ForceExit),
                (AppState::Completed, false, true, TuiQuitAction::ExitOk),
                (AppState::Error, false, true, TuiQuitAction::ExitError),
                (AppState::Running, false, false, TuiQuitAction::ExitOk),
            ] {
                assert_eq!(tui_quit_action(&key, state, pending, active), expected);
            }
        }
        for code in [KeyCode::Char('c'), KeyCode::Char('q'), KeyCode::Esc] {
            assert!(!is_ctrl_c(&test_key(code)));
        }
    }

    #[test]
    fn tui_settings_lets_interrupts_through() {
        for key in [ctrl_c_key(), test_key(KeyCode::Char('\x03'))] {
            assert!(!settings_captures_key(true, &key));
            assert!(server_tui_quit_key(&key));
        }
        for code in [
            KeyCode::Char('c'),
            KeyCode::Char('q'),
            KeyCode::Enter,
            KeyCode::Esc,
        ] {
            assert!(settings_captures_key(true, &test_key(code)));
            assert!(!settings_captures_key(false, &test_key(code)));
        }
        assert!(server_tui_quit_key(&test_key(KeyCode::Char('q'))));
        assert!(!server_tui_quit_key(&test_key(KeyCode::Char('c'))));
        assert!(!server_tui_quit_key(&test_key(KeyCode::Esc)));
    }

    #[test]
    fn tui_protocol_changes_keep_udp_length_as_a_preference() {
        let mut config = ClientConfig {
            protocol: Protocol::Udp,
            udp_payload_size: Some(1368),
            ..Default::default()
        };
        for protocol in [Protocol::Tcp, Protocol::Quic, Protocol::Udp] {
            config.protocol = protocol;
            let active = tui_run_config(&config);
            assert!(
                active.validate().is_ok(),
                "{protocol}: incompatible UDP payload setting"
            );
            assert_eq!(
                active.udp_payload_size,
                (protocol == Protocol::Udp).then_some(1368)
            );
            assert_eq!(
                config.udp_payload_size,
                Some(1368),
                "keep the choice for switching back"
            );
        }
    }

    #[test]
    fn tui_protocol_changes_scope_tcp_only_send_options() {
        let mut config = ClientConfig {
            protocol: Protocol::Tcp,
            mptcp: true,
            zerocopy: ZerocopyMode::Requested,
            ..Default::default()
        };
        for protocol in [Protocol::Udp, Protocol::Quic, Protocol::Tcp] {
            config.protocol = protocol;
            let active = tui_run_config(&config);
            assert_eq!(active.mptcp, protocol == Protocol::Tcp);
            assert_eq!(
                active.zerocopy,
                if protocol == Protocol::Tcp {
                    ZerocopyMode::Requested
                } else {
                    ZerocopyMode::Off
                }
            );
            assert!(config.mptcp);
            assert_eq!(config.zerocopy, ZerocopyMode::Requested);
        }
    }

    #[test]
    fn tui_switch_to_tcp_uses_the_cli_zerocopy_preference() {
        use clap::Parser;
        // A `-u`/`-Q` session switched to TCP gets the TCP default, and an
        // explicit --no-zerocopy survives the switch.
        for (args, startup, tcp) in [
            (
                &["xfr", "host", "-u"][..],
                Protocol::Udp,
                ZerocopyMode::Auto,
            ),
            (
                &["xfr", "host", "-Q"][..],
                Protocol::Quic,
                ZerocopyMode::Auto,
            ),
            (
                &["xfr", "host", "-u", "--no-zerocopy"][..],
                Protocol::Udp,
                ZerocopyMode::Off,
            ),
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            let mut config = ClientConfig {
                protocol: startup,
                zerocopy: effective_zerocopy(&cli),
                ..Default::default()
            };
            assert_eq!(tui_run_config(&config).zerocopy, ZerocopyMode::Off);
            config.protocol = Protocol::Tcp;
            assert_eq!(tui_run_config(&config).zerocopy, tcp, "{args:?}");
        }
    }

    #[test]
    fn tui_protocol_changes_recompute_fixed_port_assignment() {
        let mut config = ClientConfig {
            protocol: Protocol::Quic,
            streams: 2,
            bind_addr: Some("127.0.0.1:5202".parse().unwrap()),
            sequential_ports: false,
            ..Default::default()
        };
        for protocol in [Protocol::Tcp, Protocol::Udp, Protocol::Quic] {
            config.protocol = protocol;
            assert_eq!(
                tui_run_config(&config).sequential_ports,
                protocol != Protocol::Quic,
                "{protocol} must use its current socket layout"
            );
            assert!(!config.sequential_ports);
        }
    }

    #[test]
    fn tui_stream_count_changes_recompute_fixed_port_assignment() {
        let mut config = ClientConfig {
            protocol: Protocol::Udp,
            bind_addr: Some("127.0.0.1:5202".parse().unwrap()),
            ..Default::default()
        };
        for streams in [1, 2, 1] {
            config.streams = streams;
            assert_eq!(tui_run_config(&config).sequential_ports, streams > 1);
        }
    }

    #[test]
    fn tui_ephemeral_binds_keep_independent_ports() {
        for bind_addr in [None, Some("127.0.0.1:0".parse().unwrap())] {
            for protocol in [Protocol::Tcp, Protocol::Udp, Protocol::Quic] {
                let config = ClientConfig {
                    protocol,
                    streams: 2,
                    bind_addr,
                    ..Default::default()
                };
                assert!(!tui_run_config(&config).sequential_ports);
            }
        }
    }

    #[test]
    fn tui_quit_action_ignores_non_quit_keys() {
        assert_eq!(
            tui_quit_action(
                &test_key(KeyCode::Char('r')),
                AppState::Running,
                false,
                true
            ),
            TuiQuitAction::Noop
        );
    }

    #[test]
    fn tui_quit_action_starts_graceful_cancel_for_running_test() {
        assert_eq!(
            tui_quit_action(
                &test_key(KeyCode::Char('q')),
                AppState::Running,
                false,
                true
            ),
            TuiQuitAction::StartCancel
        );
        assert_eq!(
            tui_quit_action(&ctrl_c_key(), AppState::Running, false, true),
            TuiQuitAction::StartCancel
        );
    }

    #[test]
    fn tui_quit_action_keeps_repeated_q_graceful_while_cancel_pending() {
        assert_eq!(
            tui_quit_action(&test_key(KeyCode::Char('q')), AppState::Running, true, true),
            TuiQuitAction::Ignore
        );
        assert_eq!(
            tui_quit_action(&ctrl_c_key(), AppState::Running, true, true),
            TuiQuitAction::ForceExit
        );
    }

    #[test]
    fn tui_fallback_result_only_when_data_was_collected() {
        // Nothing transferred -> no summary worth showing ("Test cancelled.").
        assert!(tui_fallback_result(0, 1000).is_none());

        // 1 MB over 2 s -> 4 Mbps, built from the local counters (issue #159).
        let result = tui_fallback_result(1_000_000, 2000).expect("summary for collected data");
        assert_eq!(result.bytes_total, 1_000_000);
        assert_eq!(result.duration_ms, 2000);
        assert!((result.throughput_mbps - 4.0).abs() < 1e-9);
    }

    #[test]
    fn tui_quit_action_exits_when_run_is_done_or_missing() {
        assert_eq!(
            tui_quit_action(
                &test_key(KeyCode::Char('q')),
                AppState::Completed,
                false,
                true
            ),
            TuiQuitAction::ExitOk
        );
        assert_eq!(
            tui_quit_action(
                &test_key(KeyCode::Char('q')),
                AppState::Running,
                false,
                false
            ),
            TuiQuitAction::ExitOk
        );
    }

    #[test]
    fn tui_quit_action_preserves_error_exit() {
        assert_eq!(
            tui_quit_action(&test_key(KeyCode::Char('q')), AppState::Error, false, true),
            TuiQuitAction::ExitError
        );
    }

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
    fn test_duration_rejects_wrapping_protocol_seconds() {
        for value in ["4294967296", "4294967296s", "18446744073709551615"] {
            assert!(parse_test_duration(value).is_err(), "accepted {value}");
            assert!(Cli::try_parse_from(["xfr", "host", "-t", value]).is_err());
        }
    }

    #[test]
    fn test_duration_preserves_zero_fractional_values_and_the_wire_boundary() {
        assert_eq!(parse_test_duration("0").unwrap(), Duration::ZERO);
        assert_eq!(
            parse_test_duration("1s 500ms").unwrap(),
            Duration::from_millis(1500)
        );
        assert_eq!(
            parse_test_duration("4294967295s 500ms").unwrap(),
            Duration::new(u64::from(u32::MAX), 500_000_000),
        );
        let cli = Cli::try_parse_from(["xfr", "host", "-t", "4294967295"]).unwrap();
        assert_eq!(cli.time, Some(Duration::from_secs(u64::from(u32::MAX))));
    }

    #[test]
    fn explicit_duration_overrides_unused_out_of_range_config() {
        let mut config = Config::default();
        config.client.duration_secs = Some(u64::from(u32::MAX) + 1);
        let cli = Cli::try_parse_from(["xfr", "host", "-t", "1"]).unwrap();
        let duration = resolve_client_duration(cli.time, false, None, &config);
        assert_eq!(duration, Duration::from_secs(1));
        assert!(
            ClientConfig {
                duration,
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn test_parse_duration_bare_integer_is_seconds() {
        // iperf3 muscle memory: `-t 10` should mean 10 seconds.
        assert_eq!(parse_duration("10").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_duration("1").unwrap(), Duration::from_secs(1));
        assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
        assert_eq!(parse_duration("3600").unwrap(), Duration::from_secs(3600));

        // Unit-suffixed forms continue to work unchanged.
        assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_duration("1min").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    }

    #[test]
    fn test_parse_test_duration_bare_integer_is_seconds() {
        // Bare-int path must also apply the >= 1s minimum rule.
        assert_eq!(parse_test_duration("10").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_test_duration("1").unwrap(), Duration::from_secs(1));
        assert_eq!(parse_test_duration("0").unwrap(), Duration::ZERO);
    }

    #[test]
    fn test_parse_duration_rejects_garbage() {
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("10q").is_err()); // unknown unit
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn rate_limit_window_cli_rejects_zero_and_fractional_seconds() {
        for value in ["0", "0s", "0ms", "1ns", "500ms", "1500ms", "1.5s"] {
            let error = Cli::try_parse_from(["xfr", "serve", "--rate-limit-window", value])
                .err()
                .unwrap_or_else(|| panic!("rate limit window {value} should be rejected"));
            assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        }
    }

    #[test]
    fn rate_limit_window_cli_accepts_whole_seconds_and_preserves_unset() {
        for (value, expected) in [("1", 1), ("60s", 60), ("5min", 300), ("1000ms", 1)] {
            let cli = Cli::try_parse_from(["xfr", "serve", "--rate-limit-window", value]).unwrap();
            let Some(Commands::Serve {
                rate_limit_window, ..
            }) = cli.command
            else {
                panic!("expected serve command");
            };
            assert_eq!(rate_limit_window, Some(expected));
        }

        let cli = Cli::try_parse_from(["xfr", "serve"]).unwrap();
        let Some(Commands::Serve {
            rate_limit_window, ..
        }) = cli.command
        else {
            panic!("expected serve command");
        };
        assert_eq!(rate_limit_window, None);
    }

    #[test]
    fn rate_limit_window_uses_cli_then_config_then_default() {
        assert_eq!(resolve_rate_limit_window(None, None).unwrap(), 60);
        assert_eq!(resolve_rate_limit_window(None, Some(30)).unwrap(), 30);
        // Even an explicit CLI value equal to the default wins over config.
        assert_eq!(resolve_rate_limit_window(Some(60), Some(30)).unwrap(), 60);
        assert_eq!(resolve_rate_limit_window(Some(1), Some(0)).unwrap(), 1);
    }

    #[test]
    fn rate_limit_window_rejects_zero_config() {
        let error = resolve_rate_limit_window(None, Some(0)).unwrap_err();
        assert!(error.to_string().contains("server.rate_limit_window"));
    }

    #[test]
    fn test_parse_nonzero_duration_rejects_zero_variants() {
        // Bare-integer 0 and any unit-suffixed zero must fail.
        assert!(parse_nonzero_duration("0").is_err());
        assert!(parse_nonzero_duration("0s").is_err());
        assert!(parse_nonzero_duration("0ms").is_err());

        // Non-zero values still parse the same as parse_duration.
        assert_eq!(parse_nonzero_duration("1").unwrap(), Duration::from_secs(1));
        assert_eq!(
            parse_nonzero_duration("60s").unwrap(),
            Duration::from_secs(60)
        );
        assert_eq!(
            parse_nonzero_duration("500ms").unwrap(),
            Duration::from_millis(500)
        );

        // Garbage still fails.
        assert!(parse_nonzero_duration("abc").is_err());
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
    fn parallel_streams_resolve_cli_then_config_then_default() {
        let explicit = Cli::try_parse_from(["xfr", "host", "-P", "1"]).unwrap();
        let unset = Cli::try_parse_from(["xfr", "host"]).unwrap();
        assert_eq!(unset.parallel, None);
        assert_eq!(
            resolve_parallel_streams(explicit.parallel, Some(8)).unwrap(),
            1
        );
        assert_eq!(
            resolve_parallel_streams(unset.parallel, Some(8)).unwrap(),
            8
        );
        assert_eq!(resolve_parallel_streams(unset.parallel, None).unwrap(), 1);
        // Only the effective count is validated.
        for invalid in [0, 129] {
            assert_eq!(
                resolve_parallel_streams(explicit.parallel, Some(invalid)).unwrap(),
                1
            );
            let error = resolve_parallel_streams(None, Some(invalid)).unwrap_err();
            assert!(error.to_string().contains("client.parallel_streams"));
        }
    }

    #[test]
    fn window_size_rejects_invalid_effective_config() {
        for value in ["1MiB", "", "999999999999999999999G"] {
            let error = resolve_window_size(None, Some(value)).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("client.window_size"));
            assert!(message.contains(&format!("{value:?}")));
        }
    }

    #[test]
    fn window_size_uses_valid_defaults_and_explicit_overrides() {
        assert_eq!(resolve_window_size(None, None).unwrap(), None);
        for (value, expected) in [("512K", 512 * 1024), ("1m", 1024 * 1024), ("0", 0)] {
            assert_eq!(
                resolve_window_size(None, Some(value)).unwrap(),
                Some(expected)
            );
        }
        let cli = Cli::try_parse_from(["xfr", "host", "-w", "2M"]).unwrap();
        assert_eq!(
            resolve_window_size(cli.window, Some("1MiB")).unwrap(),
            Some(2 * 1024 * 1024),
        );
        // Preserve the existing explicit zero semantics as well.
        assert_eq!(resolve_window_size(Some(0), Some("1MiB")).unwrap(), Some(0));
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
    fn theme_option_is_available_after_the_server_subcommand() {
        assert!(Cli::try_parse_from(["xfr", "serve", "--tui", "--theme", "default"]).is_ok());
    }

    #[test]
    fn theme_parser_distinguishes_an_explicit_default_from_no_choice() {
        for args in [vec!["xfr", "localhost"], vec!["xfr", "serve", "--tui"]] {
            let cli = Cli::try_parse_from(&args).unwrap();
            assert!(cli.theme.is_none());
            let mut explicit = args;
            explicit.extend(["--theme", "default"]);
            let cli = Cli::try_parse_from(explicit).unwrap();
            assert_eq!(cli.theme.as_deref(), Some("default"));
            let prefs = xfr::prefs::Prefs {
                theme: Some("dracula".into()),
                ..Default::default()
            }
            .with_overrides(cli.theme.as_deref(), Some("nord"));
            assert_eq!(prefs.theme.as_deref(), Some("default"));
        }
        let cli = Cli::try_parse_from(["xfr", "--theme", "default", "serve", "--tui"]).unwrap();
        assert_eq!(cli.theme.as_deref(), Some("default"));
    }

    #[test]
    fn automatic_theme_is_not_saved_without_an_explicit_edit() {
        for theme in ["default", "monochrome"] {
            let mut prefs = xfr::prefs::Prefs::default();
            let mut app = App::with_theme_name(
                "localhost".into(),
                5201,
                Protocol::Tcp,
                Direction::Upload,
                1,
                Duration::from_secs(10),
                None,
                TimestampFormat::Relative,
                theme,
            );
            remember_tui_theme(&mut prefs, &app);
            assert!(
                prefs.theme.is_none(),
                "automatic {theme} must remain transient"
            );

            // An unrelated display edit must not promote an automatic
            // palette into an explicit saved preference either.
            app.settings.visible = true;
            app.settings.selected_index = 1; // Timestamp
            handle_tui_settings_key(&mut app, test_key(KeyCode::Right));
            remember_tui_theme(&mut prefs, &app);
            assert!(prefs.theme.is_none());

            app.cycle_theme();
            remember_tui_theme(&mut prefs, &app);
            assert_eq!(prefs.theme.as_deref(), Some(app.theme_name()));

            // Returning to the initial palette is still an explicit choice,
            // and restarting a test must preserve that intent.
            for _ in 1..xfr::tui::Theme::list().len() {
                app.cycle_theme();
            }
            app.reset_for_restart();
            remember_tui_theme(&mut prefs, &app);
            assert_eq!(prefs.theme.as_deref(), Some(theme));
        }
    }

    #[test]
    fn untouched_explicit_theme_preferences_are_preserved() {
        for theme in ["default", "dracula", "mono"] {
            let mut prefs = xfr::prefs::Prefs {
                theme: Some(theme.into()),
                ..Default::default()
            };
            let app = App::with_theme_name(
                "localhost".into(),
                5201,
                Protocol::Tcp,
                Direction::Upload,
                1,
                Duration::from_secs(10),
                None,
                TimestampFormat::Relative,
                theme,
            );
            remember_tui_theme(&mut prefs, &app);
            assert_eq!(prefs.theme.as_deref(), Some(theme));
        }
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
    fn test_cli_zerocopy_flag() {
        use clap::Parser;

        // Bare invocation: neither flag set, mode resolves to Auto for TCP.
        let cli = Cli::try_parse_from(["xfr", "host"]).unwrap();
        assert!(!cli.zerocopy);
        assert!(!cli.no_zerocopy);
        assert_eq!(effective_zerocopy(&cli), ZerocopyMode::Auto);

        let cli = Cli::try_parse_from(["xfr", "host", "-Z"]).unwrap();
        assert!(cli.zerocopy);
        assert_eq!(effective_zerocopy(&cli), ZerocopyMode::Requested);

        let cli = Cli::try_parse_from(["xfr", "host", "--zerocopy"]).unwrap();
        assert!(cli.zerocopy);

        let cli = Cli::try_parse_from(["xfr", "host", "--no-zerocopy"]).unwrap();
        assert!(cli.no_zerocopy);
        assert_eq!(effective_zerocopy(&cli), ZerocopyMode::Off);

        // Opt-in and opt-out are mutually exclusive
        assert!(Cli::try_parse_from(["xfr", "host", "-Z", "--no-zerocopy"]).is_err());

        // sendfile is a TCP-only mechanism: explicit -Z with UDP/QUIC must
        // conflict; --no-zerocopy with them is a harmless no-op.
        assert!(Cli::try_parse_from(["xfr", "host", "-Z", "-u"]).is_err());
        assert!(Cli::try_parse_from(["xfr", "host", "-Z", "-Q"]).is_err());
        assert!(Cli::try_parse_from(["xfr", "host", "--no-zerocopy", "-u"]).is_ok());

        // MPTCP is allowed (sendfile goes through the regular splice path)
        assert!(Cli::try_parse_from(["xfr", "host", "-Z", "--mptcp"]).is_ok());
    }

    #[test]
    fn test_cli_connect_timeout() {
        use clap::Parser;

        let cli = Cli::try_parse_from(["xfr", "host"]).unwrap();
        assert!(cli.connect_timeout.is_none(), "no timeout by default");

        let cli = Cli::try_parse_from(["xfr", "host", "--connect-timeout", "5s"]).unwrap();
        assert_eq!(cli.connect_timeout, Some(Duration::from_secs(5)));

        // Zero would mean "fail instantly, always" — rejected like the
        // other nonzero-duration flags.
        assert!(Cli::try_parse_from(["xfr", "host", "--connect-timeout", "0s"]).is_err());
    }

    #[test]
    fn test_cli_probe_mtu_flag() {
        use clap::Parser;

        let cli = Cli::try_parse_from(["xfr", "host", "--probe-mtu"]).unwrap();
        assert!(cli.probe_mtu);

        // Redundant -u is accepted (probe implies UDP anyway)
        assert!(Cli::try_parse_from(["xfr", "host", "--probe-mtu", "-u"]).is_ok());

        // A diagnostic mode: throughput-test knobs conflict
        for forbidden in ["-Q", "-R", "--bidir", "-Z", "--zeros"] {
            assert!(
                Cli::try_parse_from(["xfr", "host", "--probe-mtu", forbidden]).is_err(),
                "--probe-mtu must conflict with {forbidden}"
            );
        }
        assert!(Cli::try_parse_from(["xfr", "host", "--probe-mtu", "-b", "1G"]).is_err());
        assert!(Cli::try_parse_from(["xfr", "host", "--probe-mtu", "-P", "4"]).is_err());
    }

    #[test]
    fn test_cli_udp_payload_size() {
        assert!(
            Cli::try_parse_from(["xfr", "host", "-u"])
                .unwrap()
                .udp_payload_size
                .is_none()
        );
        for flag in ["-l", "--length"] {
            for size in ["16", "1368", "65507"] {
                let cli = Cli::try_parse_from(["xfr", "host", "-u", flag, size]).unwrap();
                assert_eq!(cli.udp_payload_size, Some(size.parse().unwrap()));
            }
            for size in ["0", "15", "65508", "65536"] {
                assert!(Cli::try_parse_from(["xfr", "host", "-u", flag, size]).is_err());
            }
        }
        assert!(Cli::try_parse_from(["xfr", "host", "-l", "1368"]).is_err());
        assert!(Cli::try_parse_from(["xfr", "host", "-Q", "-l", "1368"]).is_err());
        assert!(Cli::try_parse_from(["xfr", "host", "--mptcp", "-l", "1368"]).is_err());
        assert!(Cli::try_parse_from(["xfr", "host", "-u", "--probe-mtu", "-l", "1368"]).is_err());
    }

    #[test]
    fn test_zerocopy_notice() {
        // Off and Auto never warn — default-on falls back silently.
        for mode in [ZerocopyMode::Off, ZerocopyMode::Auto] {
            assert!(zerocopy_notice(mode, Direction::Upload).is_none());
            assert!(zerocopy_notice(mode, Direction::Bidir).is_none());
            assert!(zerocopy_notice(mode, Direction::Download).is_none());
        }

        if cfg!(target_os = "linux") {
            // Supported platform: never warn.
            assert!(zerocopy_notice(ZerocopyMode::Requested, Direction::Upload).is_none());
            assert!(zerocopy_notice(ZerocopyMode::Requested, Direction::Download).is_none());
            assert!(zerocopy_notice(ZerocopyMode::Requested, Direction::Bidir).is_none());
        } else {
            // Unsupported platform: warn only when the client itself sends.
            assert!(zerocopy_notice(ZerocopyMode::Requested, Direction::Upload).is_some());
            assert!(zerocopy_notice(ZerocopyMode::Requested, Direction::Bidir).is_some());
            // Download mode: the server decides its own zero-copy support.
            assert!(zerocopy_notice(ZerocopyMode::Requested, Direction::Download).is_none());
        }
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
    fn explicit_server_port_equal_to_default_overrides_config() {
        let cli = Cli::try_parse_from(["xfr", "serve", "--port", "5201"]).unwrap();
        let Some(Commands::Serve { port, .. }) = cli.command else {
            panic!("expected serve command");
        };
        assert_eq!(resolve_server_port(port, Some(9000)), DEFAULT_PORT);
    }

    #[test]
    fn server_port_uses_config_then_default_when_unset() {
        use clap::{CommandFactory, FromArgMatches};

        // Isolate the fallback case from an XFR_PORT set by the test runner.
        let matches = Cli::command()
            .mut_subcommand("serve", |command| {
                command.mut_arg("port", |arg| arg.env(None::<&str>))
            })
            .try_get_matches_from(["xfr", "serve"])
            .unwrap();
        let cli = Cli::from_arg_matches(&matches).unwrap();
        let Some(Commands::Serve { port, .. }) = cli.command else {
            panic!("expected serve command");
        };
        assert_eq!(port, None);
        assert_eq!(resolve_server_port(port, Some(9000)), 9000);
        assert_eq!(resolve_server_port(port, None), DEFAULT_PORT);
    }

    #[test]
    fn server_port_environment_and_cli_override_config() {
        const CHILD_FLAG: &str = "XFR_SERVER_PORT_TEST_CHILD";
        if std::env::var_os(CHILD_FLAG).is_none() {
            // A child process avoids mutating the environment while other
            // parser tests are running in parallel.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::server_port_environment_and_cli_override_config",
                ])
                .env(CHILD_FLAG, "1")
                .env("XFR_PORT", DEFAULT_PORT.to_string())
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            // `--exact` with a stale name runs zero tests and still exits 0.
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "environment precedence check failed: {stdout}{}",
                String::from_utf8_lossy(&output.stderr),
            );
            return;
        }

        for (args, expected) in [
            (vec!["xfr", "serve"], DEFAULT_PORT),
            (vec!["xfr", "serve", "--port", "8000"], 8000),
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            let Some(Commands::Serve { port, .. }) = cli.command else {
                panic!("expected serve command");
            };
            assert_eq!(resolve_server_port(port, Some(9000)), expected);
        }
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
    fn address_family_cli_rejects_contradictory_flags() {
        for mode in ["host", "serve"] {
            for flags in [
                vec!["-4", "-6"],
                vec!["-6", "-4"],
                vec!["--ipv4", "--ipv6"],
                vec!["--ipv6", "--ipv4"],
                vec!["-46"],
            ] {
                let error = Cli::try_parse_from(["xfr", mode].into_iter().chain(flags))
                    .err()
                    .expect("contradictory address-family flags should fail");
                assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
            }
        }
    }

    #[test]
    fn address_family_rejects_invalid_effective_config() {
        for key in ["client.address_family", "server.address_family"] {
            let error = resolve_address_family(false, false, Some("ip4"), key).unwrap_err();
            let message = error.to_string();
            assert!(message.contains(key));
            assert!(message.contains("ip4"));
        }
    }

    #[test]
    fn address_family_accepts_valid_config() {
        // Alias coverage lives with `AddressFamily::from_str` in net.rs.
        let key = "client.address_family";
        let resolve = |value| resolve_address_family(false, false, value, key).unwrap();
        assert_eq!(resolve(Some("IPv6")), AddressFamily::V6Only);
        assert_eq!(resolve(None), AddressFamily::DualStack);
    }

    #[test]
    fn address_family_cli_overrides_unused_invalid_config() {
        for key in ["client.address_family", "server.address_family"] {
            assert_eq!(
                resolve_address_family(true, false, Some("ip4"), key).unwrap(),
                AddressFamily::V4Only,
            );
            assert_eq!(
                resolve_address_family(false, true, Some("ip4"), key).unwrap(),
                AddressFamily::V6Only,
            );
        }
        for mode in ["host", "serve"] {
            for flag in ["-4", "-6", "--ipv4", "--ipv6"] {
                assert!(Cli::try_parse_from(["xfr", mode, flag]).is_ok());
            }
        }
    }

    #[test]
    fn address_family_server_bind_overrides_unused_config() {
        for (ip, expected) in [
            ("127.0.0.1", AddressFamily::V4Only),
            ("::1", AddressFamily::V6Only),
        ] {
            assert_eq!(
                resolve_server_address_family(Some(ip.parse().unwrap()), false, false, Some("ip4"))
                    .unwrap(),
                expected,
            );
        }
        assert!(
            resolve_server_address_family(Some("127.0.0.1".parse().unwrap()), false, true, None)
                .is_err()
        );
        assert!(
            resolve_server_address_family(Some("::1".parse().unwrap()), true, false, None).is_err()
        );
        for ip in ["0.0.0.0", "::"] {
            assert!(
                resolve_server_address_family(Some(ip.parse().unwrap()), false, false, None)
                    .is_err()
            );
        }
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
        assert!(parse_bind_address("[::1]:notaport").is_err());
    }

    #[test]
    fn test_parse_bind_address_ipv6_zone() {
        let scope = |addr: std::net::SocketAddr| match addr {
            std::net::SocketAddr::V6(v6) => (v6.scope_id(), v6.port()),
            _ => panic!("expected IPv6"),
        };
        assert_eq!(scope(parse_bind_address("fe80::1%3").unwrap()), (3, 0));
        assert_eq!(scope(parse_bind_address("[fe80::1%3]").unwrap()), (3, 0));
        assert_eq!(
            scope(parse_bind_address("[fe80::1%3]:5000").unwrap()),
            (3, 5000)
        );
        #[cfg(target_os = "linux")]
        assert_ne!(scope(parse_bind_address("[fe80::1%lo]:5000").unwrap()).0, 0);
        let err = parse_bind_address("fe80::1%xfr-no-such-if0").unwrap_err();
        assert!(err.to_string().contains("xfr-no-such-if0"), "{err}");
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

    #[test]
    fn scripted_udp_direction_cache_keeps_gap_repairs_and_measured_jitter() {
        use xfr::protocol::{UdpDirectionProgress, UdpIntervalProgress};
        let gap = UdpDirectionProgress {
            progress: UdpIntervalProgress {
                packets_received: 2,
                packets_lost: 1,
            },
            jitter_ms: Some(0.0),
        };
        let repair = UdpDirectionProgress {
            progress: UdpIntervalProgress {
                packets_received: 3,
                packets_lost: 0,
            },
            jitter_ms: None,
        };
        let mut cache = None;
        assert!(cache_udp_direction(&mut cache, None).is_none());
        assert!(cache.is_none());
        cache_udp_direction(&mut cache, Some(gap));
        let accepted = cache_udp_direction(&mut cache, Some(repair)).unwrap();
        assert_eq!(accepted.progress.lost_percent(), Some(0.0));
        assert_eq!(accepted.jitter_ms, Some(0.0));
        assert!(cache_udp_direction(&mut cache, Some(gap)).is_none());
        assert_eq!(cache.unwrap().progress.packets_received, 3);
        assert!(cache_udp_direction(&mut cache, None).is_none());
        assert_eq!(cache.unwrap().jitter_ms, Some(0.0));
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
                    bytes_sent: None,
                    bytes_received: None,
                    throughput_send_mbps: None,
                    throughput_recv_mbps: None,
                    rtt_us: None,
                    cwnd: None,
                })
                .collect(),
            rtt_us: None,
            cwnd: None,
            total_retransmits,
            bytes_sent: None,
            bytes_received: None,
            throughput_send_mbps: None,
            throughput_recv_mbps: None,
            udp_progress: None,
            udp_feedback_only: false,
            udp_send: None,
            udp_recv: None,
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
