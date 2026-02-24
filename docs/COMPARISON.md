# xfr vs Other Tools

A comparison of xfr with other network bandwidth testing tools.

## Feature Matrix

| Feature | xfr | iperf3 | iperf2 | rperf | nperf |
|---------|-----|--------|--------|-------|-------|
| **Multi-client server** | Yes | No | Yes | Yes | ? |
| **Live TUI** | Yes (client & server) | No | No | No | No |
| **QUIC support** | Yes (TLS 1.3) | No | No | No | Yes |
| **TCP/UDP** | Yes | Yes | Yes | Yes | Yes |
| **Single-port TCP** | Yes | No (port per stream) | No | No | ? |
| **Multi-stream** | Yes | Yes | Yes | Yes | Yes |
| **Bidirectional** | Yes | Yes | Yes (`-d`) | Yes | ? |
| **One-off mode** | Yes (`--one-off`) | Yes (`--one-off`) | No | No | ? |
| **JSON output** | Yes | Yes | Yes | Yes | ? |
| **CSV output** | Yes | No | Yes | No | ? |
| **JSON streaming** | Yes (`--json-stream`) | No | No | No | ? |
| **Prometheus metrics** | Yes (opt-in feature) | No | No | No | No |
| **Result comparison** | `xfr diff` | No | No | No | No |
| **LAN discovery** | `xfr discover` (mDNS) | No | No | No | No |
| **Config file** | Yes (TOML) | No | No | No | No |
| **TCP/UDP bitrate pacing** | Yes (`-b`) | Yes (`-b`) | Yes (`-b`) | No | ? |
| **Congestion control selection** | Yes (`--congestion`) | Yes (`--congestion`) | Yes (`-Z`) | No | ? |
| **PSK authentication** | Yes | Yes | No | No | ? |
| **Capability negotiation** | Yes (protocol v1.1) | No | No | No | ? |
| **Connection rate limiting** | Yes (per-IP) | No | No | No | ? |
| **Slow-loris protection** | Yes | No | No | No | ? |
| **Language** | Rust | C | C | Rust | Rust |
| **Active development** | Yes | Maintenance | Yes | Yes | Yes |

## For iperf3 Users

xfr is designed as a drop-in replacement for iperf3 with familiar CLI flags.

### CLI Flag Mapping

| iperf3 | xfr | Description |
|--------|-----|-------------|
| `-s` | `serve` | Start server |
| `-c HOST` | `HOST` | Connect to server (just the hostname) |
| `-p PORT` | `-p PORT` | Port number |
| `-t SECS` | `-t SECS` | Test duration |
| `-P N` | `-P N` | Parallel streams |
| `-R` | `-R` | Reverse direction (download) |
| `-u` | `-u` | UDP mode |
| `-b RATE` | `-b RATE` | Target bitrate |
| `-i SECS` | `-i SECS` | Report interval |
| `-J` | `--json` | JSON output |
| N/A | `--json-stream` | JSON streaming (one object per interval) |
| N/A | `--csv` | CSV output |
| N/A | `-Q` / `--quic` | QUIC mode (TLS 1.3) |
| `--bidir` | `--bidir` | Bidirectional test |
| `--one-off` | `--one-off` | Exit server after one test |
| `--logfile` | `--log-file` | Log to file |

### Key Differences

1. **Server command**: iperf3 uses `-s`, xfr uses the `serve` subcommand
   ```bash
   # iperf3
   iperf3 -s

   # xfr
   xfr serve
   ```

2. **Client connection**: iperf3 uses `-c`, xfr takes the host as a positional argument
   ```bash
   # iperf3
   iperf3 -c 192.168.1.1

   # xfr
   xfr 192.168.1.1
   ```

3. **TUI is default**: xfr shows a live TUI by default. Use `--no-tui` for plain text output.

4. **JSON streaming**: xfr supports `--json-stream` for one JSON object per interval (useful for real-time parsing).

5. **Single-port TCP**: xfr multiplexes all data connections over the control port (5201) using `DataHello` identification. iperf3 opens separate ephemeral ports for each stream, which can be problematic with firewalls.

6. **Protocol negotiation**: xfr clients and servers exchange capabilities during the Hello handshake (protocol v1.1). The server falls back to multi-port TCP for legacy clients that do not advertise `single_port_tcp`.

7. **QUIC transport**: xfr supports QUIC (`--quic` or `-Q`) with built-in TLS 1.3 encryption and multiplexed streams. iperf3 has no QUIC support.

### Migration Examples

```bash
# iperf3: Basic server
iperf3 -s -p 9000
# xfr equivalent
xfr serve -p 9000

# iperf3: 4 parallel TCP streams, 30 second test
iperf3 -c host -P 4 -t 30
# xfr equivalent
xfr <host> -P 4 -t 30s

# iperf3: TCP at 100 Mbps
iperf3 -c host -b 100M
# xfr equivalent
xfr <host> -b 100M

# iperf3: UDP at 1 Gbps
iperf3 -c host -u -b 1G
# xfr equivalent
xfr <host> -u -b 1G

# iperf3: Reverse mode (download test)
iperf3 -c host -R
# xfr equivalent
xfr <host> -R

# iperf3: JSON output
iperf3 -c host -J
# xfr equivalent (for scripting, disable TUI)
xfr <host> --json --no-tui

# xfr: QUIC mode (no iperf3 equivalent)
xfr <host> -Q

# xfr: One-off server (exits after a single test)
xfr serve --one-off
```

## When to Use Each Tool

### Use xfr when you need:

- **Multi-client server** - iperf3's single-client limitation is its biggest complaint
- **Firewall-friendly testing** - Single-port TCP mode keeps everything on one port (no ephemeral data ports to open)
- **Visual monitoring** - Live TUI with throughput graphs
- **CI/CD integration** - JSON streaming, result comparison, Prometheus metrics
- **QUIC testing** - Test encrypted transport with built-in TLS 1.3
- **Server hardening** - Connection rate limiting, slow-loris protection, DataHello flood protection, PSK authentication
- **Modern tooling** - Config files, environment variables, auto-discovery, capability negotiation

### Use iperf3 when:

- You need exact iperf3 protocol compatibility
- Working with systems that only have iperf3 installed
- You need features xfr doesn't have yet (e.g., `--get-server-output`)

### Use iperf2 when:

- You need multicast or broadcast testing
- Working with legacy systems requiring iperf2 protocol

### Use nperf when:

- Working in environments where nperf is already deployed
- You need nperf-specific features

## Protocol Differences

xfr uses its own control protocol (v1.1) over newline-delimited JSON messages. Key differences from iperf3:

| Aspect | xfr | iperf3 |
|--------|-----|--------|
| **Control protocol** | JSON over TCP/QUIC | Custom binary/text |
| **Protocol version** | 1.1 (numeric major comparison) | N/A |
| **TCP data ports** | Single port (DataHello routing) | Separate port per stream |
| **Capability exchange** | Client/server Hello with capabilities list | None |
| **Authentication** | PSK with HMAC challenge-response | PSK (RSA-based) |
| **MPTCP support** | Yes (auto on server, `--mptcp` on client, Linux 5.6+) | No |
| **Transport encryption** | QUIC mode: TLS 1.3 built-in | None |

### Single-Port TCP Architecture

iperf3 opens a separate ephemeral TCP port for each data stream. This requires firewall rules for each port and causes issues in locked-down environments.

xfr v1.1 multiplexes all connections over the control port (default 5201):

1. Client opens control connection to port 5201 and exchanges Hello with capabilities
2. Client opens additional TCP connections to the same port 5201
3. Each data connection sends a `DataHello` message identifying its `test_id` and `stream_index`
4. Server routes connections to the correct test session

The server falls back to multi-port mode automatically when connecting to legacy clients that lack the `single_port_tcp` capability.

## Security Comparison

| Protection | xfr | iperf3 |
|------------|-----|--------|
| **PSK authentication** | Yes (HMAC challenge-response) | Yes (RSA-based) |
| **Transport encryption** | QUIC mode (TLS 1.3) | No |
| **Slow-loris protection** | Yes (5s initial read timeout, per-connection tasks) | No |
| **Connection flood protection** | Yes (DataHello validation, unknown test_ids dropped) | No |
| **Connection rate limiting** | Yes (per-IP limits) | No |
| **Concurrent connection limit** | Yes (configurable semaphore) | Single client only |
| **DataHello IP validation** | Yes (same-IP enforcement) | N/A |

## Performance Notes

xfr is built on Tokio's async runtime and uses the same high-performance buffer strategies as iperf3. In benchmarks, xfr achieves comparable throughput:

- **10G TCP**: Both saturate the link
- **25G TCP**: Both saturate with sufficient streams (`-P 4`)
- **UDP**: Both handle 10+ Gbps with tuned buffers
- **QUIC**: Slightly lower throughput than raw TCP due to TLS 1.3 overhead, but provides encryption

The main performance consideration is that xfr's TUI adds minimal overhead (~0.1% CPU). Disable with `--no-tui` for absolute maximum performance in benchmarks.
