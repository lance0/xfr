# Features Reference

Comprehensive documentation of xfr features.

## Transport Modes

### TCP (Default)

Standard TCP bulk transfer with configurable options:

```bash
xfr <host>                     # Basic TCP test
xfr <host> --tcp-nodelay       # Disable Nagle's algorithm
xfr <host> --window 4M         # Set TCP window size
```

TCP provides:
- Reliable, ordered delivery
- Congestion control
- TCP_INFO statistics (RTT, retransmits, cwnd)

### UDP

Unreliable datagram transfer for testing network capacity:

```bash
xfr <host> -u                  # UDP mode
xfr <host> -u -b 1G            # UDP at 1 Gbps
xfr <host> -u -b 0             # UDP unlimited (flood test - use carefully)
```

UDP provides:
- Configurable bitrate pacing
- Jitter calculation (RFC 3550)
- Packet loss detection
- Out-of-order detection

**Note**: UDP is not congestion-controlled. High bitrates can cause network congestion.

### QUIC

Encrypted transport over UDP using TLS 1.3:

```bash
xfr <host> --quic              # QUIC transport
xfr <host> --quic -P 4         # QUIC with 4 multiplexed streams
xfr <host> --quic --psk secret # QUIC with PSK authentication
```

QUIC provides:
- Built-in TLS 1.3 encryption
- Multiplexed streams over single connection
- Connection migration capability
- Head-of-line blocking avoidance

**Security Note**: QUIC encrypts traffic but uses self-signed certificates by default. For authenticated connections, combine with `--psk` to prevent MITM attacks.

## Multi-Stream Testing

Test with parallel streams to utilize multiple CPU cores or aggregated links:

```bash
xfr <host> -P 4                # 4 parallel streams
xfr <host> -P 8                # 8 parallel streams
```

Each stream reports individual statistics. Aggregate throughput is also shown.

Use cases:
- Bonded/LACP interface testing
- Multi-core CPU utilization
- Identifying per-flow rate limits

## Bidirectional Mode

Test upload and download simultaneously:

```bash
xfr <host> --bidir             # Bidirectional
xfr <host> --bidir -P 2        # Bidir with 2 streams each direction
```

Reports separate statistics for upload and download.

## Direction Control

```bash
xfr <host>                     # Upload (client → server)
xfr <host> -R                  # Reverse/download (server → client)
xfr <host> --bidir             # Both directions
```

## Output Formats

### Plain Text (Default)

Human-readable output with optional TUI:

```bash
xfr <host>                     # TUI with live graphs
xfr <host> --no-tui            # Plain text output
xfr <host> -q                  # Quiet mode (summary only)
```

### JSON

Machine-readable JSON output:

```bash
xfr <host> --json              # JSON summary at end
xfr <host> --json-stream       # JSON per interval (one object per line)
xfr <host> -o result.json      # Save to file
```

### CSV

Spreadsheet-compatible output:

```bash
xfr <host> --csv               # CSV output
xfr <host> --csv -o data.csv   # Save to file
```

### Timestamps

Control timestamp format in output:

```bash
xfr <host> --timestamp-format relative   # "1.0s", "2.0s" (default)
xfr <host> --timestamp-format iso8601    # "2024-01-15T10:30:00Z"
xfr <host> --timestamp-format unix       # "1705316400"
```

## Configuration

### Config File

xfr reads defaults from `~/.config/xfr/config.toml`:

```toml
[client]
duration_secs = 10
parallel_streams = 1
tcp_nodelay = false
json_output = false
no_tui = false
theme = "default"
timestamp_format = "relative"
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

### Environment Variables

Environment variables override config file settings:

| Variable | Description |
|----------|-------------|
| `XFR_PORT` | Server/client port |
| `XFR_DURATION` | Test duration (e.g., "30s") |
| `XFR_LOG_LEVEL` | Log level (error, warn, info, debug, trace) |
| `XFR_LOG_FILE` | Log file path |

### User Preferences

Theme preference is saved to `~/.config/xfr/prefs.toml`:

```toml
theme = "dracula"
```

Press `t` during a test to cycle themes. Your choice is auto-saved.

## Server Features

### Multi-Client Support

Unlike iperf3, xfr's server handles multiple simultaneous clients:

```bash
xfr serve                    # Accept unlimited clients
xfr serve --one-off          # Exit after one test
```

### Server TUI Dashboard

Monitor active tests in real-time:

```bash
xfr serve --tui              # Live dashboard
```

Shows:
- Active test count
- Per-client throughput
- Connection duration
- Protocol and direction

### Rate Limiting

Limit concurrent tests per IP:

```bash
xfr serve --rate-limit 2     # Max 2 concurrent tests per IP
```

### Access Control Lists

Restrict which IPs can connect:

```bash
xfr serve --allow 192.168.0.0/16           # Only allow local network
xfr serve --allow 10.0.0.0/8 --deny 0.0.0.0/0   # Allow 10.x, deny all else
xfr serve --acl-file /etc/xfr/acl.txt      # Load ACL from file
```

ACL file format:
```
allow 192.168.0.0/16
allow 10.0.0.0/8
deny 0.0.0.0/0
```

Rules are evaluated in order. First match wins.

### PSK Authentication

Require pre-shared key for connections:

```bash
# Server
xfr serve --psk mysecretkey
xfr serve --psk-file /etc/xfr/psk.txt

# Client
xfr <host> --psk mysecretkey
```

Authentication uses HMAC-SHA256 challenge-response.

### Duration Limits

Limit maximum test duration server-side:

```bash
xfr serve --max-duration 60s   # Cap tests at 60 seconds
```

## Prometheus Metrics

Build with `--features prometheus` for metrics support.

### Metrics Endpoint

```bash
xfr serve --prometheus 9090
```

Available at `http://localhost:9090/metrics`:

```
xfr_bytes_total{direction="upload"} 1234567890
xfr_throughput_mbps{direction="upload"} 987.65
xfr_active_tests 2
xfr_retransmits_total 42
```

### Push Gateway

Push metrics on test completion:

```bash
xfr serve --push-gateway http://pushgateway:9091
```

## LAN Discovery

Find xfr servers on the local network using mDNS:

```bash
xfr discover                 # Scan for servers
xfr discover --timeout 10s   # Extended search
```

Servers automatically register with mDNS when started.

## Result Comparison

Compare test results to detect regressions:

```bash
xfr diff baseline.json current.json
xfr diff baseline.json current.json --threshold 5%
```

Exit code:
- 0: Within threshold
- 1: Regression detected

## Logging

Enable file logging for debugging:

```bash
xfr serve --log-file /var/log/xfr.log --log-level debug
xfr <host> --log-file ./test.log --log-level trace
```

Log levels: `error`, `warn`, `info`, `debug`, `trace`

Logs rotate daily when using a log file.

## CLI Reference

See `xfr --help` for complete CLI documentation.

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
| `--theme` | | default | Color theme |
| `--tcp-nodelay` | | false | Disable Nagle algorithm |
| `--window` | | OS default | TCP window size |
| `--quic` | | false | Use QUIC transport |
| `--psk` | | none | Pre-shared key |
| `--timestamp-format` | | relative | Timestamp format |
| `--log-file` | | none | Log file path |
| `--log-level` | | info | Log level |

### Server-Specific Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--tui` | false | Enable server dashboard |
| `--one-off` | false | Exit after one test |
| `--max-duration` | none | Maximum test duration |
| `--rate-limit` | none | Max concurrent tests per IP |
| `--allow` | none | Allow IP/subnet (repeatable) |
| `--deny` | none | Deny IP/subnet (repeatable) |
| `--acl-file` | none | Load ACL from file |
| `--psk-file` | none | Read PSK from file |
| `--push-gateway` | none | Prometheus Push Gateway URL |
| `--prometheus` | none | Metrics endpoint port |
