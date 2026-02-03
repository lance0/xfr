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
- **Server dashboard** - `xfr serve --tui` for monitoring active tests
- **Multi-client server** - handle multiple simultaneous tests
- **TCP, UDP, and QUIC** with configurable bitrate and parallel streams
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
| Live TUI | No | Yes (client & server) |
| Multi-client server | No | Yes |
| Output formats | Text/JSON | Text/JSON/CSV/Prometheus |
| Compare runs | No | `xfr diff` |
| LAN discovery | No | `xfr discover` |
| Config file | No | Yes |

## Real-World Use Cases

### VPN Tunnel Testing
Measure actual throughput through your VPN:
```bash
# On VPN server
xfr serve

# From client, through VPN
xfr 10.8.0.1 -t 30s
```

### UDP Congestion Detection
Test UDP at your expected rate to detect packet loss:
```bash
xfr host -u -b 500M -t 60s    # Watch for loss percentage in TUI
```

### Before/After Comparison
Quantify the impact of network changes:
```bash
xfr host --json -o before.json
# ... make changes ...
xfr host --json -o after.json
xfr diff before.json after.json --threshold 5
```

### Multi-Stream for Bonded Connections
Test aggregate bandwidth across bonded/LACP interfaces:
```bash
xfr host -P 8 -t 30s          # 8 streams to utilize all links
```

### Prometheus Monitoring
Continuous performance monitoring:
```bash
xfr serve --prometheus 9090 --push-gateway http://pushgateway:9091
# Scrape metrics or view in Grafana
```

## Installation

### Quick Install (Linux/macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/lance0/xfr/master/install.sh | sh
```

### From crates.io

```bash
cargo install xfr
```

### From Source

```bash
git clone https://github.com/lance0/xfr
cd xfr && cargo build --release
sudo cp target/release/xfr /usr/local/bin/
```

### Optional Features

| Feature | Default | Description |
|---------|---------|-------------|
| `discovery` | Yes | mDNS LAN discovery (`xfr discover`) |
| `prometheus` | No | Prometheus metrics endpoint and Push Gateway support |

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
xfr serve --tui              # Live dashboard showing active tests
xfr serve --one-off          # Exit after one test
xfr serve --max-duration 60s # Limit test duration
xfr serve --push-gateway http://pushgateway:9091  # Push metrics on test complete
xfr serve --psk mysecret     # Require PSK authentication
xfr serve --rate-limit 2     # Max 2 concurrent tests per IP
xfr serve --allow 192.168.0.0/16 --deny 0.0.0.0/0  # IP ACL
```

### Client

```bash
xfr 192.168.1.1              # TCP test, 10s, single stream
xfr 192.168.1.1 -t 30s       # 30 second test
xfr 192.168.1.1 -P 4         # 4 parallel streams
xfr 192.168.1.1 -R           # Reverse (download test)
xfr 192.168.1.1 --bidir      # Bidirectional
xfr 192.168.1.1 -6           # Force IPv6 only
xfr ::1 -6                   # IPv6 localhost
```

### UDP Mode

```bash
xfr 192.168.1.1 -u           # UDP mode
xfr 192.168.1.1 -u -b 1G     # UDP at 1 Gbps
xfr 192.168.1.1 -u -b 100M   # UDP at 100 Mbps
```

### QUIC Mode

```bash
xfr 192.168.1.1 --quic       # QUIC transport (encrypted)
xfr 192.168.1.1 --quic -P 4  # QUIC with 4 parallel streams
xfr 192.168.1.1 --quic -R    # QUIC download test
```

QUIC provides built-in TLS 1.3 encryption with stream multiplexing over a single connection.

**Security Note:** QUIC encrypts traffic but does not verify server identity by default. For authenticated connections, use `--psk` on both client and server to prevent MITM attacks.

### Output Formats

```bash
xfr host --json              # JSON summary
xfr host --json-stream       # JSON per interval (for scripting)
xfr host --csv               # CSV output
xfr host -q                  # Quiet mode (summary only)
xfr host -o results.json     # Save to file
xfr host --no-tui            # Plain text, no TUI
xfr host --timestamp-format iso8601  # ISO 8601 timestamps
```

**Note:** Log messages go to stderr, allowing clean JSON/CSV piping: `xfr host --json 2>/dev/null`

### Interval Control

```bash
xfr host -i 2                # Report every 2 seconds
xfr host --omit 3            # Skip first 3s of intervals (TCP ramp-up)
```

### Compare Results

```bash
xfr diff baseline.json current.json
xfr diff baseline.json current.json --threshold 5
```

### Discovery

```bash
xfr discover                 # Find xfr servers on LAN
xfr discover --timeout 10s   # Extended search
```

## Keybindings (Client TUI)

| Key | Action |
|-----|--------|
| `q` | Quit (cancels test) |
| `p` | Pause/Resume display |
| `s` | Settings modal |
| `t` | Cycle color theme |
| `d` | Toggle per-stream view |
| `?` / `F1` | Help |
| `j` | Print JSON result |

## Keybindings (Server TUI)

| Key | Action |
|-----|--------|
| `q` | Quit server |
| `?` / `F1` | Help |
| `Esc` | Close help |

## Themes

xfr includes 11 built-in color themes. Select with `--theme` or press `t` during a test:

```bash
xfr host --theme dracula     # Dark purple theme
xfr host --theme matrix      # Green on black hacker style
xfr host --theme catppuccin  # Soothing pastels
xfr host --theme nord        # Arctic blue tones
```

Available themes: `default`, `kawaii`, `cyber`, `dracula`, `monochrome`, `matrix`, `nord`, `gruvbox`, `catppuccin`, `tokyo_night`, `solarized`

Your theme preference is auto-saved to `~/.config/xfr/prefs.toml`.

## Configuration

xfr reads defaults from `~/.config/xfr/config.toml`:

```toml
[client]
duration_secs = 10
parallel_streams = 1
tcp_nodelay = false
json_output = false
no_tui = false
theme = "default"  # or dracula, catppuccin, nord, matrix, etc.
timestamp_format = "relative"  # or "iso8601", "unix"
log_file = "~/.config/xfr/xfr.log"
log_level = "info"

[server]
port = 5201
push_gateway = "http://pushgateway:9091"
log_file = "~/.config/xfr/xfr-server.log"
log_level = "info"
psk = "my-secret-key"
rate_limit = 5
allow = ["192.168.0.0/16", "10.0.0.0/8"]
```

Environment variables override config file:

```bash
export XFR_PORT=9000
export XFR_DURATION=30s
```

## Prometheus Metrics

Enable with `--features prometheus`:

```bash
xfr serve --prometheus 9090
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
| `--quic` | `-Q` | false | QUIC mode (encrypted) |
| `--bitrate` | `-b` | unlimited | Target bitrate for UDP (e.g., 1G, 100M) |
| `--parallel` | `-P` | 1 | Parallel streams |
| `--reverse` | `-R` | false | Reverse direction (download) |
| `--bidir` | | false | Bidirectional test |
| `--ipv4` | `-4` | false | Force IPv4 only |
| `--ipv6` | `-6` | false | Force IPv6 only |
| `--json` | | false | JSON output |
| `--json-stream` | | false | JSON per interval |
| `--csv` | | false | CSV output |
| `--quiet` | `-q` | false | Summary only |
| `--interval` | `-i` | 1.0 | Report interval (seconds) |
| `--omit` | | 0 | Omit first N seconds |
| `--output` | `-o` | stdout | Output file |
| `--no-tui` | | false | Disable TUI |
| `--theme` | | default | Color theme (dracula, nord, matrix, etc.) |
| `--tcp-nodelay` | | false | Disable Nagle algorithm |
| `--window` | | OS default | TCP window size |
| `--timestamp-format` | | relative | Timestamp format (relative, iso8601, unix) |
| `--log-file` | | none | Log file path (e.g., ~/.config/xfr/xfr.log) |
| `--log-level` | | info | Log level (error, warn, info, debug, trace) |
| `--push-gateway` | | none | Prometheus Push Gateway URL (server) |
| `--prometheus` | | none | Prometheus metrics port (server, requires feature) |
| `--psk` | | none | Pre-shared key for authentication |
| `--psk-file` | | none | Read PSK from file |
| `--rate-limit` | | none | Max concurrent tests per IP (server) |
| `--rate-limit-window` | | 60s | Rate limit time window (server) |
| `--completions` | | none | Generate shell completions (bash, zsh, fish, powershell) |
| `--allow` | | none | Allow IP/subnet, repeatable (server) |
| `--deny` | | none | Deny IP/subnet, repeatable (server) |
| `--tui` | | false | Enable live dashboard (server) |
| `--one-off` | | false | Exit after one test (server) |

## Security Considerations

### Transport Encryption

| Mode | Encryption | Certificate Verification |
|------|------------|-------------------------|
| TCP | None | N/A |
| UDP | None | N/A |
| QUIC | TLS 1.3 | Disabled by default |

**QUIC mode** (`-Q/--quic`) provides TLS 1.3 encryption but does not verify server certificates. This is suitable for trusted networks. For untrusted networks, use a VPN or SSH tunnel.

### Authentication

PSK authentication (`--psk`) verifies client identity but does not encrypt TCP/UDP traffic. For encrypted + authenticated connections, use QUIC with PSK:

```bash
# Server
xfr serve --psk "secretkey"

# Client (encrypted + authenticated)
xfr host -Q --psk "secretkey"
```

### Network Considerations

- **UDP on untrusted networks**: UDP mode may be susceptible to reflection attacks from spoofed source addresses. Use TCP or QUIC on public networks.
- **Rate limiting**: Use `--rate-limit` on public servers to prevent abuse.
- **ACLs**: Use `--allow`/`--deny` to restrict client access.

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

- [Comparison with iperf3](docs/COMPARISON.md) - Feature matrix and migration guide
- [Scripting & CI/CD](docs/SCRIPTING.md) - Automation, Docker, Prometheus
- [Features Reference](docs/FEATURES.md) - Detailed feature documentation
- [Architecture](docs/ARCHITECTURE.md) - For contributors
- [Changelog](CHANGELOG.md) - Release history
- [Known Issues](KNOWN_ISSUES.md) - Edge cases and limitations
- [Roadmap](ROADMAP.md) - Planned features
- [Contributing](CONTRIBUTING.md) - Development guidelines

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
