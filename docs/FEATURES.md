# Features Reference

Comprehensive documentation of xfr features.

## Transport Modes

### TCP (Default)

Standard TCP bulk transfer with configurable options:

```bash
xfr <host>                     # Basic TCP test
xfr <host> -b 100M             # TCP at 100 Mbps
xfr <host> --tcp-nodelay       # Disable Nagle's algorithm
xfr <host> --window 4M         # Set TCP window size
xfr <host> --congestion bbr    # Use BBR congestion control
```

TCP provides:
- Reliable, ordered delivery
- Bitrate pacing (`-b`) with byte-budget sleep approach
- Selectable congestion control (`--congestion cubic`, `--congestion bbr`, `--congestion reno`)
- TCP_INFO statistics (RTT, retransmits, cwnd) — polled live per interval, not just at test end

#### Single-Port TCP (Default)

By default, all TCP connections (control and data) use a single port (5201) via the DataHello protocol. When the client connects data streams, each one sends a `DataHello` message containing the `test_id` and `stream_index` to identify itself to the server. This eliminates the need to open additional ports for data transfer.

If the server detects that a client does not advertise the `single_port_tcp` capability, it falls back to multi-port mode (allocating separate ephemeral ports per stream) for legacy compatibility.

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
xfr <host> --quic              # QUIC transport (short: -Q)
xfr <host> --quic -P 4         # QUIC with 4 multiplexed streams
xfr <host> --quic --psk secret # QUIC with PSK authentication
```

QUIC provides:
- Built-in TLS 1.3 encryption
- Multiplexed streams over single connection
- Connection migration capability
- Head-of-line blocking avoidance

**Security Note**: QUIC encrypts traffic but uses self-signed certificates by default. For authenticated connections, combine with `--psk` to prevent MITM attacks.

### MPTCP (Multi-Path TCP)

Multi-Path TCP enables a single connection to use multiple network paths simultaneously (e.g., WiFi + Ethernet, or multiple WAN links). Requires Linux 5.6+ with `CONFIG_MPTCP=y`.

```bash
# Server
xfr serve --mptcp

# Client
xfr <host> --mptcp                    # MPTCP single stream
xfr <host> --mptcp -P 4               # MPTCP with 4 parallel streams
xfr <host> --mptcp --bidir            # MPTCP bidirectional
xfr <host> --mptcp --congestion bbr   # MPTCP with BBR congestion control
```

MPTCP is transparent at the application layer — all TCP features work unchanged:
- Single-port mode, multi-stream, bidirectional
- TCP_INFO stats (RTT, retransmits, cwnd)
- Congestion control, window size, nodelay
- PSK authentication

**Mixed-mode operation**: If only one side uses `--mptcp`, the kernel falls back to regular TCP automatically. Both sides need `--mptcp` for actual multi-path behavior.

**Platform**: Linux only. Non-Linux platforms receive a clear error message. The kernel must have MPTCP enabled (`CONFIG_MPTCP=y`, default on most modern distros).

## Protocol

### Version and Compatibility

The control protocol version is 1.1. Client and server exchange version information during the Hello handshake. Compatibility is checked by comparing the major version number numerically -- major versions must match exactly, while minor version differences are allowed (backwards compatible).

### Capability Negotiation

Both client and server advertise capabilities in their Hello messages. Current capabilities include: `tcp`, `udp`, `quic`, `multistream`, and `single_port_tcp`. The server inspects the client's capabilities to determine which features to use (e.g., single-port vs. multi-port TCP mode).

### Message Flow

The control channel uses newline-delimited JSON over TCP or QUIC:

```
Client                          Server
  |                               |
  |-------- Hello --------------->|  (control connection)
  |<------- Hello (+ auth?) -----|
  |-------- AuthResponse? ------>|
  |<------- AuthSuccess? --------|
  |-------- TestStart ---------->|
  |<------- TestAck -------------|
  |                               |
  |-------- DataHello ---------->|  (data connection 1, same port)
  |-------- DataHello ---------->|  (data connection 2, same port)
  |      [Data Transfer]          |
  |                               |
  |<------- Interval ------------|  (periodic, on control)
  |<------- Result --------------|
```

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

## Infinite Duration

Run tests indefinitely until manually stopped:

```bash
xfr <host> -t 0                # Run until Ctrl+C or 'q'
xfr <host> -u -t 0 -b 100M     # Continuous UDP at 100 Mbps
```

The TUI shows elapsed time as `Xs/∞`. Press `q` to stop.

Servers can cap infinite requests with `--max-duration`:

```bash
xfr serve --max-duration 300s  # Cap all tests at 5 minutes
```

## Local Bind Address

Bind the client to a specific local IP (useful for multi-homed hosts):

```bash
xfr <host> --bind 192.168.1.100         # Bind to specific IPv4
xfr <host> --bind 10.0.0.1:0            # IP with auto-assigned port
xfr <host> --bind [::1]                 # IPv6 address
```

**Note:** TCP mode does not support explicit port binding (use IP only).

## Client Source Port (`--cport`)

Pin the client's local source port for firewall traversal (UDP and QUIC only):

```bash
xfr <host> -u --cport 5300              # UDP with source port 5300
xfr <host> -u --cport 5300 -P 4         # 4 UDP streams on ports 5300-5303
xfr <host> --quic --cport 5300          # QUIC with source port 5300
xfr <host> --bind 10.0.0.1 --cport 5300 # Combine with --bind for IP + port
```

Multi-stream UDP assigns sequential ports starting from the specified port. QUIC multiplexes all streams on a single port, so only one port is needed regardless of `-P`.

**Not supported with TCP** — TCP already uses single-port mode (all connections go through port 5201), so source port pinning is unnecessary. Stateful firewalls handle TCP ephemeral source ports automatically.

## Dual-Stack and IPv6 Support

The server defaults to dual-stack mode, accepting both IPv4 and IPv6 connections on a single socket. You can restrict to a specific address family:

```bash
xfr serve -4                   # IPv4 only
xfr serve -6                   # IPv6 only
xfr <host> -4                  # Client: force IPv4
xfr <host> -6                  # Client: force IPv6
```

IPv4-mapped IPv6 addresses (`::ffff:x.x.x.x`) are automatically normalized to their IPv4 form when comparing peer IPs. This ensures consistent behavior in dual-stack environments, such as when validating that a DataHello connection comes from the same client as the control connection.

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
xfr serve --one-off          # Exit after one test (TCP or QUIC)
```

The `--one-off` flag causes the server to shut down after a single test completes. This works for both TCP and QUIC connections. When one-off mode is active, the shutdown signal is shared between the TCP accept loop and the QUIC acceptor task, so whichever protocol completes a test first triggers the server exit.

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

### Connection Security

The server implements several protections against connection-based attacks:

- **Slow-loris protection**: Each accepted TCP connection is immediately spawned into its own task. A 5-second initial read timeout (`INITIAL_READ_TIMEOUT`) is applied to the first line read, preventing idle connections from blocking the accept loop.
- **DataHello flood protection**: When a data connection sends a `DataHello`, the server validates the `test_id` against active tests before processing. Unknown test IDs are rejected immediately. The server also verifies that the DataHello originates from the same IP address as the control connection for that test (using IPv4-mapped IPv6 normalization for dual-stack compatibility).
- **Bounded message length**: Control messages are limited to 8192 bytes to prevent memory exhaustion.
- **Handshake timeouts**: A 30-second timeout is applied to control-plane handshake reads after the initial connection.
- **Concurrent handler limit**: A configurable semaphore (default: 100) limits the number of concurrent client handlers.

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

## Update Notifications

xfr checks GitHub for newer versions in the background. If an update is available, a yellow banner appears below the title bar showing:

- Current version → new version
- Appropriate update command (based on how you installed xfr)

Press `u` to dismiss the banner.

The check uses a 1-hour cache to avoid GitHub API rate limits.

## CLI Reference

See `xfr --help` for complete CLI documentation.

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--port` | `-p` | 5201 | Server/client port |
| `--time` | `-t` | 10s | Test duration (use 0 for infinite) |
| `--udp` | `-u` | false | UDP mode |
| `--bitrate` | `-b` | unlimited | Target bitrate for TCP and UDP (e.g., 1G, 100M). 0 = unlimited |
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
| `--congestion` | | OS default | TCP congestion control algorithm (e.g. cubic, bbr, reno) |
| `--quic` | `-Q` | false | Use QUIC transport |
| `--psk` | | none | Pre-shared key |
| `--timestamp-format` | | relative | Timestamp format |
| `--log-file` | | none | Log file path |
| `--log-level` | | info | Log level |
| `--ipv4` | `-4` | false | Force IPv4 only |
| `--ipv6` | `-6` | false | Force IPv6 only |
| `--bind` | | none | Local address to bind (IP or IP:port) |
| `--cport` | | none | Client source port for firewall traversal (UDP/QUIC only) |
| `--mptcp` | | false | MPTCP mode (Multi-Path TCP, Linux 5.6+) |

### Server-Specific Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--tui` | false | Enable server dashboard |
| `--one-off` | false | Exit after one test (TCP or QUIC) |
| `--max-duration` | none | Maximum test duration |
| `--rate-limit` | none | Max concurrent tests per IP |
| `--allow` | none | Allow IP/subnet (repeatable) |
| `--deny` | none | Deny IP/subnet (repeatable) |
| `--acl-file` | none | Load ACL from file |
| `--psk-file` | none | Read PSK from file |
| `--push-gateway` | none | Prometheus Push Gateway URL |
| `--prometheus` | none | Metrics endpoint port |
| `--mptcp` | false | Enable MPTCP for TCP listeners (Linux 5.6+) |
