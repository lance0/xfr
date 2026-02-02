//! xfr - Modern network bandwidth testing with TUI

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
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
use xfr::protocol::{DEFAULT_PORT, Direction, Protocol, TimestampFormat};
use xfr::serve::{Server, ServerConfig};
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
                .without_time();
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
            // Normal mode: console logging only
            let console_layer = tracing_subscriber::fmt::layer()
                .with_target(false)
                .without_time();
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

    /// Test duration
    #[arg(short = 't', long, default_value = "10s", value_parser = parse_duration, env = "XFR_DURATION")]
    time: Duration,

    /// UDP mode
    #[arg(short = 'u', long)]
    udp: bool,

    /// QUIC mode (encrypted, multiplexed streams)
    #[arg(long)]
    quic: bool,

    /// Target bitrate (e.g., 1G, 100M)
    #[arg(short = 'b', long, value_parser = parse_bitrate)]
    bitrate: Option<u64>,

    /// Number of parallel streams
    #[arg(short = 'P', long, default_value_t = 1)]
    parallel: u8,

    /// Reverse direction (server sends to client)
    #[arg(short = 'R', long)]
    reverse: bool,

    /// Bidirectional test
    #[arg(long)]
    bidir: bool,

    /// JSON output
    #[arg(long)]
    json: bool,

    /// JSON streaming output (one object per line)
    #[arg(long)]
    json_stream: bool,

    /// CSV output
    #[arg(long)]
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
    #[arg(long, default_value = "0")]
    omit: u64,

    /// Disable Nagle algorithm
    #[arg(long)]
    tcp_nodelay: bool,

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

        /// Force IPv4 only
        #[arg(short = '4', long = "ipv4")]
        ipv4_only: bool,

        /// Force IPv6 only
        #[arg(short = '6', long = "ipv6")]
        ipv6_only: bool,
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

    num.parse::<u64>()
        .map(|n| n * suffix)
        .map_err(|e| e.to_string())
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
        .map(|n| n * suffix)
        .map_err(|e| e.to_string())
}

fn parse_timestamp_format(s: &str) -> Result<TimestampFormat, String> {
    s.parse::<TimestampFormat>()
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
            ipv4_only,
            ipv6_only,
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
            let address_family = if ipv4_only {
                xfr::net::AddressFamily::V4Only
            } else if ipv6_only {
                xfr::net::AddressFamily::V6Only
            } else if let Some(ref af) = file_config.server.address_family {
                af.parse().unwrap_or_default()
            } else {
                xfr::net::AddressFamily::default()
            };

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
                tui_tx: None,
                enable_quic: true, // Enable QUIC by default
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
            let Some(host) = cli.host else {
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

            let config = ClientConfig {
                host: host.clone(),
                port: cli.port,
                protocol,
                streams,
                duration,
                direction,
                bitrate: cli.bitrate,
                tcp_nodelay,
                window_size,
                psk: client_psk,
                address_family: client_address_family,
            };

            // Determine output format
            let output_opts = OutputOptions {
                json: json_output,
                json_stream: cli.json_stream,
                csv: cli.csv,
                quiet: cli.quiet,
                omit_secs: cli.omit,
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
    let client = Client::new(config.clone());

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

    // Print intervals in a separate task
    let print_handle = tokio::spawn(async move {
        let mut last_printed_interval: i64 = -1;

        while let Some(progress) = rx.recv().await {
            let elapsed_secs = progress.elapsed_ms as f64 / 1000.0;

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

            let retransmits = progress.streams.first().and_then(|s| s.retransmits);
            let jitter_ms = progress.streams.first().and_then(|s| s.jitter_ms);
            let lost = progress.streams.first().and_then(|s| s.lost);

            let now = std::time::Instant::now();
            let timestamp = timestamp_format.format(test_start, now);

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
                )
            } else {
                xfr::output::plain::output_interval_plain(
                    &timestamp,
                    elapsed_secs,
                    progress.throughput_mbps,
                    progress.total_bytes,
                    retransmits,
                )
            };
            print!("{}", interval_output);
            let _ = io::stdout().flush();
        }
    });

    let result = client.run(Some(tx)).await?;

    // Wait for print task to finish
    let _ = print_handle.await;

    // Output result
    let output_str = if opts.json || opts.json_stream {
        output_json(&result)
    } else if opts.csv {
        output_csv(&result)
    } else {
        output_plain(&result)
    };

    println!("{}", output_str);

    if let Some(path) = output {
        xfr::output::json::save_json(&result, &path)?;
        info!("Results saved to {}", path.display());
    }

    Ok(())
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
                    println!("{}", output_plain(test_result));
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

    let client = Arc::new(Client::new(config));
    let client_for_task = client.clone();
    let (progress_tx, mut progress_rx) = mpsc::channel::<TestProgress>(100);

    // Start the test
    let test_handle = tokio::spawn(async move { client_for_task.run(Some(progress_tx)).await });

    app.on_connected();

    loop {
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

            match key.code {
                KeyCode::Char('q') => {
                    // Cancel the test on server before exiting
                    let _ = client.cancel();
                    prefs.theme = Some(app.theme_name().to_string());
                    return Ok((app.result, prefs, print_json_on_exit));
                }
                KeyCode::Char('p') => {
                    app.toggle_pause();
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
                KeyCode::Esc => {
                    if app.show_help {
                        app.show_help = false;
                    }
                }
                KeyCode::Char('j') => {
                    if app.result.is_some() {
                        // Set flag to print JSON after TUI closes
                        print_json_on_exit = true;
                        app.log("JSON output queued for display on exit.");
                    }
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
                    // Show final result for a moment
                    terminal.draw(|f| draw(f, &app))?;

                    // Wait for quit
                    loop {
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

                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => {
                                    prefs.theme = Some(app.theme_name().to_string());
                                    return Ok((app.result, prefs, print_json_on_exit));
                                }
                                KeyCode::Char('t') => {
                                    app.cycle_theme();
                                }
                                KeyCode::Char('s') => {
                                    app.settings.toggle();
                                }
                                KeyCode::Char('j') => {
                                    print_json_on_exit = true;
                                    app.log("JSON output queued for display on exit.");
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Err(e) => {
                    app.on_error(e.to_string());
                    terminal.draw(|f| draw(f, &app))?;

                    // Wait for quit
                    loop {
                        if event::poll(Duration::from_millis(100))?
                            && let Event::Key(key) = event::read()?
                            && key.kind == KeyEventKind::Press
                            && key.code == KeyCode::Char('q')
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
                KeyCode::Esc => {
                    if app.show_help {
                        app.show_help = false;
                    }
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
