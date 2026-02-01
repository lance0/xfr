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
- [x] Full IPv6 support (`-4`, `-6` flags) - dual-stack, IPv6-only modes

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
- [x] **Timestamp format options** (`--timestamp-format`) - ISO 8601, Unix epoch, relative

### Platform
- [x] **Environment variable overrides** - `XFR_PORT`, `XFR_DURATION`, etc.
- [x] **Push gateway support** (`--push-gateway`) - push Prometheus metrics to gateway
- [x] **File logging** (`--log-file`, `--log-level`) - with daily rotation via tracing-appender

---

## v0.4 - Security & Enterprise (Completed)

**Why this matters:** Enterprise environments require authentication. Security is a blocker for many deployments.

### Authentication
- [x] **Pre-shared key authentication** (`--psk`, `--psk-file`) - HMAC-SHA256 challenge-response
- [x] **TLS for control channel** (`--tls`, `--tls-cert`, `--tls-key`) - rustls-based encryption
- [x] **Rate limiting** (`--rate-limit`) - per-IP concurrent test limits

### Enterprise Features
- [x] **Server access control lists** (`--allow`, `--deny`, `--acl-file`) - IP/subnet allowlists
- [ ] **Bandwidth quotas per client** - limit resource usage

---

## v0.5 - Advanced Protocols & TUI Enhancements (In Progress)

**Why this matters:** QUIC is the future of internet transport. HTTP/3 adoption is accelerating (Cloudflare, Akamai, Fastly enable by default). Testing QUIC is increasingly important.

### QUIC Support
- [ ] **QUIC transport via `quinn` crate** - battle-tested implementation

*Note: QUIC provides encrypted data path, solving the current TLS limitation where only the control channel is encrypted.*

### TUI Enhancements
- [x] **Theme system** - 11 built-in themes, `t` to cycle
- [x] **Preferences persistence** (`~/.config/xfr/prefs.toml`)
- [x] **Server TUI dashboard** (`xfr serve --tui`) - live view of active tests

---

## Future Ideas

### High-Speed Optimization (10-25G)

*Target: Saturate 10G home lab and 25G data center links.*

- [ ] **sendmmsg for UDP bursts** - batch multiple packets per syscall (Linux)
- [ ] **SO_BUSY_POLL for UDP** - reduce jitter via busy polling (Linux)
- [ ] **CPU affinity options** (`--affinity`) - pin to specific cores
- [ ] **Socket buffer auto-tuning** - optimal SO_SNDBUF/SO_RCVBUF for link speed

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

### Advanced QUIC Features
- [ ] 0-RTT connection establishment
- [ ] Connection migration testing
- [ ] Stream multiplexing tests

*Rationale: Get basic QUIC working first. These are nice-to-haves.*

### IPv6 Flow Labels
- [ ] Traffic engineering tests with flow labels

*Rationale: Niche use case, rarely needed.*

### MPTCP
- [ ] Multi-Path TCP support

*Rationale: Very few deployments have MPTCP enabled.*

### Integrations & Bindings
- [ ] Web UI for long-running servers
- [ ] Cloud discovery (AWS/GCP/Azure)
- [ ] Grafana plugin (native data source)
- [ ] OpenTelemetry export
- [ ] libxfr C FFI
- [ ] Python bindings

*Rationale: Scope creep. Prometheus export and CLI cover most use cases.*

### Storage & Analysis
- [ ] Historical data storage (SQLite)
- [ ] Scheduled tests
- [ ] Baseline management

*Rationale: Use external tools (cron, databases). `xfr diff` covers basic comparison.*

### 100G+ Optimization
- [ ] io_uring on Linux
- [ ] Zerocopy send/receive (MSG_ZEROCOPY)
- [ ] Multi-queue NIC support
- [ ] AF_XDP kernel bypass

*Rationale: Diminishing returns. Most users have 1-25G. 100G+ needs specialized hardware and often dedicated tools like DPDK.*

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

## Known Limitations

These features are partially implemented and documented for transparency:

| Feature | Status | Notes |
|---------|--------|-------|
| TLS data transfer | Control channel only | Data path uses plain sockets; QUIC will solve this |

---

## Contributing

See issues labeled `good first issue` for entry points. PRs welcome for any roadmap item.
