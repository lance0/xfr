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
- [x] **Rate limiting** (`--rate-limit`) - per-IP concurrent test limits

### Enterprise Features
- [x] **Server access control lists** (`--allow`, `--deny`, `--acl-file`) - IP/subnet allowlists
- [ ] **Bandwidth quotas per client** - limit resource usage

*Note: TLS was removed in favor of QUIC, which provides built-in encryption.*

---

## v0.5 - Advanced Protocols & TUI Enhancements (In Progress)

**Why this matters:** QUIC is the future of internet transport. HTTP/3 adoption is accelerating (Cloudflare, Akamai, Fastly enable by default). Testing QUIC is increasingly important.

### QUIC Support
- [x] **QUIC transport** (`quinn` crate) - full client/server implementation
- [x] **QUIC data transfer** - stream-based send/receive with multiplexing
- [x] **QUIC server endpoint** - accepts QUIC alongside TCP on same port
- [x] **QUIC integration tests** - upload, download, multi-stream, PSK auth

*Note: QUIC provides built-in TLS 1.3 encryption. The legacy TLS-over-TCP code was removed in favor of QUIC.*

### TUI Enhancements
- [x] **Theme system** - 11 built-in themes, `t` to cycle
- [x] **Preferences persistence** (`~/.config/xfr/prefs.toml`)
- [x] **Server TUI dashboard** (`xfr serve --tui`) - live view of active tests
- [x] **Completion summary panel** - clear final stats display with throughput graph
- [x] **Color-coded metrics** - green/yellow/red for retransmits, loss, jitter, RTT
- [x] **Enhanced sparkline** - 2-row graph with peak value display
- [ ] **Settings modal** (`s` key) - adjust display and test settings on the fly
- [ ] **Test profiles** - save/load named test configurations
- [ ] **Per-stream expandable view** - collapse/expand for multi-stream tests
- [ ] **Side-by-side comparison mode** - compare baseline vs current in TUI

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

## Documentation (Completed)

- [x] `docs/COMPARISON.md` - Feature matrix vs iperf3/iperf2/rperf/nperf
- [x] `docs/SCRIPTING.md` - CI/CD, Docker, Prometheus integration
- [x] `docs/FEATURES.md` - Comprehensive feature reference
- [x] `docs/ARCHITECTURE.md` - Module structure, data flow, threading model
- [x] `install.sh` - Cross-platform installer
- [x] Enhanced README with real-world use cases
- [x] Module-level rustdoc (lib.rs, protocol.rs, quic.rs)

---

## Competitive Landscape

| Tool | Language | Multi-client | TUI | QUIC | Active Development |
|------|----------|--------------|-----|------|-------------------|
| iperf3 | C | No | No | No | Maintenance only |
| iperf2 | C | Yes | No | No | Active |
| rperf | Rust | Yes | No | No | Active |
| nperf | Rust | ? | No | Yes | Active |
| **xfr** | Rust | Yes | Yes | Yes | Active |

---

## Scope Creep / Non-Goals

xfr is a **CLI bandwidth testing tool**. The following are explicitly out of scope:

- **Web UI** - xfr is CLI-only. Use Prometheus + Grafana for dashboards.
- **Traceroute / path analysis** - Use [ttl](https://github.com/lance0/ttl) instead.
- **Packet capture** - Use tcpdump, Wireshark, or similar tools.
- **Network scanning** - Use nmap or masscan.
- **Historical database** - Use external storage. `xfr diff` covers basic comparison.
- **Scheduled tests** - Use cron or CI/CD schedulers.
- **Configuration management** - Manage config files with your preferred tools.

If you need these features, combine xfr with purpose-built tools.

---

## Known Limitations

All major features are fully implemented. See "Low Priority" section for features not planned.

---

## Contributing

See issues labeled `good first issue` for entry points. PRs welcome for any roadmap item.
