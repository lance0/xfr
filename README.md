# xfr

<p align="center">
  <img src="xfr-logo.png" alt="xfr logo" width="200">
</p>

A fast, modern network bandwidth testing tool with TUI. Built in Rust as an iperf replacement.

[![Crates.io](https://img.shields.io/crates/v/xfr.svg)](https://crates.io/crates/xfr)
[![CI](https://github.com/lance0/xfr/actions/workflows/ci.yml/badge.svg)](https://github.com/lance0/xfr/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)
[![Ko-fi](https://img.shields.io/badge/Ko--fi-tip-ff5e5b?logo=ko-fi)](https://ko-fi.com/lance0)

## Quick Start

```bash
# Server
xfr serve

# Client (in another terminal or machine)
xfr 192.168.1.1              # Basic TCP test
xfr 192.168.1.1 -P 4         # 4 parallel streams
xfr 192.168.1.1 -u -b 1G     # UDP at 1 Gbps
```

## Features

- **Live TUI** with real-time throughput graphs and per-stream stats
- **Multi-client server** - handle multiple simultaneous tests
- **TCP and UDP** with configurable bitrate and parallel streams
- **Bidirectional testing** - measure upload and download simultaneously
- **Multiple output formats** - plain text, JSON, JSON streaming, CSV
- **Result comparison** - `xfr diff` to detect performance regressions
- **LAN discovery** - find xfr servers with mDNS (`xfr discover`)
- **Prometheus metrics** - export stats for monitoring dashboards
- **Config file** - save defaults in `~/.config/xfr/config.toml`
- **Environment variables** - `XFR_PORT`, `XFR_DURATION` overrides

### vs iperf3

| Feature | iperf3 | xfr |
|---------|--------|-----|
| Live TUI | No | Yes |
| Multi-client server | No | Yes |
| Output formats | Text/JSON | Text/JSON/CSV/Prometheus |
| Compare runs | No | `xfr diff` |
| LAN discovery | No | `xfr discover` |
| Config file | No | Yes |

## Installation

### From crates.io (Recommended)

```bash
cargo install xfr
```

### From Source

```bash
git clone https://github.com/lance0/xfr
cd xfr && cargo build --release
sudo cp target/release/xfr /usr/local/bin/
```

### With Optional Features

```bash
# Prometheus metrics support
cargo install xfr --features prometheus

# All features
cargo install xfr --all-features
```

## Usage

### Server

```bash
xfr serve                    # Listen on port 5201
xfr serve -p 9000            # Custom port
xfr serve --one-off          # Exit after one test
xfr serve --max-duration 60s # Limit test duration
```

### Client

```bash
xfr 192.168.1.1              # TCP test, 10s, single stream
xfr 192.168.1.1 -t 30s       # 30 second test
xfr 192.168.1.1 -P 4         # 4 parallel streams
xfr 192.168.1.1 -R           # Reverse (download test)
xfr 192.168.1.1 --bidir      # Bidirectional
```

### UDP Mode

```bash
xfr 192.168.1.1 -u           # UDP mode
xfr 192.168.1.1 -u -b 1G     # UDP at 1 Gbps
xfr 192.168.1.1 -u -b 100M   # UDP at 100 Mbps
```

### Output Formats

```bash
xfr host --json              # JSON summary
xfr host --json-stream       # JSON per interval (for scripting)
xfr host --csv               # CSV output
xfr host -q                  # Quiet mode (summary only)
xfr host -o results.json     # Save to file
xfr host --no-tui            # Plain text, no TUI
```

### Interval Control

```bash
xfr host -i 2                # Report every 2 seconds
xfr host --omit 3            # Skip first 3s of intervals (TCP ramp-up)
```

### Compare Results

```bash
xfr diff baseline.json current.json
xfr diff baseline.json current.json --threshold 5%
```

### Discovery

```bash
xfr discover                 # Find xfr servers on LAN
xfr discover --timeout 10s   # Extended search
```

## Keybindings (TUI)

| Key | Action |
|-----|--------|
| `q` | Quit (cancels test) |
| `p` | Pause/Resume display |
| `?` / `F1` | Help |
| `j` | Print JSON result |

## Configuration

xfr reads defaults from `~/.config/xfr/config.toml`:

```toml
[client]
duration_secs = 10
parallel_streams = 1
tcp_nodelay = false
json_output = false
no_tui = false

[server]
port = 5201
```

Environment variables override config file:

```bash
export XFR_PORT=9000
export XFR_DURATION=30s
```

## Prometheus Metrics

Enable with `--features prometheus`:

```bash
xfr serve --prometheus-port 9090
```

Metrics available at `http://localhost:9090/metrics`:

- `xfr_bytes_total` - Total bytes transferred
- `xfr_throughput_mbps` - Current throughput
- `xfr_active_tests` - Number of active tests
- `xfr_retransmits_total` - TCP retransmissions

See `examples/grafana-dashboard.json` for a sample Grafana dashboard.

## CLI Reference

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--port` | `-p` | 5201 | Server/client port |
| `--time` | `-t` | 10s | Test duration |
| `--udp` | `-u` | false | UDP mode |
| `--bitrate` | `-b` | unlimited | Target bitrate (e.g., 1G, 100M) |
| `--parallel` | `-P` | 1 | Parallel streams |
| `--reverse` | `-R` | false | Reverse direction (download) |
| `--bidir` | | false | Bidirectional test |
| `--json` | | false | JSON output |
| `--json-stream` | | false | JSON per interval |
| `--csv` | | false | CSV output |
| `--quiet` | `-q` | false | Summary only |
| `--interval` | `-i` | 1.0 | Report interval (seconds) |
| `--omit` | | 0 | Omit first N seconds |
| `--output` | `-o` | stdout | Output file |
| `--no-tui` | | false | Disable TUI |
| `--tcp-nodelay` | | false | Disable Nagle algorithm |
| `--window` | | OS default | TCP window size |

## Platform Support

| Platform | Status |
|----------|--------|
| Linux | Full support, TCP_INFO stats |
| macOS | Full support, TCP_INFO stats |
| Windows | Via WSL2 |

## Troubleshooting

### Permission denied on port 5201

Use a port above 1024 or run with elevated privileges:

```bash
xfr serve -p 9000
```

### Connection refused

Ensure the server is running and the port is not blocked by a firewall.

### Low throughput

- Try multiple parallel streams: `-P 4`
- Disable Nagle's algorithm: `--tcp-nodelay`
- Increase TCP window size: `--window 4M`

### UDP packet loss

- Reduce bitrate: `-b 500M`
- Check for network congestion or firewall issues

## Documentation

- [Changelog](CHANGELOG.md) - Release history
- [Roadmap](ROADMAP.md) - Planned features
- [Contributing](CONTRIBUTING.md) - Development guidelines

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
