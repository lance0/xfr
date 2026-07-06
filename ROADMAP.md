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
- [x] Server presets (allowed clients and max duration; bandwidth limit reserved)
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
- [x] **Preset-scoped client allowlists** - `xfr serve --preset` now enforces `ServerPreset.allowed_clients` as an additional AND predicate on top of the regular ACL, at both TCP and QUIC accept time before auth/handshake/test allocation
- [x] **PSK file hygiene and inline-key warnings** - `--psk-file` rejects Unix files with group/other permissions, constant-time equality no longer leaks length or accepts trailing-zero variants, and inline `--psk`/`XFR_PSK` paths warn users toward `--psk-file`
- [ ] **HMAC-signed UDP feedback packets** (#70 follow-up) — the `udp_feedback_v1` packet has no auth. An on-path or spoofing attacker who can land a 36-byte packet on the client's connected upload socket can affect the live UDP-loss reading across all output paths (TUI, plain text, JSON-stream, CSV — feedback updates feed the cumulative cache that all of these consume after v0.9.14). Stream_id is not validated against the receive task's slot index (`receive_udp_feedback_only` writes to `aggregator[stream_index]`, ignoring `pkt.stream_id`), so the attacker doesn't need to guess it; landing on the connected socket's tuple is enough. The producer-side filter rejects only stale lower-denominator packets — it does NOT prevent an attacker from inflating `received` (or both `received` and `lost`) to dilute or distort the live loss percentage. The asymmetric "can only push higher" framing was wrong. Defer until someone's threat model needs it
- [x] **Slow-loris protection** - accept loop spawns per-connection tasks with 5s initial read timeout
- [x] **DataHello flood protection** - validate test_id exists before processing data connections
- [x] **Client capabilities negotiation** - client advertises capabilities; server falls back to multi-port TCP for legacy clients
- [x] **Pre-handshake connection gate** - limits concurrent unclassified connections to prevent connection-flood DoS
- [x] **Multi-port TCP IP validation** - per-stream fallback listeners validate peer IP against control connection

### Polish
- [x] **Suppress TCP send errors on graceful shutdown** - TCP `Connection reset by peer` / `Broken pipe` / `Connection aborted` at teardown now suppressed via targeted grace window (issue #25)
- [ ] **Suppress UDP/QUIC send errors on graceful shutdown** - UDP shows `Connection refused (os error 111)`, QUIC shows `sending stopped by peer: error 0` when server tears down sockets before client finishes sending; cosmetic but noisy
- [ ] **High-loss UDP `Connection refused` storm** (#70 follow-up) — under `tc netem loss 50%` on lo with short tests, TCP control handshake delay eats into test duration; if server's test-end fires before client's first UDP packet, kernel sticks ECONNREFUSED on the connected socket and rejects every subsequent send. Pre-existing in v0.9.13. Fix candidates: extend handshake budget under observed loss, recreate connected UDP socket on first ECONNREFUSED, or surface kernel-level signal to the client retry path
- [ ] **Surface kernel-side UDP drops via SO_RXQ_OVFL** (#70 follow-up) — Linux `SO_RXQ_OVFL` cmsg distinguishes receiver-buffer saturation from on-wire loss. Currently both surface as plain "lost" in the sequence-gap tracker. Track separately so users can tell whether to tune `-w`/`SO_RCVBUF` vs accept the link's actual loss
- [ ] **Control-plane DSCP for UDP feedback packets** (#70 follow-up) — `udp_feedback_v1` packets share the data socket and inherit the data DSCP, which could queue feedback behind low-priority test traffic on the return path. Mark feedback packets with a control-plane DSCP (CS6 or 0)
- [ ] **Feedback-driven scripted output cadence** (#70 follow-up, non-TUI half) — the TUI half shipped in v0.9.17 (issue #93): the sparkline now synthesizes bars from feedback state on a local clock when control-channel Intervals stall. `--json-stream`, CSV, and plain interval rows still print on TCP control `Interval` arrival and can bunch under extreme loss. The #93 approach generalizes: decouple the scripted interval clock from control-message delivery and merge the freshest cached UDP loss into scheduled rows (or emit explicit feedback-only events with a schema marker)
- [x] **Summary on early exit** (issue #35) — Ctrl+C now triggers the cancel path and displays the test summary with accumulated stats. Plain mode uses `tokio::signal::ctrl_c`; TUI treats Ctrl+C as a key event in raw mode. Double Ctrl+C force-exits (exit code 130). Fallback summary from cumulative counters when server is unreachable
- [x] **Delta retransmits in plain-text interval output** (issue #36) - plain-text interval reports now show retransmit deltas instead of cumulative totals while the final summary remains cumulative. TUI stats remain cumulative by design, and server-reported per-stream interval deltas continue in JSON/CSV output
- [x] **`omit_secs` in config file** (issue #43) — `[client] omit_secs` in config.toml sets default `--omit` value
- [ ] **Group CLI help by client/server** (issue #43) — restructure `--help` output into client-only, server-only, and shared sections like iperf3
- [x] **CSV bidir intervals lack per-direction columns** — `--csv` interval rows now carry `bytes_sent`/`bytes_received`/`throughput_send_mbps`/`throughput_recv_mbps`, matching the summary row. Columns are appended at the end of the schema (position-stable for existing parsers); empty for unidirectional tests (#56 family)
- [x] **Log client version and capability mismatches server-side** — shipped (PR #97): every control connection logs client software + protocol version, and any server capabilities the client lacks (the features that fall back that session)
- [ ] **Config-file drift sweep** — `bitrate`, `congestion`, `dscp`, `interval`, `cport`, and `bind` have no `config.toml` equivalents while their siblings do; one pass to close the gap and a test that pins CLI/config parity going forward
- [ ] **Per-stream direction splits + download-direction loss/jitter in bidir** (out-of-scope notes from issue #91) — per-stream rows in bidir show the server's combined view, and bidir has one `udp_stats` field for two directions. Both need wire-format additions; batch them with the next protocol-touching feature

### Code Quality
- [ ] **Panic-safe test lifecycle guard** - normal cancel/error/disconnect returns now clean up `active_tests`, cancel data handlers, update the Prometheus active gauge, and clear server-TUI rows. Remaining work is a true Drop/panic guard if handler panics need the same guarantee. Consider DashMap to reduce lock contention at higher concurrency
- [ ] **Decouple stats sampling from TCP control writes** (#70 follow-up) — server's interval loop currently couples `stats.record_intervals()` to `writer.write_all()`. v0.9.14 mitigates this with `MissedTickBehavior::Skip` (stops bunched-stale output) and `udp_feedback_v1` (sidesteps TCP control for live UDP loss), so further decoupling is no longer urgent. A bounded "latest-only" channel between sampler and writer would still be the durable correctness fix and would generalize to any future stats whose live visibility today rides the same write path. Packet-capture evidence from the v0.9.18 cycle: under the skew test's FIFO-bloat profile the server emits intervals at ~2 s cadence because the write blocks and Skip eats the missed ticks — the coupling is directly observable on the wire
- [x] **Fix PSK unwrap panics** - serve.rs PSK `.unwrap()` replaced with `.ok_or_else()` error propagation
- [x] **UDP encode bounds check** - `UdpPacketHeader::encode()` now validates buffer length before indexing
- [x] **Timestamp clock skew** - ISO8601/Unix timestamps now derive wall clock from `system_start + elapsed` instead of `SystemTime::now()`
- [x] **Extract magic number constants** - 6 named constants replace 12 hardcoded values (STATS_INTERVAL, CANCEL_CHECK_TIMEOUT, etc.)
- [ ] **Refactor run_test()** - serve.rs (352 lines), run_quic_test (269 lines), client equivalents similarly oversized; split into protocol-specific helpers
- [ ] **Refactor main.rs** - 340-line main() mixing CLI parsing, config building, PSK handling, dispatch; split into modules
- [ ] **Deduplicate TUI event loop** - main.rs has ~90 lines copy-pasted between active test loop and result wait loop
- [ ] **Clean up remaining dead code** - `TuiLoopResult` is gone; remaining known items include unused ProgressBar.style() and completing InstallMethod::update_command
- [x] **Add SAFETY comments** - document invariants for 4 unsafe blocks in tcp_info.rs, tcp.rs, net.rs
- [ ] **Audit unwrap()/expect() calls** - reduce calls in production code, especially auth.rs HMAC init and serve.rs PSK handling
- [ ] **Audit swallowed channel sends** - ~15 `let _ =` sites on `try_send`/`send` in serve.rs (TUI event channel, cancel/shutdown signals). Most are benign watch-channel semantics, but the cancel-path ones could mask a test that didn't actually stop; classify each and log the ones that matter
- [ ] **Finish the settings-modal restart path** - `src/main.rs` has a live TODO ("Return restart signal with new params"); settings changed mid-test from the TUI modal may not fully apply
- [ ] **Remove unused dependencies** - once_cell→OnceLock, evaluate futures, humantime, async-trait
- [x] **Join client data tasks** - Client::run now collects JoinHandles and joins with 2s timeout + abort before returning
- [ ] **Extract shared handshake logic** - ~100+ lines duplicated between TCP and QUIC paths in both serve.rs and client.rs

### Testing
- [ ] **Concurrent client tests** - simulate multiple clients to verify race condition handling
- [ ] **Fuzz testing** - fuzz control protocol JSON parsing for robustness
- [ ] **Property-based testing** - packet sequence tracking, rate limiter invariants
- [ ] **Expand rate limiting and ACL tests** - explicit rate-limit rejection and preset `allowed_clients` coverage now exist; remaining gaps are allow/deny precedence and IPv4-mapped IPv6 combinations
- [ ] **Cancellation flow tests** - client cancel, server cancel, partial stream setup
- [ ] **QUIC negative tests** - client opens fewer streams than requested, malformed messages
- [ ] **Listener backlog stress test** - verify single-port mode handles many concurrent data connections
- [ ] **UDP feedback aggregator stress test** (#70 follow-up) — exercise `spawn_udp_feedback_consumer` with `-P 32`, `-P 64`, `-P 128` under saturation. parking_lot::Mutex contention between writers (per-stream feedback recv tasks) and the 1 Hz consumer is unproven at high concurrency; high stream count + high feedback rate is the relevant failure mode
- [x] **Relax MPTCP 10-stream throughput threshold in CI** — lowered to 17 Mbps (PR #90); the bug it catches (#24 JoinHandle panic) crashes throughput to ~0, so no coverage lost
- [ ] **TUI render fuzzing** - existing TUI tests assert against `App` state and ratatui `Buffer` cells, but never put the rendered escape-sequence stream through a real terminal emulator. Drive randomized `App` state through the renderer and parse the output with the [`vt100`](https://crates.io/crates/vt100) crate to assert no panics, no overflow past widget bounds, and no escape-sequence injection escaping the server-version sanitizer or other future text fields. Eventual target: [libghostty](https://github.com/ghostty-org/ghostty) (C-ABI library from Mitchell Hashimoto's terminal, embeddable from Rust via `bindgen`) for highest-fidelity emulation and Antithesis-style deterministic fuzzing — but its API is explicitly pre-1.0 ("signatures still in flux") so wait for a tagged release before depending on it. Bugs this catches that nothing else does: extreme values overflowing layout (PB-scale transfers, jitter that won't fit format width), rapid state transitions during paint, theme switching mid-redraw, resize storms.

### CI / Infrastructure
- [x] **CI hardening (batch 1)** — cross-target smoke matrix on every PR (catches the v0.9.15 glibc-symbol class before tag), `--all-features`/`--no-default-features` test + clippy matrices (catches the LAN-172 dead-code-under-no-default-features class), rustdoc `-D warnings` doc check, `--locked` builds, concurrency cancellation, release-target cross builds via pinned cargo-zigbuild (glibc 2.17 floor) with pinned cross for Android, per-job timeouts, and an `audit.yml` PR path filter
- [ ] **`cargo-deny`** — license compliance, banned crates, and duplicate-version detection (would surface `windows-sys`-style version fan-out); complements `cargo audit`
- [ ] **`crate-ci/typos`** — cheap typo check over docs, comments, and user-facing strings
- [ ] **Windows `cargo check`** — no Windows binary is shipped, but live `#[cfg(windows)]` paths (single-port UDP fail-closed to legacy ports, `windows-sys` deps) go uncompiled by CI; a cheap `windows-latest` check keeps them building
- [ ] **Nightly concurrency sanitizer** — ThreadSanitizer over the async unit tests, or `loom` model-checking on the small sync primitives (pause/cancel watch-channel state machines, the control-reader mpsc lifecycle). Highest-rigor option for the class of bug this project keeps hitting — busy-loops, control-stream desync, cancel-while-paused freeze, split-codec races. Finicky (TSAN + tokio is flaky; loom needs loom-wrapped types), so nightly rather than per-PR

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
- [ ] **Configurable UDP packet size** (`--packet-size`) - set UDP datagram size for jumbo frame validation and MTU path testing; iperf3 `--set-mss` is TCP-only (issue esnet/iperf#861). `--probe-mtu` (v0.9.17) answers "what size survives" — this is the complement: actually *running the test* at that size
- [ ] **QUIC bitrate pacing** - the warning half shipped in PR #97 (`-b`/`-w`/`--congestion`/`--tcp-nodelay` with `-Q` now warn instead of vanishing silently); the real fix remains: pace QUIC sends with the same byte-budget loop UDP uses
- [x] **Connect timeout** (`--connect-timeout`) - shipped (PR #97): bounds TCP connect / QUIC handshake with a clear error naming the flag; off by default so existing behavior is unchanged
- [x] **Propagate `--tcp-nodelay` to the server** - shipped as `TestStart.tcp_nodelay` (wire-additive, same threading as `zerocopy`). The client's request ORs into the server's data-socket nodelay default; no capability was added because the server already runs its data sockets with `TCP_NODELAY` always-on, so an old server ignoring the field degrades to identical behavior (same call as `window_size`)
- [ ] **Test by amount** (`-n`/`--bytes`) - "send exactly 1 GiB then stop" instead of time-based; gives comparable runs across link speeds for CI and benchmarking. iperf3 parity (`-n`/`-k`)
- [ ] **Result metadata passthrough** (`--title`, `--extra-data`) - tag JSON/CSV results with a run label or arbitrary metadata for CI log ingestion; pairs with the JSON-schema item below
- [ ] **Get server output** (`--get-server-output`) - return server's JSON result to client (iperf3 parity)
- [x] **DSCP/TOS marking** (`--dscp`) - set DSCP/TOS on client TCP/UDP sockets for QoS policy testing; QUIC ignores it. Accepts numeric (0-255) or DSCP names (EF, AF11-AF43, CS0-CS7)
- [ ] **TCP Fast Open** (`--fast-open`) - reduce handshake latency for short tests; `setsockopt(TCP_FASTOPEN)` on server, `MSG_FASTOPEN` on client connect
- [ ] **CC algorithm A/B comparison** (`xfr cca-compare <host>`) - run back-to-back tests with different congestion control algorithms (BBR, CUBIC, Reno, etc.) and produce side-by-side comparison (throughput, retransmits, RTT). Leverages existing `--congestion` and `xfr diff`. BBR vs CUBIC is one of the most common network testing questions
- [ ] **Server-side bandwidth caps** (`--max-bandwidth`) - per-test server-enforced bandwidth limit to prevent abuse of public servers. iperf3 issue #937 is highly requested. Distinct from `--rate-limit` (which limits concurrent tests per IP)
- [ ] **Download-mode authoritative live loss from local recv stats** (#70 follow-up) — v0.9.14's `udp_feedback_v1` sidesteps the TCP control channel for upload-mode live loss visibility, but download (`-R`) and the receive half of bidir still rely on TCP control `Interval` messages. The client is the receiver in download mode and has authoritative recv-side stats locally; plumb them directly to consumers (TUI/plain/JSON-stream) the same way upload feedback flows
- [x] **Reverse-flow data accounting bug** (issue #81) — confirmed structural and fixed in v0.9.16: `-R` UDP displayed the server's send-side counts; the client (receiver) now overlays its own counters onto live intervals and the final result. The bidir download half had the same flaw, fixed in v0.9.17 (issue #91). Remaining wire-change follow-ups tracked below (per-stream bidir splits, bidir download loss/jitter)

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
         Modern peers use `single_port_udp_v1` by default, so all UDP
         streams target the server port via connected same-port sockets.
         Legacy peers or kernels that fail the startup self-test fall back
         to the old per-stream server data ports.
```

**TCP `--cport` scope:** only TCP data-stream source ports are pinned. The control connection still uses an ephemeral source port, and xfr fails fast if that ephemeral control port overlaps the requested data-port range.

**Remaining legacy fallback work:** a configurable server UDP data-port range still helps older peers or platforms where same-port connected UDP fails its runtime self-test.

### Medium Effort (moderate effort, high impact)
- [x] **Pause/resume** (`p` key) - real traffic pause via `Pause`/`Resume` protocol messages and a second `watch` channel to data loops (v0.7.0, issue #19)
- [ ] **Repeat mode** (`--repeat N --interval 60s`) - run N tests with delays and output summary; replaces cron-based scripting for CI/monitoring. Could extend to `xfr monitor` with local time-series storage (SQLite/JSON) and percentile tracking (p50/p95/p99)
- [ ] **Responsiveness / bufferbloat scoring** (`xfr responsiveness`) - saturate the link (upload + download) while measuring latency every 200ms, report RPM (Roundtrips Per Minute) score and bufferbloat letter grade (A-F). Follows IETF `draft-ietf-ippm-responsiveness` methodology. No single-binary CLI tool does both throughput AND responsiveness scoring — Crusader, Flent, and Apple's `networkQuality` each require separate tools or are platform-specific. Highest differentiation opportunity
- [ ] **Server UDP port range** (`--data-port-range`) - configurable ephemeral port range for server-side UDP data sockets (requested in issue #38 for strict firewall environments on Windows)
- [x] **UDP single-port mode** (issue #63) - shipped as `single_port_udp_v1`: modern peers route upload, download, bidir, and MTU-probe UDP streams to the server port via token-bearing hellos and connected same-port sockets. QUIC owns the shared UDP socket when enabled; xfr hellos are demuxed before QUIC, fixed-bit greasing is disabled server-side for sound classification, and a startup self-test gates capability advertisement. Legacy per-stream ports remain the fallback
- [ ] **UDP GSO/GRO** - kernel-level packet batching for UDP; iperf3 added this Aug 2025, would break through the 2 Gbps UDP ceiling
- [x] **UDP packet-size / MTU probe** (issue #64) - shipped in v0.9.17 as `--probe-mtu`: ladder + binary search with DF set, per-direction attribution via ack + same-size echo (`mtu_probe_v1` capability), netns CI coverage. Follow-up if users hit it: server-driven probe schedule so a reverse path *wider* than the forward one is observable

### Larger Projects (high effort, high impact)
- [ ] **Real-time traffic emulation** (`--profile voip|gaming|video`) - preset packet sizes and intervals to emulate real-time traffic: VoIP (64B/20ms bidirectional), gaming (100-300B/8-16ms), video call (1200B/33ms). Reports jitter, out-of-order, and MOS (Mean Opinion Score) estimate. Replaces IRTT for most use cases in a single binary
- [ ] **Mesh / multi-node matrix testing** (`xfr mesh`) - given a list of xfr server addresses, run all-pairs throughput/latency tests and output a matrix (table, CSV, JSON). Useful for Kubernetes cluster validation, multi-region deployments, and SD-WAN path selection. No open-source CLI tool does this
- [ ] **sendmmsg for UDP bursts** - batch multiple packets per syscall (Linux)
- [ ] **CPU affinity options** (`--affinity`) - pin streams to specific cores, reduces page faults and context switches (rperf has this)
- [x] ~~**Socket buffer auto-tuning** - optimal SO_SNDBUF/SO_RCVBUF for link speed~~ - Resolved in v0.9.9 (issue #60): xfr now defers to the kernel's TCP autotuning by default, applying `SO_SNDBUF`/`SO_RCVBUF` only when the user passes `-w`/`--window`. Application-side tuning was the wrong layer; the kernel already does this per-connection.

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
- [x] `sendfile()` on the TCP send path — shipped as `-Z`/`--zerocopy` (v0.9.16): payload lives in a `memfd_create` anonymous file and is pushed with `sendfile(2)`, skipping the per-write userspace copy. Forwarded to the server via `TestStart.zerocopy` for `-R`/`--bidir` (`zerocopy_v1` capability); MPTCP-compatible; falls back to regular writes when unsupported. **On by default since v0.9.17** (`--no-zerocopy` opts out; explicit `-Z` warns on downgrade)
- [ ] `MSG_ZEROCOPY` on the send path — `setsockopt(SO_ZEROCOPY)` + `send()` with `MSG_ZEROCOPY` flag, picked automatically under the existing `--zerocopy` flag when supported and beneficial. Must stay optional: MPTCP doesn't support it yet ([mptcp_net-next#578](https://github.com/multipath-tcp/mptcp_net-next/issues/578)). Linux 4.14+, needs error-queue completion reaping
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
- [ ] QUIC bitrate pacing support — folded into the "QUIC silently drops TestStart parameters" quick win above
- [ ] UDP reverse mode error reporting to client

---

## Contributing

See issues labeled `good first issue` for entry points. PRs welcome for any roadmap item.
