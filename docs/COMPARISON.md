# xfr vs Other Tools

A comparison of xfr with other network bandwidth testing tools.

## Feature Matrix

| Feature | xfr | iperf3 | iperf2 | rperf | nperf |
|---------|-----|--------|--------|-------|-------|
| **Multi-client server** | Yes | No | Yes | Yes | ? |
| **Live TUI** | Yes (client & server) | No | No | No | No |
| **QUIC support** | Yes | No | No | No | Yes |
| **TCP/UDP** | Yes | Yes | Yes | Yes | Yes |
| **Multi-stream** | Yes | Yes | Yes | Yes | Yes |
| **Bidirectional** | Yes | Yes | No | Yes | ? |
| **JSON output** | Yes | Yes | Yes | Yes | ? |
| **CSV output** | Yes | No | Yes | No | ? |
| **Prometheus metrics** | Yes | No | No | No | No |
| **Result comparison** | `xfr diff` | No | No | No | No |
| **LAN discovery** | `xfr discover` | No | No | No | No |
| **Config file** | Yes | No | No | No | No |
| **PSK authentication** | Yes | Yes | No | No | ? |
| **Rate limiting** | Yes | No | No | No | ? |
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

### Migration Examples

```bash
# iperf3: Basic server
iperf3 -s -p 9000
# xfr equivalent
xfr serve -p 9000

# iperf3: 4 parallel TCP streams, 30 second test
iperf3 -c host -P 4 -t 30
# xfr equivalent
xfr host -P 4 -t 30s

# iperf3: UDP at 1 Gbps
iperf3 -c host -u -b 1G
# xfr equivalent
xfr host -u -b 1G

# iperf3: Reverse mode (download test)
iperf3 -c host -R
# xfr equivalent
xfr host -R

# iperf3: JSON output
iperf3 -c host -J
# xfr equivalent (for scripting, disable TUI)
xfr host --json --no-tui
```

## When to Use Each Tool

### Use xfr when you need:

- **Multi-client server** - iperf3's single-client limitation is its biggest complaint
- **Visual monitoring** - Live TUI with throughput graphs
- **CI/CD integration** - JSON streaming, result comparison, Prometheus metrics
- **QUIC testing** - Test encrypted HTTP/3-style transport
- **Modern tooling** - Config files, environment variables, auto-discovery

### Use iperf3 when:

- You need exact iperf3 protocol compatibility
- Working with systems that only have iperf3 installed
- You need features xfr doesn't have yet (e.g., --get-server-output)

### Use iperf2 when:

- You need multicast or broadcast testing
- Working with legacy systems requiring iperf2 protocol

### Use nperf when:

- You specifically need its QUIC implementation
- Working in environments where nperf is already deployed

## Performance Notes

xfr is built on Tokio's async runtime and uses the same high-performance buffer strategies as iperf3. In benchmarks, xfr achieves comparable throughput:

- **10G TCP**: Both saturate the link
- **25G TCP**: Both saturate with sufficient streams (`-P 4`)
- **UDP**: Both handle 10+ Gbps with tuned buffers

The main performance consideration is that xfr's TUI adds minimal overhead (~0.1% CPU). Disable with `--no-tui` for absolute maximum performance in benchmarks.
