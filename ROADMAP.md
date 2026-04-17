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
- [x] **Separate send/recv in bidir summary** (issue #56) — `--bidir` now reports per-direction bytes and throughput in plain text, JSON, CSV, and TUI outputs. Combined total kept for backward compat

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

## Pre-1.0 - Code Quality & Robustness

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

### Polish
- [x] **Suppress TCP send errors on graceful shutdown** - TCP `Connection reset by peer` / `Broken pipe` / `Connection aborted` at teardown now suppressed via targeted grace window (issue #25)
- [ ] **Suppress UDP/QUIC send errors on graceful shutdown** - UDP shows `Connection refused (os error 111)`, QUIC shows `sending stopped by peer: error 0` when server tears down sockets before client finishes sending; cosmetic but noisy
- [x] **Summary on early exit** (issue #35) — Ctrl+C now triggers the cancel path and displays the test summary with accumulated stats. Plain mode uses `tokio::signal::ctrl_c`; TUI treats Ctrl+C as a key event in raw mode. Double Ctrl+C force-exits (exit code 130). Fallback summary from cumulative counters when server is unreachable
- [x] **Delta retransmits in plain-text interval output** (issue #36) - plain-text interval reports now show retransmit deltas instead of cumulative totals while the final summary remains cumulative. TUI stats remain cumulative by design, and server-reported per-stream interval deltas continue in JSON/CSV output
- [x] **`omit_secs` in config file** (issue #43) — `[client] omit_secs` in config.toml sets default `--omit` value
- [ ] **Group CLI help by client/server** (issue #43) — restructure `--help` output into client-only, server-only, and shared sections like iperf3

### Code Quality
- [ ] **Test lifecycle guard** - wrap active_tests entries in a `Drop` guard so cleanup runs regardless of handler panic/error; covers orphaned state on control disconnect, semaphore permit leak, and QUIC handler cleanup. Consider DashMap to reduce lock contention at higher concurrency
- [x] **Fix PSK unwrap panics** - serve.rs PSK `.unwrap()` replaced with `.ok_or_else()` error propagation
- [x] **UDP encode bounds check** - `UdpPacketHeader::encode()` now validates buffer length before indexing
- [x] **Timestamp clock skew** - ISO8601/Unix timestamps now derive wall clock from `system_start + elapsed` instead of `SystemTime::now()`
- [x] **Extract magic number constants** - 6 named constants replace 12 hardcoded values (STATS_INTERVAL, CANCEL_CHECK_TIMEOUT, etc.)
- [ ] **Refactor run_test()** - serve.rs (352 lines), run_quic_test (269 lines), client equivalents similarly oversized; split into protocol-specific helpers
- [ ] **Refactor main.rs** - 340-line main() mixing CLI parsing, config building, PSK handling, dispatch; split into modules
- [ ] **Deduplicate TUI event loop** - main.rs has ~90 lines copy-pasted between active test loop and result wait loop
- [ ] **Clean up dead code** - remove unused ProgressBar.style(), complete InstallMethod::update_command
- [x] **Add SAFETY comments** - document invariants for 4 unsafe blocks in tcp_info.rs, tcp.rs, net.rs
- [ ] **Audit unwrap()/expect() calls** - reduce calls in production code, especially auth.rs HMAC init and serve.rs PSK handling
- [ ] **Remove unused dependencies** - once_cell→OnceLock, evaluate futures, humantime, async-trait
- [x] **Join client data tasks** - Client::run now collects JoinHandles and joins with 2s timeout + abort before returning
- [ ] **Extract shared handshake logic** - ~100+ lines duplicated between TCP and QUIC paths in both serve.rs and client.rs

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

*Prioritized by effort vs user impact. Quick wins first, then bigger lifts.*

### Quick Wins (low effort, high impact)
- [x] **Congestion control** (`--congestion`) - select TCP CC algorithm (cubic, bbr, reno); just a `setsockopt` call. High demand for BBR vs CUBIC comparison on WAN/cloud links
- [x] **Live TCP_INFO polling** - periodically sample RTT, retransmits, cwnd during test (issue #13); extends existing TCP_INFO code. Essential for `-t 0` where results are never finalized
- [x] **TCP bitrate pacing** (`-b` for TCP) - byte-budget sleep pacing with interruptible sleeps and buffer auto-capping; `-b` flag now applies to TCP and UDP (issue #14). On Linux, uses kernel `SO_MAX_PACING_RATE` for precise per-packet pacing via the FQ scheduler (issue #30)
- [x] **Client source port pinning** (`--cport`) - pin local port for firewall traversal (issue #16); UDP and TCP data streams use sequential ports for multi-stream (`-P 4` → ports 5300-5303), QUIC multiplexes on single port. See decision tree below
- [x] **Random payload data** (`--random`) - fill send buffers with random bytes to defeat WAN optimizer/compression/dedup bias (issue #34). Both client and server TCP/UDP; QUIC skipped (already encrypted). Fill-once per buffer, no per-write overhead. `--zeros` only affects client-sent traffic; server payload mode not yet negotiated over wire
- [ ] **Configurable UDP packet size** (`--packet-size`) - set UDP datagram size for jumbo frame validation and MTU path testing; iperf3 `--set-mss` is TCP-only (issue esnet/iperf#861)
- [ ] **Get server output** (`--get-server-output`) - return server's JSON result to client (iperf3 parity)
- [x] **DSCP/TOS marking** (`--dscp`) - set DSCP/TOS on client TCP/UDP sockets for QoS policy testing; QUIC ignores it. Accepts numeric (0-255) or DSCP names (EF, AF11-AF43, CS0-CS7)
- [ ] **TCP Fast Open** (`--fast-open`) - reduce handshake latency for short tests; `setsockopt(TCP_FASTOPEN)` on server, `MSG_FASTOPEN` on client connect
- [ ] **CC algorithm A/B comparison** (`xfr cca-compare <host>`) - run back-to-back tests with different congestion control algorithms (BBR, CUBIC, Reno, etc.) and produce side-by-side comparison (throughput, retransmits, RTT). Leverages existing `--congestion` and `xfr diff`. BBR vs CUBIC is one of the most common network testing questions
- [ ] **Server-side bandwidth caps** (`--max-bandwidth`) - per-test server-enforced bandwidth limit to prevent abuse of public servers. iperf3 issue #937 is highly requested. Distinct from `--rate-limit` (which limits concurrent tests per IP)

### Firewall Traversal Decision Tree

Issue #16 revealed that strict firewalls need predictable ports on both sides. Here's how xfr handles each protocol through firewalls:

```
Client behind strict firewall → which protocol?
│
├─ TCP
│  └─ Already works ✓ (single-port mode since v0.5.0)
│     All connections use destination port 5201.
│     Stateful firewalls track ephemeral source ports automatically.
│     --cport can pin TCP data-stream source ports when egress
│     rules or ECMP testing need predictable client ports.
│     Control connection remains ephemeral.
│
├─ QUIC
│  └─ Already works ✓ through stateful firewalls (one UDP socket, multiplexed)
│     --cport pins the local port for strict egress rules.
│
└─ UDP
   ├─ Stateful firewall?
   │  └─ Works if outbound traffic auto-allows return.
   │     --cport pins the source port(s) for strict egress rules.
   │     Multi-stream (-P N) uses sequential ports: cport, cport+1, ..., cport+N-1
   │
   └─ Strict ingress+egress (both sides pinned)?
      └─ --cport handles the client source side.
         Server control port is configurable via -p.
         Server data ports are still ephemeral (random).
         → Use QUIC as workaround (single port, multiplexed).
         → Future: UDP single-port mode would multiplex all
           streams through one port (like TCP's DataHello).
```

**TCP `--cport` scope:** only TCP data-stream source ports are pinned. The control connection still uses an ephemeral source port, and xfr fails fast if that ephemeral control port overlaps the requested data-port range.

**Future work:** UDP single-port mode (multiplex all streams through one socket) would be the full solution for environments where even sequential ports aren't acceptable. This is a larger protocol change tracked separately.

### Medium Effort (moderate effort, high impact)
- [x] **Pause/resume** (`p` key) - real traffic pause via `Pause`/`Resume` protocol messages and a second `watch` channel to data loops (v0.7.0, issue #19)
- [ ] **Repeat mode** (`--repeat N --interval 60s`) - run N tests with delays and output summary; replaces cron-based scripting for CI/monitoring. Could extend to `xfr monitor` with local time-series storage (SQLite/JSON) and percentile tracking (p50/p95/p99)
- [ ] **Responsiveness / bufferbloat scoring** (`xfr responsiveness`) - saturate the link (upload + download) while measuring latency every 200ms, report RPM (Roundtrips Per Minute) score and bufferbloat letter grade (A-F). Follows IETF `draft-ietf-ippm-responsiveness` methodology. No single-binary CLI tool does both throughput AND responsiveness scoring — Crusader, Flent, and Apple's `networkQuality` each require separate tools or are platform-specific. Highest differentiation opportunity
- [ ] **Server UDP port range** (`--data-port-range`) - configurable ephemeral port range for server-side UDP data sockets (requested in issue #38 for strict firewall environments on Windows)
- [ ] **UDP single-port mode** - multiplex all UDP streams through a single port, eliminating per-stream port allocation. Analogous to TCP's DataHello approach. Would make UDP fully firewall-friendly without `--cport`
- [ ] **UDP GSO/GRO** - kernel-level packet batching for UDP; iperf3 added this Aug 2025, would break through the 2 Gbps UDP ceiling

### Larger Projects (high effort, high impact)
- [ ] **Real-time traffic emulation** (`--profile voip|gaming|video`) - preset packet sizes and intervals to emulate real-time traffic: VoIP (64B/20ms bidirectional), gaming (100-300B/8-16ms), video call (1200B/33ms). Reports jitter, out-of-order, and MOS (Mean Opinion Score) estimate. Replaces IRTT for most use cases in a single binary
- [ ] **Mesh / multi-node matrix testing** (`xfr mesh`) - given a list of xfr server addresses, run all-pairs throughput/latency tests and output a matrix (table, CSV, JSON). Useful for Kubernetes cluster validation, multi-region deployments, and SD-WAN path selection. No open-source CLI tool does this
- [ ] **sendmmsg for UDP bursts** - batch multiple packets per syscall (Linux)
- [ ] **CPU affinity options** (`--affinity`) - pin streams to specific cores, reduces page faults and context switches (rperf has this)
- [ ] **Socket buffer auto-tuning** - optimal SO_SNDBUF/SO_RCVBUF for link speed

### Nice to Have
- [x] **Infinite duration** (`-t 0`) - run test indefinitely until manually stopped
- [x] **Bind to interface** (`--bind`) - bind to specific IP/interface for multi-homed hosts; supported in both client and server mode (issue #38)
- [ ] **SO_BUSY_POLL for UDP** - reduce jitter via busy polling (Linux)
- [ ] **Batch atomic counter updates** - reduce per-packet atomic operations at high PPS (flush once per interval)
- [ ] **Test profiles** - save/load named test configurations
- [ ] **Side-by-side comparison mode** - compare baseline vs current in TUI
- [ ] **Server health check** (`--health-check`) - simple HTTP endpoint for load balancer integration; tiny listener on separate port or same port responding to `GET /health`
- [ ] **UDP out-of-order & duplicate counting** - report reordered and duplicate packets separately from lost packets. rperf has this; important for lossy WAN analysis
- [ ] **QUIC-specific metrics** - report handshake time, 0-RTT resumption bytes, stream multiplexing overhead. Extends existing QUIC support with diagnostic detail

---

## Low Priority

### Distribution & Visibility
- [ ] **Homebrew core submission** - at 439+ stars, well past the 75-star threshold. Would remove the need for `lance0/tap`
- [ ] **ESnet Fasterdata listing** - contact ESnet to add xfr to their [throughput tool comparison page](https://fasterdata.es.net/performance-testing/network-troubleshooting-tools/throughput-tool-comparision/). The reference page for network tools in the research/enterprise community
- [ ] **winget / MSI packaging** - first-class Windows distribution. Microsoft [tells users not to use iperf3 on Windows](https://techcommunity.microsoft.com/blog/networkingblog/three-reasons-why-you-should-not-use-iperf3-on-windows/4117876) — opportunity to capture that audience

### Windows Native
- [x] **QUIC dual-stack fix** (issue #39) — QUIC server endpoint now handles `IPV6_V6ONLY` explicitly via socket2, fixing IPv4 QUIC on Windows/macOS where the default is `true`
- [ ] Basic TCP/UDP testing (no TCP_INFO)
- [ ] TUI compatibility with Windows Terminal
- [ ] Pre-built binaries
- [x] Config path adjustment (`%APPDATA%\xfr`) — documented in README, FEATURES.md, and manpage

*Rationale: Codebase already has fallbacks for Unix-specific features. Microsoft recommends against iperf3 on Windows, creating an opening for a cross-platform Rust tool.*

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
- [x] Multi-Path TCP support (`--mptcp` client flag, Linux 5.6+)
- [x] Server auto-MPTCP (try MPTCP listeners by default, silent fallback to TCP)
- [ ] `MPTCP_FULL_INFO` per-subflow stats (RTT, cwnd per path via `MPTCP_INFO` / `TCP_INFO` per subflow)

*Rationale: Simple socket-level change via socket2. Requested by kernel MPTCP co-maintainer (issue #24). Per-subflow stats via `MPTCP_FULL_INFO` would show multi-path behavior in the TUI.*

### IO Optimization / Zero-Copy (issue #33)
- [ ] `sendfile()` + `mmap()` on the send path — create anonymous memory region via `mmap()` and pass fd to `sendfile()`, avoiding userspace buffer copies entirely. No temp file needed (iperf3 uses this approach). Lowest effort, biggest win on embedded/constrained devices (3x improvement measured on MIPS router: 30 → 100 Mbps)
- [ ] `MSG_ZEROCOPY` on the send path — `setsockopt(SO_ZEROCOPY)` + `send()` with `MSG_ZEROCOPY` flag. Must be optional/flag-gated: MPTCP doesn't support it yet ([mptcp_net-next#578](https://github.com/multipath-tcp/mptcp_net-next/issues/578)). Linux 4.14+
- [ ] io_uring with fixed buffers — biggest win but requires different async runtime story (tokio-uring or glommio)
- [ ] Multi-queue NIC support
- [ ] AF_XDP kernel bypass

*Rationale: Zero-copy isn't just for 100G+ — it dramatically reduces CPU overhead on embedded/constrained devices where memory copies are the bottleneck. `sendfile()+mmap()` is the pragmatic first step; `MSG_ZEROCOPY` and io_uring are bigger lifts with diminishing returns for most users.*

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
- [ ] **JSON schema for output** - publish a JSON Schema file so programmatic consumers can validate xfr's JSON and JSON-stream output

---

## Competitive Landscape

| Tool | Language | Multi-client | TUI | QUIC | Active Development |
|------|----------|--------------|-----|------|-------------------|
| iperf3 | C | No | No | No | Maintenance only |
| iperf2 | C | Yes | No | No | Active |
| rperf | Rust | Yes | No | No | Stale (since 2023) |
| nperf | Rust | ? | No | Yes | Low activity |
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
- **Language bindings / FFI** - Prometheus export and CLI cover most integration use cases.
- **Built-in storage / baselines** - Use external databases. `xfr diff` covers comparison.

If you need these features, combine xfr with purpose-built tools.

---

## Known Limitations

See [KNOWN_ISSUES.md](KNOWN_ISSUES.md) for documented edge cases and limitations.

**Potential future improvements** (from known issues):
- [ ] QUIC bitrate pacing support
- [ ] UDP reverse mode error reporting to client

---

## Contributing

See issues labeled `good first issue` for entry points. PRs welcome for any roadmap item.
