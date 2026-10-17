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
xfr <host> --random            # Random payload (defeat compression/dedup bias)
```

TCP provides:
- Reliable, ordered delivery
- Bitrate pacing (`-b`) — on Linux, uses kernel `SO_MAX_PACING_RATE` with the FQ scheduler for precise per-packet timing; falls back to userspace byte-budget sleep pacing on other platforms or MPTCP sockets. Note: `-b` sets a global bitrate shared across all parallel streams (unlike iperf3 where `-b` is per-stream)
- Selectable congestion control (`--congestion cubic`, `--congestion bbr`, `--congestion reno`)
- TCP_INFO statistics (RTT, retransmits, cwnd) — polled live per interval, not just at test end
- High stream-count teardown resilience — client stops local data loops at local test end and scales teardown wait time with stream count. TCP receivers briefly drain after cancel to cover cross-peer cancel latency. On Linux, TCP senders use `SO_LINGER=0` (abortive close) at end-of-test and clamp `bytes_sent` to `tcpi_bytes_acked` so that bufferbloat-discarded tail bytes are not reported as sent; macOS falls back to graceful `shutdown()` since it lacks the `bytes_acked` counter needed to correct for overcount

#### Single-Port TCP (Default)

By default, all TCP connections (control and data) use a single port (5201) via the DataHello protocol. When the client connects data streams, each one sends a `DataHello` message containing the `test_id` and `stream_index` to identify itself to the server. This eliminates the need to open additional ports for data transfer.

For high stream counts on constrained links, single-port setup is hardened: the client limits concurrent `connect + DataHello` handshakes (max 16 in flight) and the server adapts initial first-line read timeout to active stream count (capped at 20s). This reduces handshake-loss-driven mid-test stream failures.

If the server detects that a client does not advertise the `single_port_tcp` capability, it falls back to multi-port mode (allocating separate ephemeral ports per stream) for legacy compatibility.

### UDP

Unreliable datagram transfer for testing network capacity:

```bash
xfr <host> -u                  # UDP mode
xfr <host> -u -b 1G            # UDP at 1 Gbps
xfr <host> -u -b 0             # UDP unlimited (flood test - use carefully)
xfr <host> -u --random         # Random UDP payload bytes
xfr <host> -u -l 1368          # 1368-byte datagrams (fits a 1420-byte tunnel MTU)
```

UDP provides:
- Configurable bitrate pacing
- Configurable datagram size (`-l/--length`, 16–65507 bytes including the 16-byte xfr header; default 1400). Pick a size that fits the path MTU — a 1400-byte payload fragments on any path MTU below 1428 bytes (1448 over IPv6), such as WireGuard's 1420 default (issue #213). `--probe-mtu` finds the largest size that survives each direction. Requires a server advertising `udp_payload_size_v1`; older servers are refused rather than silently running at 1400
- Jitter calculation (RFC 3550)
- Packet loss detection with gap repair for unique late arrivals within the latest 65,536 sequence numbers (8 KiB of tracking memory per receiver); duplicate datagrams cannot dilute loss
- Out-of-order detection for unique late packets, excluding duplicates. Packets older than the tracking window cannot repair loss; wire byte counts still include all data datagrams
- Live receiver-progress feedback at 2 Hz on the data socket (when both peers advertise `udp_feedback_v1`), so the live loss counter stays current under upload-mode saturation even when the TCP control channel back-pressures. Surfaces in the TUI live counter and in `--no-tui --json-stream` / `--csv` / plain interval output. Freshness checks compare both unique received packets and the expected packet count, keeping delayed reports from undoing gap repairs

`-l/--length` sets the complete UDP payload size, including xfr's 16-byte
sequence/timestamp header (16–65507 bytes; default 1400). It applies in both
directions and across all streams. Both peers must support `udp_payload_size_v1`;
an explicit size fails with an upgrade message against an older server.

Choose a size that fits the path: allow 28 bytes for IPv4 + UDP headers or
48 bytes for IPv6 + UDP headers, plus any IP options or extension headers.
For example, 1368 fits a 1420-byte MTU with either address family's basic headers.
Use `--probe-mtu` separately to find the largest payload that survives each
direction, then pass the smaller result to `-l`. The default remains 1400;
throughput tests do not automatically probe or adjust the size. JSON
`udp_stats.packet_size` reports the largest valid datagram received, or is absent
when no data arrived.

`--probe-mtu` sets the don't-fragment flag so middleboxes can't hide the limit
by fragmenting, which makes it useful for NFS-over-UDP and VPN/tunnel tuning:

```bash
xfr <host> --probe-mtu
```

#### Single-Port UDP (Default, v0.9.18+)

When both peers advertise `single_port_udp_v1`, all per-stream UDP data flows to the server's main port (5201/UDP by default) instead of per-stream ephemeral server ports — the UDP firewall story matches single-port TCP. Per stream, the client sends a token-bearing hello datagram to the main port; the server creates a connected same-port socket (`SO_REUSEPORT`) for that stream and acks from it, after which the kernel routes the stream's traffic to its dedicated socket. The server advertises the capability only after a startup self-test proves the running kernel's connected-socket routing; on unsupported platforms (e.g. Windows) or against older peers, it falls back to ephemeral per-stream ports automatically.

**Note**: UDP is not congestion-controlled. High bitrates can cause network congestion.

### Payload Pattern

Both client and server use random payloads by default for TCP and UDP, avoiding inflated results from WAN optimizers, compression offload, and dedup hardware. Buffers are filled with random bytes once at allocation time (not per write), so the CPU cost is negligible.

- `--zeros` forces zero-filled client-sent payloads for compression/dedup testing.
- `--zeros` only affects client-sent traffic; the server always sends random. Payload mode is not negotiated over the wire (future enhancement).
- QUIC payloads are ignored since QUIC encrypts all traffic via TLS 1.3.

### Zero-Copy Sends (`--zerocopy`, Linux)

TCP sends can skip the per-write userspace-to-kernel copy with `-Z`/`--zerocopy` (issue #33), which serves the payload from an anonymous in-memory file via `sendfile(2)` — the same mechanism as iperf3's `-Z`. On constrained CPUs (embedded routers, SBCs) the copy itself is often the bottleneck, so this raises the throughput ceiling and lowers CPU load at any given rate.

- Client-sent traffic uses zero-copy directly; for `-R`/`--bidir`, the request is forwarded in `TestStart` and servers that advertise the `zerocopy_v1` capability apply it to their sends (older servers ignore it, and the client warns).
- Works with MPTCP (`sendfile` goes through the regular splice path, unlike `MSG_ZEROCOPY`).
- Falls back to regular writes — with a warning, never a failed test — on non-Linux platforms, kernels without `memfd_create`, or sockets that reject `sendfile`.
- Payload semantics are unchanged: the in-memory file is filled once with the same random (or `--zeros`) pattern the regular path uses.

### QUIC

Encrypted transport over UDP using TLS 1.3:

```bash
xfr <host> --quic              # QUIC transport (short: -Q)
xfr <host> --quic -P 4         # QUIC with 4 multiplexed streams
xfr <host> --quic -R           # QUIC download test
xfr <host> --quic --psk secret # PSK-authenticated QUIC
```

QUIC provides:
- Built-in TLS 1.3 encryption
- Multiplexed streams over single connection
- Connection migration capability
- Head-of-line blocking avoidance

**Security Note**: Without a PSK, QUIC encrypts traffic but does not verify the
self-signed server certificate. With a PSK, xfr binds mutual authentication and
protected-control keys to the exact TLS connection using the RFC 9266 exporter,
so a terminating relay cannot splice the handshake onto a different connection.
Both peers must support `quic_channel_binding_v1`; QUIC + PSK fails closed
otherwise. The self-signed certificate is still not a public PKI identity.

### MPTCP (Multi-Path TCP)

Multi-Path TCP enables a single connection to use multiple network paths simultaneously (e.g., WiFi + Ethernet, or multiple WAN links). Requires Linux 5.6+ with `CONFIG_MPTCP=y`.

```bash
# Server — MPTCP is automatic, no flag needed
xfr serve

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

**Server auto-MPTCP**: The server always tries to create MPTCP listeners. MPTCP listeners accept both MPTCP and regular TCP clients transparently — the kernel handles the fallback. If the kernel doesn't support MPTCP, the server silently falls back to regular TCP. No `--mptcp` flag is needed on the server side.

**Client `--mptcp`**: The client must explicitly opt in with `--mptcp` to request MPTCP connections. Both sides need MPTCP-capable sockets for actual multi-path behavior; otherwise the kernel falls back to regular TCP.

**Platform**: Linux only. Non-Linux platforms receive a clear error message on the client. The server silently falls back to TCP on non-Linux. The kernel must have MPTCP enabled (`CONFIG_MPTCP=y`, default on most modern distros).

## Protocol

### Version and Compatibility

The control protocol version is 1.1. Client and server exchange version information during the Hello handshake. Compatibility is checked by comparing the major version number numerically -- major versions must match exactly, while minor version differences are allowed (backwards compatible).

### Capability Negotiation

Both client and server advertise capabilities in their Hello messages. Current capabilities include: `tcp`, `udp`, `quic`, `multistream`, `single_port_tcp`, `pause_resume`, `udp_feedback_v1`, `zerocopy_v1`, `mtu_probe_v1`, `single_port_udp_v1`, `protected_control_v1`, `quic_channel_binding_v1`, `byte_budget_v1`, and `udp_payload_size_v1`. The server inspects the client's capabilities to determine which features to use (e.g., single-port vs. multi-port mode); a feature activates only when both peers advertise it. Optional capabilities fall back to the prior behavior with older peers. Mandatory capabilities fail closed instead: PSK requires `protected_control_v1`, QUIC + PSK additionally requires `quic_channel_binding_v1`, byte-budget tests require `byte_budget_v1`, and an explicit UDP payload size (`-l`) requires `udp_payload_size_v1`. Both sides of a QUIC + PSK deployment must therefore be upgraded together. The server may withhold an optional capability it can't honor at runtime (e.g. `single_port_udp_v1` when the startup self-test fails).

The `udp_feedback_v1` capability (added in v0.9.14) lets the server send 36-byte cumulative-counts packets back to the client at 2 Hz on the same UDP data socket, providing live UDP loss visibility without depending on the TCP control channel that may be competing for ACKs against a saturated UDP uplink.

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

Plain text summary shows per-direction bytes and throughput plus the
combined total:

```
  Transfer:    Send: 5.00 GB    Recv: 7.00 GB    (Total: 12.00 GB)
  Throughput:  Send: 40.0 Gbps  Recv: 56.0 Gbps  (Total: 96.0 Gbps)
```

JSON adds `bytes_sent`, `bytes_received`, `throughput_send_mbps`, and
`throughput_recv_mbps` fields; CSV gets four matching columns; the TUI
shows `↑` / `↓` throughput in the stats panel. Unidirectional tests
continue to report only the combined number (which equals the
single-direction throughput).

Bidirectional UDP (`-u --bidir`) also reports upload and download loss and
jitter separately. Upload measurements come from the server receiver;
download measurements come from the client receiver and keep updating on a
local clock when control reports stall. Final JSON contains `udp_stats_send`
and `udp_stats_recv`; `udp_stats` retains its upload meaning for existing
scripts. Stream intervals and final rows include the same client-relative
byte/rate splits as the aggregate. Against older servers, missing per-stream
upload splits remain absent; download splits still use local receive counters.

UDP loss counts sequence gaps observed by each receiver. Completely unseen
trailing packets cannot be inferred from these measurements.

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

### Early Exit Summary

Ctrl+C during any test (infinite or finite) triggers a graceful cancel and displays the full test summary with whatever stats have accumulated — rather than silently killing the process. Works in both plain text and TUI modes. A second Ctrl+C force-exits immediately: without the TUI it exits with code 130; in the TUI it stops the test without waiting for the summary, prints `Test cancelled.` (or the result, if one had already arrived) and exits with code 0. If the server is unreachable, a partial summary is printed from the client's cumulative counters instead of saving a synthetic result to `--output`.

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
xfr <host> --bind fe80::1%eth0          # IPv6 link-local (zone required)
```

**Note:** Plain `--bind IP:PORT` is still not supported for TCP control sockets. Use `--bind IP` for source-IP selection, or combine `--bind IP --cport PORT` to pin TCP data-stream source ports.

## Client Source Port (`--cport`)

Pin the client's local source port for firewall traversal:

```bash
xfr <host> --cport 5300                 # TCP data stream with source port 5300
xfr <host> --cport 5300 -P 4            # 4 TCP data streams on ports 5300-5303
xfr <host> -u --cport 5300              # UDP with source port 5300
xfr <host> -u --cport 5300 -P 4         # 4 UDP streams on ports 5300-5303
xfr <host> --quic --cport 5300          # QUIC with source port 5300
xfr <host> --bind 10.0.0.1 --cport 5300 # Combine with --bind for IP + port
```

Multi-stream TCP and UDP assign sequential ports starting from the specified port. QUIC multiplexes all streams on a single port, so only one port is needed regardless of `-P`.

For TCP, `--cport` only applies to data streams. The control connection still uses an ephemeral source port, and xfr fails fast if that ephemeral port overlaps the requested TCP data-port range.

## DSCP / TOS Marking (`--dscp`)

Mark client TCP or UDP sockets for QoS / DSCP testing:

```bash
xfr <host> --dscp EF                    # Expedited Forwarding (46 << 2)
xfr <host> -u --dscp AF31               # Assured Forwarding class 3 drop precedence 1
xfr <host> --dscp 184                   # Raw TOS byte value
```

`--dscp` accepts either a raw TOS byte (`0-255`) or DSCP names such as `EF`, `AF11-AF43`, `CS0-CS7`, and `VA`.

- TCP and UDP apply the marking on client sockets after connect and on server
  sockets for server-sent download/bidirectional traffic.
- QUIC ignores `--dscp` because the underlying UDP socket is owned by Quinn.
- On non-Unix platforms, xfr currently warns instead of applying socket marking.

## Dual-Stack and IPv6 Support

The server defaults to dual-stack mode, accepting both IPv4 and IPv6 connections on a single socket. You can restrict to a specific address family:

```bash
xfr serve -4                   # IPv4 only
xfr serve -6                   # IPv6 only
xfr <host> -4                  # Client: force IPv4
xfr <host> -6                  # Client: force IPv6
xfr ::1 -6                     # IPv6 localhost
```

IPv6 link-local addresses need a zone naming the interface, as in `xfr fe80::2%eth0` or `xfr [fe80::2%eth0]`; a numeric interface index (`%3`) also works, and is the only form accepted on non-Unix platforms. The zone is carried through to every control, data and QUIC socket, which makes a direct cable between two machines testable with no IP configuration at all. `xfr discover` prints link-local answers with their zone. `xfr serve --bind` takes a plain IP, so serve link-local clients from the default wildcard listener.

IPv4-mapped IPv6 addresses (`::ffff:x.x.x.x`) are automatically normalized to their IPv4 form when comparing peer IPs. This ensures consistent behavior in dual-stack environments, such as when validating that a DataHello connection comes from the same client as the control connection.

## Output Formats

### Plain Text (Default)

Human-readable output with optional TUI:

```bash
xfr <host>                     # TUI with live graphs
xfr <host> --no-tui            # Plain text output
xfr <host> -q                  # Quiet mode (summary only)
```

The client TUI and `xfr serve --tui` report test status to the terminal through OSC 7501 ([Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status)): working with a progress percentage and the current rate, paused, done with the summary, or the error. Terminals that do not implement it ignore the sequence. Plain, JSON and CSV output never emit it.

Log messages go to stderr, so JSON and CSV output pipe cleanly:
`xfr <host> --json 2>/dev/null`.

### JSON

Machine-readable JSON output:

```bash
xfr <host> --json              # JSON summary at end
xfr <host> --json-stream       # JSON per interval (one object per line)
xfr <host> -o result.json      # Save to file
```

`--json-stream` cannot be combined with `-q`, which would suppress its
interval lines; use `--json` for the summary alone.

`-o` replaces an existing regular file atomically: the result is written to a
temporary file in the same directory and renamed over the destination, so a
failed save leaves the previous file intact. The directory must be writable.
Devices and pipes such as `/dev/null` are written in place.

### CSV

Spreadsheet-compatible output:

```bash
xfr <host> --csv               # CSV output
xfr <host> --csv -o data.csv   # Save the CSV header and summary row
```

### Timestamps

Control timestamp format in output:

```bash
xfr <host> --timestamp-format relative   # "1.0s", "2.0s" (default)
xfr <host> --timestamp-format iso8601    # "2024-01-15T10:30:00Z"
xfr <host> --timestamp-format unix       # "1705316400"
```

### Interval Control

```bash
xfr <host> -i 2                # Report every 2 seconds
xfr <host> --omit 3            # Skip first 3s of intervals (TCP ramp-up)
```


## Configuration

### Config File

xfr reads defaults from:
- **Linux:** `$XDG_CONFIG_HOME/xfr/config.toml`, or `~/.config/xfr/config.toml` when `XDG_CONFIG_HOME` is unset
- **macOS:** `~/Library/Application Support/xfr/config.toml`
- **Windows:** `%APPDATA%\xfr\config.toml`

```toml
[client]
duration_secs = 10
parallel_streams = 1          # 1-128; explicit -P 1 overrides the config default
tcp_nodelay = false
window_size = "1M"           # TCP/UDP socket buffer size (SO_SNDBUF/SO_RCVBUF), e.g. "512K", "2M"
# bitrate = "100M"            # target bitrate; accepts the same units as --bitrate
# congestion = "bbr"          # TCP congestion-control algorithm
# dscp = "EF"                 # DSCP name or raw TOS byte (0-255)
interval_secs = 1.0           # report interval; default is 1.0 seconds
json_output = false
no_tui = false
theme = "default"            # or dracula, catppuccin, nord, matrix, etc.
timestamp_format = "relative" # or "iso8601", "unix"
address_family = "dual"      # "ipv4", "ipv6", or "dual"
omit_secs = 0                # omit first N seconds (TCP ramp-up)
# bind = "192.168.1.100"      # local address, optionally with :port
# cport = 5202                # client source port for firewall traversal
psk = "my-secret-key"
log_file = "~/.config/xfr/xfr.log"
log_level = "info"

[server]
port = 5201
one_off = false
no_mdns = false
address_family = "dual"      # "ipv4", "ipv6", or "dual"
psk = "my-secret-key"
rate_limit = 5
rate_limit_window = 60       # whole seconds; must be at least 1
allow = ["192.168.0.0/16", "10.0.0.0/8"]
deny = []
acl_file = "/path/to/acl.txt"
push_gateway = "http://pushgateway:9091"
log_file = "~/.config/xfr/xfr-server.log"
log_level = "info"

[[presets]]                  # selected with `xfr serve --preset internal`
name = "internal"
allowed_clients = ["192.168.1.0/24"]  # extra allowlist on top of allow/deny
max_duration_secs = 60       # applies when --max-duration is not given
```

```bash
xfr serve --preset internal  # Apply the [[presets]] entry named "internal"
```

Unknown preset names are rejected.

On Unix, a config containing either `psk` must grant no access to group or
other users, so the shared key is not exposed to another local account:

```bash
# Linux
chmod 600 "${XDG_CONFIG_HOME:-$HOME/.config}/xfr/config.toml"

# macOS
chmod 600 "$HOME/Library/Application Support/xfr/config.toml"
```

The value-taking transport and output settings under `[client]` are defaults: an explicit CLI value takes precedence. This includes `parallel_streams`, `omit_secs`, and `interval_secs`, so `-P 1`, `--omit 0`, and `--interval 1.0` override the config file. `parallel_streams` must be between 1 and 128. `window_size` uses the same bytes or `K`/`M`/`G` suffixes as `--window` (for example `512K` or `1M`); invalid effective values are rejected instead of silently using kernel defaults. `bitrate` accepts the same `100M`/`1G` syntax as `--bitrate`; `dscp` accepts the same DSCP names and numeric values as `--dscp`.

The `address_family` setting accepts `ipv4`, `ipv6`, or `dual`, including existing
aliases such as `4`, `6`, and `both`. Unknown effective values are rejected.
`-4`/`--ipv4` and `-6`/`--ipv6` override config defaults and cannot be used together.
A specific server `--bind` address also selects its address family; any `-4`/`-6`
flag must agree with that address.

Environment variables override config file:

```bash
export XFR_PORT=9000
export XFR_DURATION=30s
```

For `xfr serve`, `--port` takes precedence over `XFR_PORT`, which takes precedence over
`server.port` in the config file. If none is set, the server uses port 5201. An explicit
port of 5201 also overrides the config file.

### Environment Variables

Environment variables override config file settings:

| Variable | Description |
|----------|-------------|
| `XFR_PORT` | Server/client port |
| `XFR_DURATION` | Test duration (e.g., "30s") |
| `XFR_LOG_LEVEL` | Log level (error, warn, info, debug, trace) |
| `XFR_LOG_FILE` | Log file path |
| `XFR_BYTES` | Byte count for a fixed-size transfer (`-n`) |
| `XFR_PSK` | Pre-shared key (visible in process metadata; prefer `--psk-file`) |
| `XFR_PUSH_GATEWAY` | Prometheus push gateway URL (server) |
| `XFR_TIMESTAMP_FORMAT` | Timestamp format (relative, iso8601, unix) |

### User Preferences

Theme preference is saved beside `config.toml` as `xfr/prefs.toml` in the same
platform configuration directory:

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
xfr serve --allow 10.0.0.0/8 --deny 10.42.0.0/16 # Allow 10.x except 10.42.x
xfr serve --acl-file /etc/xfr/acl.txt      # Load ACL from file
```

ACL file format:
```
allow 192.168.0.0/16
allow 10.0.0.0/8
deny 192.168.1.0/24
```

Deny rules always take precedence. If any allow rules exist, addresses that
match none of them are rejected; with no allow rules, otherwise-unmatched
addresses are accepted. Rule order does not change the result.

### PSK Authentication

Require pre-shared key for connections:

```bash
# Server
xfr serve --psk mysecretkey
xfr serve --psk-file /etc/xfr/psk.txt

# Client
xfr <host> --psk mysecretkey
```

Authentication uses HMAC-SHA256 challenge-response. Once both peers are
authenticated, post-auth control messages use ChaCha20-Poly1305. On QUIC, the
authentication and control keys are also bound to the exact TLS connection via
the RFC 9266 exporter; both peers must support `quic_channel_binding_v1`.

> **Security note:** Values supplied via `--psk` or the `XFR_PSK` environment
> variable are visible through process metadata (`ps`, `/proc/<pid>/environ`,
> shell history). Prefer `--psk-file` with a file readable only by the owner.

### Duration Limits

Limit maximum test duration server-side:

```bash
xfr serve --max-duration 60s   # Cap tests at 60 seconds
```

## Prometheus Metrics

Build with `--features prometheus` for the metrics HTTP endpoint. Push Gateway
support is available without this optional feature.

### Metrics Endpoint

```bash
xfr serve --prometheus 9090
```

The endpoint accepts one request per connection (HTTP keep-alive is disabled).
Request headers must arrive within 5 seconds total, and each connection closes
within 10 seconds total, including response writes. At most 64 connections
are served at once; further connections wait in the accept backlog. An accept
error, such as running out of file descriptors, is logged and retried after
100 ms instead of stopping the endpoint. `/health` returns `OK`.

Available at `http://localhost:9090/metrics`:

```
xfr_bytes_total 1234567890
xfr_throughput_mbps 987.65
xfr_tests_total 3
xfr_test_duration_seconds_count 3
xfr_active_tests 2
xfr_stream_bytes_total{test_id="abc",stream_id="0"} 1234567890
xfr_stream_throughput_mbps{test_id="abc",stream_id="0"} 987.65
xfr_stream_retransmits_total{test_id="abc",stream_id="0"} 42
xfr_tcp_rtt_microseconds{test_id="abc"} 1200
xfr_tcp_retransmits_total{test_id="abc"} 42
```

Series labelled with `test_id` keep a test's final values after it ends, so a
scrape that runs after a short test still sees it. The server keeps them for
the 16 most recently finished tests, including tests that errored or were
cancelled, and removes older tests' series. A running test's series are never
removed. At one test every 10 seconds a finished test stays visible for about
160 seconds, which is longer than common scrape intervals. If tests finish
faster than your scraper polls, some per-test series can be missed. The
aggregate `xfr_bytes_total`, `xfr_tests_total` and `xfr_test_duration_seconds`
still count every test.

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
xfr diff baseline.json current.json --threshold 5   # percent; 0 flags any regression
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

### Disabling the update check

The background check can be turned off:

- `--no-update-check` for a single run
- `DO_NOT_TRACK=1` (cross-tool standard) or `XFR_NO_UPDATE_CHECK=1` in the environment
- `no_update_check = true` under `[client]` in `config.toml`
- the **Update check** toggle in the TUI settings (Display tab), persisted to `prefs.toml`

The TUI toggle is locked and shows the reason when the checker is compiled out, an environment opt-out or `--no-update-check` applies, or `config.toml` explicitly sets `no_update_check` (including `false`). These overrides do not change the saved preference. Without an override, the toggle remains editable and persists normally.

Precedence, highest first: compiled-out > env > `--no-update-check` > `config.toml` > saved TUI toggle. Package builds can compile the checker out entirely with `cargo build --no-default-features` (the `update-check` feature is on by default), which drops the `update-informer` dependency.

The check uses a 1-hour cache to avoid GitHub API rate limits.

<a id="server-specific-flags"></a>

## CLI Reference

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--port` | `-p` | 5201 | Server/client port |
| `--time` | `-t` | 10s | Test duration (use 0 for infinite) |
| `--bytes` | `-n` | none | Transfer a fixed number of bytes instead of running for a fixed time (e.g. `1G`; K/M/G are binary). Total across all streams; `-t` becomes an upper bound if also given |
| `--udp` | `-u` | false | UDP mode |
| `--length` | `-l` | 1400 | UDP payload bytes, including the 16-byte xfr header (16–65507); requires `-u` and server `udp_payload_size_v1` support |
| `--quic` | `-Q` | false | QUIC mode (encrypted, multiplexed streams) |
| `--bitrate` | `-b` | unlimited | Target bitrate for TCP, UDP and QUIC (e.g., 1G, 100M). 0 = unlimited. Global across all streams |
| `--parallel` | `-P` | 1 | Parallel streams |
| `--reverse` | `-R` | false | Reverse direction (download) |
| `--bidir` | | false | Bidirectional test |
| `--ipv4` | `-4` | false | Force IPv4 only |
| `--ipv6` | `-6` | false | Force IPv6 only |
| `--bind` | `-B` (server) | none | Local address to bind: IP or IP:port on the client (e.g., 192.168.1.100), plain IP on the server |
| `--cport` | | none | Client source port for firewall traversal (UDP/QUIC/TCP data streams) |
| `--dscp` | | none | DSCP/TOS marking for TCP/UDP QoS testing (0-255 or name: EF, AF11, CS1, etc.) |
| `--mptcp` | | false | MPTCP mode (client-only, Linux 5.6+; server auto-enables) |
| `--random` | | true | Use random payload data for client-sent TCP/UDP traffic (default) |
| `--zeros` | | false | Use zero-filled payload data (client-sent traffic only) |
| `--zerocopy` | `-Z` | true (TCP) | Zero-copy TCP sends via sendfile(2), like iperf3 -Z (Linux; lowers sender CPU overhead). On by default; explicit `-Z` warns when zero-copy can't take effect |
| `--no-zerocopy` | | false | Disable zero-copy TCP sends (use regular buffered writes) |
| `--probe-mtu` | | false | Probe the path MTU per direction instead of running a throughput test (UDP + DF bit; requires server `mtu_probe_v1` support) |
| `--connect-timeout` | | none | Fail if the control connection, including hostname resolution, takes longer than this (e.g. `5s`); without it, dead servers and stuck DNS lookups are bounded only by OS defaults |
| `--json` | | false | JSON output |
| `--json-stream` | | false | JSON per interval (not with `-q`) |
| `--csv` | | false | CSV output |
| `--quiet` | `-q` | false | Summary only |
| `--interval` | `-i` | 1.0 | Report interval in seconds (must be > 0) |
| `--omit` | | 0 | Omit first N seconds |
| `--output` | `-o` | | Also save the final result to a file (CSV with `--csv`, otherwise JSON) |
| `--no-tui` | | false | Disable TUI |
| `--no-update-check` | | false | Disable the background update check (also honors `DO_NOT_TRACK` / `XFR_NO_UPDATE_CHECK`) |
| `--theme` | | default | Color theme (dracula, nord, matrix, etc.) |
| `--tcp-nodelay` | | false | Disable Nagle algorithm |
| `--window` | `-w` | OS default | Socket buffer size for TCP and UDP (SO_SNDBUF/SO_RCVBUF on both ends); when unset, TCP autotunes and UDP uses the kernel default |
| `--congestion` | | OS default | TCP congestion control algorithm (e.g. cubic, bbr, reno) |
| `--timestamp-format` | | relative | Timestamp format (relative, iso8601, unix) |
| `--log-file` | | none | Log file path (e.g., ~/.config/xfr/xfr.log) |
| `--log-level` | | info | Log level (error, warn, info, debug, trace) |
| `--push-gateway` | | none | Prometheus Push Gateway URL (server) |
| `--prometheus` | | none | Prometheus metrics port (server, requires feature) |
| `--psk` | | none | Pre-shared key for authentication |
| `--psk-file` | | none | Read PSK from file |
| `--rate-limit` | | none | Max concurrent tests per IP (server) |
| `--rate-limit-window` | | config or 60s | Rate limit time window in whole seconds, at least 1s (server) |
| `--completions` | | none | Generate shell completions (bash, zsh, fish, powershell, elvish) |
| `--allow` | | none | Allow IP/subnet, repeatable (server) |
| `--deny` | | none | Deny IP/subnet, repeatable (server) |
| `--acl-file` | | none | ACL rules file (server) |
| `--max-duration` | | none | Maximum test duration, server-side limit (server) |
| `--preset` | | none | Apply a named `[[presets]]` entry from the config file (server) |
| `--tui` | | false | Enable live dashboard (server) |
| `--one-off` | | false | Exit after one test (server, works with TCP and QUIC) |
| `--no-mdns` | | false | Disable mDNS service registration (server) |

Server options go after `serve` (`xfr serve -p 9000`). Client options typed
alongside `serve`, `diff` or `discover` (`xfr -p 9000 serve`) are a usage
error; only `--theme`, `--log-file`, `--log-level`, `--no-update-check` and
`--completions` work with any subcommand, and they go before it
(`xfr --no-update-check serve`, not `xfr serve --no-update-check`). Only
`--theme` is also accepted after the subcommand; `serve` has its own
`--log-file` and `--log-level`, given after it. Environment variables such as
`XFR_PORT` are not affected.

TCP and UDP tests use random payloads by default to avoid inflated results on WAN-optimized or compressing paths. `--random` and `--zeros` control client-sent traffic. Server-sent TCP/UDP traffic also defaults to random, but payload mode is not negotiated over the wire.

`--dscp` applies to TCP and UDP sockets on both ends when relevant, including server-sent download and bidirectional traffic. QUIC ignores it because the underlying socket is managed by Quinn, and non-Unix platforms currently warn instead of applying socket marking.

## Keybindings (Client TUI)

| Key | Action |
|-----|--------|
| `q` / `Ctrl-C` | Quit (cancels a running test; a second `Ctrl-C` force-exits) |
| `p` | Pause/Resume test traffic |
| `r` | Restart the test |
| `s` | Settings modal |
| `t` | Cycle color theme |
| `d` | Toggle per-stream view |
| `?` / `F1` | Help |
| `j` | Print JSON result |
| `u` | Dismiss update notification |

`Ctrl-C` also works while Settings is open. The footer lists `?` first, so on
narrow terminals the other hints drop off before the help hint.

The Display settings' Timestamp choice reformats history immediately.
`--timestamp-format` selects its initial format.

## Keybindings (Server TUI)

| Key | Action |
|-----|--------|
| `q` / `Ctrl-C` | Quit server |
| `?` / `F1` | Help |
| `Esc` | Close help |

The client prioritizes live throughput, status, and progress on short terminals. The server dashboard drops optional columns on narrow terminals while retaining client and throughput. In either help overlay, use PgUp/PgDn to page and Home/End to jump; the border shows `PgDn more` while there is more help below. Paging adjusts when the terminal resizes.

## Themes

xfr includes 11 built-in color themes. Select with `--theme` or press `t` during a test:

```bash
xfr <host> --theme dracula     # Dark purple theme
xfr <host> --theme matrix      # Green on black hacker style
xfr <host> --theme catppuccin  # Soothing pastels
xfr <host> --theme nord        # Arctic blue tones
```

Available themes: `default`, `kawaii`, `cyber`, `dracula`, `monochrome`, `matrix`, `nord`, `gruvbox`, `catppuccin`, `tokyo_night`, `solarized`

The `monochrome` theme (alias `mono`) is the high-visibility choice: it uses
the terminal's own default colors at maximum contrast, so it works on both
dark and light terminal backgrounds — pair it with a light background at full
brightness when working in direct sunlight. Loss severity in the throughput
graph is always also encoded without color — light loss underlines the bar
(or the baseline when throughput is zero or too small to draw),
and in a theme with no hue to spend heavy loss additionally stipples the empty
cells above it, marking the whole column while the bar keeps its true height —
so lossy intervals stay visible through glare, in monochrome, and for
colorblind users. Setting the standard
[`NO_COLOR`](https://no-color.org) environment variable selects `monochrome`
automatically unless a theme is chosen explicitly.

Your theme preference is auto-saved beside `config.toml` as `xfr/prefs.toml`
in the same platform configuration directory when you change it in the TUI.
Use `--theme default` to override a config or saved theme for a run. Automatic
choices, including `NO_COLOR`'s monochrome palette, are not saved.
The server dashboard
(`xfr serve --tui`) resolves its palette through the same precedence, so
`--theme`, a saved preference, and `NO_COLOR` all apply to it too.
For example: `xfr serve --tui --theme default`.

## Security Considerations

### Transport Encryption

| Mode | Bulk test data | Control channel |
|------|----------------|-----------------|
| TCP | Plaintext | Plaintext without PSK; ChaCha20-Poly1305 after PSK authentication |
| UDP | Plaintext | Uses the same TCP control channel behavior |
| QUIC | TLS 1.3; identity unverified without PSK | TLS 1.3; exporter-bound PSK protection when enabled |

**QUIC mode** (`-Q/--quic`) uses a self-signed server certificate without PKI
verification. Without a PSK, an active relay can terminate and forward the QUIC
connection. With a PSK, xfr mixes the RFC 9266 TLS exporter into the
authentication proof and protected-control keys, binding them to the exact QUIC
connection that carries the bulk streams. A relay cannot splice authentication
across two QUIC connections without the PSK. This requires both peers to
advertise `quic_channel_binding_v1`; the session fails closed otherwise.

### Network Considerations

- **Single-port TCP**: TCP uses single-port mode by default -- control and data connections share port 5201. Data connections are validated against the control connection's IP address, preventing unauthorized access.
- **UDP on untrusted networks**: UDP mode may be susceptible to reflection attacks from spoofed source addresses. Use TCP or QUIC on public networks.
- **Rate limiting**: Use `--rate-limit` on public servers to prevent abuse.
- **ACLs**: Use `--allow`/`--deny` to restrict client access.

### Server Resource Usage

Each TCP stream allocates a 128 KB application read/write buffer. Kernel socket buffers are managed by TCP autotuning unless the client passes `-w`/`--window`, in which case the requested size is applied via `SO_SNDBUF`/`SO_RCVBUF` on both ends. Memory usage scales with concurrent clients:

| Streams per client | App buffer per client | 10 clients (app buffers) |
|-------------------|-----------------------|--------------------------|
| 1 (`-P 1`) | 128 KB | 1.3 MB |
| 8 (`-P 8`) | 1 MB | 10 MB |
| 128 (`-P 128`) | 16 MB | 160 MB |

On top of that, the kernel holds autotuned socket buffers (typically a few hundred KB per stream, capped by `net.ipv4.tcp_rmem[2]`/`tcp_wmem[2]`). When a client passes `-w N`, add roughly `N` bytes per stream on each side. The server limits concurrent handlers (default 100) to prevent resource exhaustion. Use `--rate-limit` to restrict tests per IP.

For authentication details and handshake limits, see [PSK authentication](#psk-authentication)
and [connection security](#connection-security). Use an authenticated VPN when
you need centrally managed endpoint identities or isolation between different
holders of a shared PSK.
