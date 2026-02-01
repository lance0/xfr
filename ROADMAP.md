# xfr Roadmap

## Market Context

**iperf3 is in "maintenance mode"** (since v3.1) with limited resources for new development. This creates an opportunity for xfr to become the modern alternative.

**Key advantages xfr already has over iperf3:**
- Multi-client server support (iperf3's #1 complaint)
- Async architecture (iperf3 only added threading in v3.16)
- Live TUI with real-time graphs
- Prometheus metrics built-in
- LAN discovery via mDNS
- Result comparison (`xfr diff`)

---

## Completed (v0.1.x)

- [x] Server/client modes with JSON control protocol
- [x] TCP bulk transfer with high-speed buffer configuration
- [x] UDP pacing with RFC 3550 jitter calculation
- [x] Multi-stream support (`-P` flag)
- [x] Reverse mode (`-R`) and bidirectional (`--bidir`)
- [x] TCP_INFO stats (Linux and macOS)
- [x] Live TUI with sparklines and per-stream bars
- [x] Plain text, JSON output formats
- [x] LAN discovery via mDNS (`xfr discover`)
- [x] Diff command for comparing test results
- [x] Published to crates.io

## Completed (v0.2.x)

- [x] Prometheus metrics with Grafana dashboard
- [x] Config file support (`~/.config/xfr/config.toml`)
- [x] Server presets (bandwidth limits, allowed clients)
- [x] Man page and contributing guide
- [x] Dynamic port allocation (multi-client safe)
- [x] Proper bidir mode with split sockets
- [x] Integration tests for all modes
- [x] mDNS service registration on server start

---

## v0.3 - Output Formats & Usability

**Why this matters:** Users migrating from iperf3 expect familiar output options. JSON streaming enables real-time monitoring integrations.

### Output Enhancements
- [x] **CSV output format** (`--csv`) - frequently requested iperf2 feature never added to iperf3
- [x] **JSON streaming output** (`--json-stream`) - iperf3 3.18 added this, one JSON object per line
- [x] **Quiet mode** (`-q`) - suppress interval output, show only summary

### Usability
- [x] **Server max duration** (`--max-duration`) - limit test length server-side
- [x] **Omit interval** (`--omit N`) - discard first N seconds (TCP ramp-up)
- [x] **Report interval** (`-i`) - configurable reporting interval (default 1s)
- [x] **Shell completions** (`--completions SHELL`) - bash, zsh, fish, powershell, elvish
- [ ] **Timestamp format options** - ISO 8601, Unix epoch, relative

### Platform
- [x] **Environment variable overrides** - `XFR_PORT`, `XFR_DURATION`, etc.
- [ ] **Push gateway support** - push Prometheus metrics to gateway

---

## v0.4 - Security & Enterprise

**Why this matters:** Enterprise environments require authentication and audit trails. Security is a blocker for many deployments.

### Authentication
- [ ] **Pre-shared key authentication** - simple shared secret
- [ ] **TLS for control channel** - encrypt test negotiation
- [ ] **Rate limiting** - prevent abuse of public servers

### Enterprise Features
- [ ] **Server access control lists** - IP/subnet allowlists
- [ ] **Bandwidth quotas per client** - limit resource usage
- [ ] **Audit logging** - structured logs for compliance

---

## v0.5 - Advanced Protocols

**Why this matters:** QUIC is the future of internet transport. HTTP/3 adoption is accelerating (Cloudflare, Akamai, Fastly enable by default). Testing QUIC is increasingly important.

### QUIC Support
- [ ] **QUIC transport via `quinn` crate** - battle-tested implementation
- [ ] **0-RTT connection establishment** - test resumed connections
- [ ] **Connection migration testing** - simulate IP changes (mobile networks)
- [ ] **Stream multiplexing tests** - measure QUIC's head-of-line blocking advantage

### IPv6
- [ ] **Full IPv6 support** - dual-stack, IPv6-only modes
- [ ] **IPv6 flow labels** - for traffic engineering tests

### MPTCP (Multi-Path TCP)
- [ ] **MPTCP support** - iperf3 3.16 added this, test multi-path aggregation

---

## Future Ideas

### High-Speed Optimization (10G+)
- [ ] **io_uring on Linux** - reduce syscall overhead
- [ ] **CPU affinity options** - pin to specific cores/NUMA nodes
- [ ] **Zerocopy send/receive** - MSG_ZEROCOPY, MSG_TRUNC
- [ ] **Multi-queue NIC support** - leverage hardware parallelism

### Storage & Analysis
- [ ] **Historical data storage** - SQLite for trend analysis
- [ ] **Scheduled tests** - cron-like recurring bandwidth checks
- [ ] **Baseline management** - store and compare against baselines

### Integrations
- [ ] **Web UI** - browser-based dashboard for long-running servers
- [ ] **Cloud discovery** - AWS/GCP/Azure endpoint discovery
- [ ] **Grafana plugin** - native Grafana data source
- [ ] **OpenTelemetry export** - traces and metrics

### Library Mode
- [ ] **libxfr** - C-compatible FFI for embedding in other tools
- [ ] **Python bindings** - for scripting and automation

---

## Low Priority

### Windows Native
- [ ] Basic TCP/UDP testing (no TCP_INFO)
- [ ] TUI compatibility with Windows Terminal
- [ ] Pre-built binaries

*Rationale: WSL works well. Native Windows adds complexity for limited benefit.*

### SCTP Support
- [ ] SCTP transport mode

*Rationale: Limited real-world usage compared to TCP/UDP/QUIC.*

---

## Testing Improvements

- [x] Cross-platform CI (Linux, macOS)
- [x] Integration tests for TCP, UDP, bidir, multi-client
- [ ] Performance regression CI (ensure 10G+ capability)
- [ ] Long-duration stability tests (1+ hour)
- [ ] High packet loss scenarios
- [ ] Real hardware testing (10G NICs)

---

## Competitive Landscape

| Tool | Language | Multi-client | TUI | QUIC | Active Development |
|------|----------|--------------|-----|------|-------------------|
| iperf3 | C | No | No | No | Maintenance only |
| iperf2 | C | Yes | No | No | Active |
| rperf | Rust | Yes | No | No | Active |
| nperf | Rust | ? | No | Yes | Active |
| **xfr** | Rust | Yes | Yes | Planned | Active |

---

## Contributing

See issues labeled `good first issue` for entry points. PRs welcome for any roadmap item.
