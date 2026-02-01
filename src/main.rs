//! xfr - Modern network bandwidth testing with TUI

use std::io::{self, Write};
use std::path::PathBuf;
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
use xfr::output::{output_json, output_plain};
use xfr::protocol::{DEFAULT_PORT, Direction, Protocol};
use xfr::serve::{Server, ServerConfig};
use xfr::tui::{App, draw};

#[derive(Parser)]
#[command(name = "xfr")]
#[command(author, version, about = "Modern network bandwidth testing with TUI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Target host for client mode
    #[arg(value_name = "HOST")]
    host: Option<String>,

    /// Server/client port
    #[arg(short, long, default_value_t = DEFAULT_PORT)]
    port: u16,

    /// Test duration
    #[arg(short = 't', long, default_value = "10s", value_parser = parse_duration)]
    time: Duration,

    /// UDP mode
    #[arg(short = 'u', long)]
    udp: bool,

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

    /// Output file
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,

    /// Disable TUI
    #[arg(long)]
    no_tui: bool,

    /// Disable Nagle algorithm
    #[arg(long)]
    tcp_nodelay: bool,

    /// TCP window size (e.g., 512K, 1M)
    #[arg(long, value_parser = parse_size)]
    window: Option<usize>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start server mode
    Serve {
        /// Server port
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,

        /// Exit after one test
        #[arg(long)]
        one_off: bool,

        /// Prometheus metrics port
        #[cfg(feature = "prometheus")]
        #[arg(long)]
        prometheus: Option<u16>,
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

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("xfr=info".parse()?))
        .with_target(false)
        .init();

    let cli = Cli::parse();

    // Load config file (falls back to defaults if not found)
    let file_config = Config::load().unwrap_or_default();

    match cli.command {
        Some(Commands::Serve {
            port,
            one_off,
            #[cfg(feature = "prometheus")]
            prometheus,
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

            let config = ServerConfig {
                port: server_port,
                one_off: server_one_off,
                #[cfg(feature = "prometheus")]
                prometheus_port: prom_port,
            };
            let server = Server::new(config);
            server.run().await?;
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

            let protocol = if cli.udp {
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
            };

            if no_tui || json_output {
                // Plain/JSON mode
                run_client_plain(config, json_output, cli.output).await?;
            } else {
                // TUI mode
                run_client_tui(config, cli.output).await?;
            }
        }
    }

    Ok(())
}

async fn run_client_plain(config: ClientConfig, json: bool, output: Option<PathBuf>) -> Result<()> {
    let client = Client::new(config.clone());

    let (tx, mut rx) = mpsc::channel::<TestProgress>(100);

    // Print intervals in a separate task
    let print_handle = tokio::spawn(async move {
        while let Some(progress) = rx.recv().await {
            if !json {
                let interval_output = xfr::output::plain::output_interval_plain(
                    progress.elapsed_ms as f64 / 1000.0,
                    progress.throughput_mbps,
                    progress.total_bytes,
                    progress.streams.first().and_then(|s| s.retransmits),
                );
                print!("{}", interval_output);
                let _ = io::stdout().flush();
            }
        }
    });

    let result = client.run(Some(tx)).await?;

    // Wait for print task to finish
    let _ = print_handle.await;

    // Output result
    let output_str = if json {
        output_json(&result)
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

async fn run_client_tui(config: ClientConfig, output: Option<PathBuf>) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_tui_loop(&mut terminal, config.clone()).await;

    // Restore terminal
    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;

    match result {
        Ok(Some(test_result)) => {
            println!("{}", output_plain(&test_result));

            if let Some(path) = output {
                xfr::output::json::save_json(&test_result, &path)?;
                println!("Results saved to {}", path.display());
            }
        }
        Ok(None) => {
            println!("Test cancelled.");
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            return Err(e);
        }
    }

    Ok(())
}

async fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: ClientConfig,
) -> Result<Option<xfr::protocol::TestResult>> {
    let mut app = App::new(
        config.host.clone(),
        config.port,
        config.protocol,
        config.direction,
        config.streams,
        config.duration,
        config.bitrate,
    );

    let client = Client::new(config);
    let (progress_tx, mut progress_rx) = mpsc::channel::<TestProgress>(100);

    // Start the test
    let test_handle = tokio::spawn(async move { client.run(Some(progress_tx)).await });

    app.on_connected();

    loop {
        // Draw UI
        terminal.draw(|f| draw(f, &app))?;

        // Handle events with timeout
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => {
                            return Ok(app.result);
                        }
                        KeyCode::Char('p') => {
                            app.toggle_pause();
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
                            if let Some(ref result) = app.result {
                                // Will print JSON after TUI closes
                                println!("{}", output_json(result));
                            }
                        }
                        _ => {}
                    }
                }
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
                        if event::poll(Duration::from_millis(100))? {
                            if let Event::Key(key) = event::read()? {
                                if key.kind == KeyEventKind::Press {
                                    match key.code {
                                        KeyCode::Char('q') | KeyCode::Esc => {
                                            return Ok(app.result);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    app.on_error(e.to_string());
                    terminal.draw(|f| draw(f, &app))?;

                    // Wait for quit
                    loop {
                        if event::poll(Duration::from_millis(100))? {
                            if let Event::Key(key) = event::read()? {
                                if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q')
                                {
                                    return Err(anyhow::anyhow!(app.error.unwrap_or_default()));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
