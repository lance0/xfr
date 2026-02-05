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
- [x] **In-app update notifications** - yellow banner when newer version available, `u` to dismiss
- [ ] **Bandwidth quotas per client** - limit resource usage

*Note: TLS was removed in favor of QUIC, which provides built-in encryption.*

---

## v0.5 - Advanced Protocols & TUI Enhancements (Completed)

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
- [x] **Settings modal** (`s` key) - adjust display and test settings on the fly
- [x] **Per-stream expandable view** (`d` key) - toggle between History and Streams panels
- [x] **Pause overlay** - visual feedback when test is paused
- [x] **History event logging** - automatic logging of peaks, retransmit spikes, UDP loss

---

## v0.5 - Code Quality & Robustness

### Pre-1.0 Requirements
- [ ] **Structured error types** - replace `anyhow::Error` with `thiserror` enum for library users
- [x] **Lower default max_concurrent** - reduce from 1000 to 100 for safer defaults
- [ ] **Config file versioning** - add version field and migration support for breaking changes
- [x] **Protocol version bump** - bump to 1.1 at next release to signal DataHello support

### Security Enhancements
- [ ] **QUIC certificate verification** (`--quic-verify`) - optional server cert verification for enterprise use
- [ ] **Data-plane authentication** - per-test tokens/cookies to prevent port hijacking on untrusted networks
- [ ] **Rate limiting on data connections** - apply per-IP limits to data connections (currently control-only)
- [x] **Slow-loris protection** - accept loop spawns per-connection tasks with 5s initial read timeout
- [x] **DataHello flood protection** - validate test_id exists before processing data connections
- [x] **Client capabilities negotiation** - client advertises capabilities; server falls back to multi-port TCP for legacy clients
- [x] **Pre-handshake connection gate** - limits concurrent unclassified connections to prevent connection-flood DoS
- [x] **Multi-port TCP IP validation** - per-stream fallback listeners validate peer IP against control connection

### Code Quality
- [ ] **Refactor run_test()** - split long function in serve.rs into protocol-specific helpers
- [ ] **Refactor main.rs** - split CLI, config, and TUI setup into separate modules
- [ ] **Clean up dead code** - remove unused ProgressBar.style(), complete InstallMethod::update_command
- [x] **Add SAFETY comments** - document invariants for 4 unsafe blocks in tcp_info.rs, tcp.rs, net.rs
- [ ] **Audit unwrap()/expect() calls** - reduce 61 calls in production code, especially auth.rs HMAC init
- [ ] **Remove unused dependencies** - once_cell→OnceLock, evaluate futures, humantime, async-trait
- [ ] **Join client data tasks** - ensure Client::run waits for all data tasks before returning (library safety)
- [ ] **Extract shared handshake logic** - reduce duplication between TCP and QUIC control-plane paths

### Testing
- [ ] **Concurrent client tests** - simulate multiple clients to verify race condition handling
- [ ] **Fuzz testing** - fuzz control protocol JSON parsing for robustness
- [ ] **Property-based testing** - packet sequence tracking, rate limiter invariants
- [ ] **Rate limiting and ACL tests** - allow/deny precedence, IPv4-mapped IPv6
- [ ] **Cancellation flow tests** - client cancel, server cancel, partial stream setup
- [ ] **QUIC negative tests** - client opens fewer streams than requested, malformed messages
- [ ] **Listener backlog stress test** - verify single-port mode handles many concurrent data connections

### Documentation
- [ ] **API documentation** - add examples to public functions in net module
- [ ] **Algorithm documentation** - inline comments for jitter calculation and other complex logic

---

## Future Ideas

### CLI & Scripting
- [x] **Infinite duration** (`-t 0`) - run test indefinitely until manually stopped
- [ ] **Get server output** (`--get-server-output`) - return server's JSON result to client (iperf3 parity)
- [ ] **Congestion control** (`--congestion`) - select TCP CC algorithm (cubic, bbr, reno)
- [x] **Bind to interface** (`--bind`) - bind to specific IP/interface for multi-homed hosts

### TUI Enhancements
- [ ] **Test profiles** - save/load named test configurations
- [ ] **Side-by-side comparison mode** - compare baseline vs current in TUI

### High-Speed Optimization (10-25G)

*Target: Saturate 10G home lab and 25G data center links.*

*Current limitation: UDP pacing tops out around 2 Gbps due to per-packet syscall overhead. TCP achieves 35+ Gbps on localhost.*

- [ ] **sendmmsg for UDP bursts** - batch multiple packets per syscall (Linux)
- [ ] **SO_BUSY_POLL for UDP** - reduce jitter via busy polling (Linux)
- [ ] **CPU affinity options** (`--affinity`) - pin streams to specific cores, reduces page faults and context switches (rperf has this)
- [ ] **Socket buffer auto-tuning** - optimal SO_SNDBUF/SO_RCVBUF for link speed
- [ ] **Batch atomic counter updates** - reduce per-packet atomic operations at high PPS (flush once per interval)

---

## Low Priority

### Windows Native
- [ ] Basic TCP/UDP testing (no TCP_INFO)
- [ ] TUI compatibility with Windows Terminal
- [ ] Pre-built binaries
- [ ] Config path adjustment (`%APPDATA%\xfr`)

*Rationale: Codebase already has fallbacks for Unix-specific features. Cross-compilation should work with minimal changes.*

### TUI Code Refactoring
- [ ] Split `ui.rs` into `render.rs` and `modals.rs`
- [ ] Extract state/event logic from `app.rs` into `state.rs`
- [ ] Keep `widgets.rs` as-is

*Rationale: TUI code is growing. Not urgent, but would improve maintainability for future features.*

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

See [KNOWN_ISSUES.md](KNOWN_ISSUES.md) for documented edge cases and limitations.

**Potential future improvements** (from known issues):
- [ ] QUIC bitrate pacing support
- [ ] Configurable UDP MTU/packet size
- [ ] UDP reverse mode error reporting to client

---

## Contributing

See issues labeled `good first issue` for entry points. PRs welcome for any roadmap item.
