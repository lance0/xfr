# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security
- **Preset allowlists are now enforced by `xfr serve --preset`** — selecting a server preset now applies its `allowed_clients` CIDRs as an additional allowlist, enforced at TCP and QUIC accept time before auth, handshake, or test allocation. The preset allowlist is combined with the normal ACL using AND semantics, so a client must pass both. Unknown preset names fail fast at startup. (LAN-158)
- **PSK handling is stricter and safer by default** — `constant_time_eq` no longer short-circuits on length mismatch or accepts trailing-zero variants, Unix `--psk-file` inputs with group/other permissions are rejected, and both client and server warn when inline `--psk`/`XFR_PSK` is used because those paths can leak through process listings, shell history, or environment inspection. (LAN-171)

### Fixed
- **TCP send paths no longer replay bytes after short writes** — both the regular `try_write` path and zero-copy `sendfile(2)` path now maintain a caller-owned chunk offset, so partial sends resume from the byte after the short write instead of replaying the buffer prefix. Bidirectional send halves also record final retransmits from their TCP_INFO snapshot. (#113)
- **Server active-test state is cleaned up on abort, cancel, and early error** — TCP and QUIC test handlers now run their data-plane body inside a cleanup wrapper so `active_tests` entries are removed, data handlers receive cancel, the Prometheus active-test gauge is decremented, and the server TUI does not retain stale active rows when a test exits before normal completion. (#114)
- **Server final TCP_INFO now aggregates all TCP streams** — previously the server's final `TestResult.tcp_info` was built from the last snapshot stored in the legacy `TestStats.tcp_info` vector, which could reflect a single stream or a pre-test snapshot. The server now saves each data socket's final `TcpInfoSnapshot` on its per-stream `StreamStats` and uses `TestStats::final_local_tcp_info()` for the final result, matching the client path. (LAN-155)
- **TCP RTT averages no longer truncate fractional microseconds** — `TestStats::poll_local_tcp_info()` and `TestStats::final_local_tcp_info()` computed RTT averages with integer division, silently dropping fractional microseconds. They now use floating-point division with `.round()`, so e.g. averaging 100 µs and 101 µs reports 101 µs instead of 100 µs. (LAN-164)
- **Diff zero baselines now register as regressions** — when the baseline reported zero retransmits or zero RTT and the current run reported a nonzero value, `xfr --diff` returned a 0.0 % change and `Verdict: OK`, masking a real regression. The change percent is now `+inf%`, `is_regression` now considers retransmit and RTT increases against the configured threshold, and the plain-text summary flags the line as `[FAIL]`/`[WARN]`. (LAN-162)
- **Push Gateway timeout no longer silently dropped** — `PushGatewayClient::new()` built the `reqwest` client with `.build().unwrap_or_default()`, so if the builder ever failed (or was misconfigured) the code fell back to a default client and ignored the 30-second timeout. The constructor now returns `anyhow::Result`, and `maybe_push_metrics()` logs the error instead of falling back. (LAN-156)
- **JSON output now propagates serialization errors** — `output_json()` and `output_interval_json()` used `.unwrap_or_else(|_| "{}".to_string())`, so a serialization failure would silently emit an empty JSON object. Both functions now return `anyhow::Result<String>`, and callers either propagate the error or log it and skip the interval. (LAN-156)
- **Push Gateway metrics emit `# HELP`/`# TYPE` once per family** — the text formatter previously wrote `# TYPE xfr_stream_*` before every stream sample, producing invalid Prometheus exposition with repeated headers. Headers are now emitted once per metric family, followed by all samples for that family. (LAN-157)
- **Release workflow no longer references stale completions artifact path** — the `Generate checksums` step moves every tarball to `artifacts/`, but the release job still listed `artifacts/completions/completions.tar.gz`, which no longer exists after the move. The stale path is removed; `artifacts/*.tar.gz` already covers the completions tarball after the move. (LAN-157)
- **Installer no longer points Intel Macs to a removed artifact** — `install.sh` still resolved `x86_64-apple-darwin` on Intel Macs, but release builds for that target were removed. The script now prints a clear message directing users to `cargo install xfr`. (LAN-157)
- **Malformed protocol versions are no longer treated as compatible** — `versions_compatible()` now rejects malformed strings instead of defaulting parse failures to major version `0`. (LAN-166)
- **Receive-side TCP resets at normal teardown are treated as EOF without hiding mid-test failures** — `receive_data()` and `receive_data_half()` now handle peer-close errors such as Linux `SO_LINGER=0` abortive sender teardown as graceful end-of-stream only after cancel or near the expected test deadline; mid-test resets remain fatal so truncated runs are not reported as successful. (LAN-166)
- **Socket buffer tuning once again degrades gracefully on OS rejection** — invalid `-w` values still fail up front, but `SO_SNDBUF`/`SO_RCVBUF` rejections from platform limits are warning-only again so oversized window requests do not abort otherwise valid tests. (LAN-165)
- **`xfr diff` no longer fails default comparisons on tiny TCP metric noise** — the default threshold is now 5%, and retransmit/RTT row labels use the same threshold as the final verdict. Zero-baseline TCP metric regressions still show as `+inf%` and fail when above threshold. (LAN-162)
- **Server bidirectional TCP no longer double-counts send-half retransmits** — the send-half path records the final TCP_INFO snapshot once, and the server result/prometheus paths no longer add the same retransmit count a second time.
- **Failed tests are distinct in the server TUI** — TCP and QUIC error paths now emit a failed-test event instead of the completed-test event, and the server dashboard tracks failed tests separately.
- **Bare-major protocol versions are accepted again** — version compatibility still rejects malformed versions, but a peer that sends `1` is treated as protocol-major `1` for compatibility with earlier loose parsing. (LAN-166)
- **TestAck and MTU probe packets reject invalid wire shapes** — `TestAck` deserialization now rejects messages that contain both `udp_token` and per-stream `data_ports`, and padded MTU probes now reject declared sizes below the header or beyond the received datagram. (LAN-167)
- **MTU probing handles echo-only success and IPv6 literals correctly** — an MTU echo now proves forward-path success even when the ack is lost, and literal IPv6 probe targets are bracketed before address resolution. (LAN-168)
- **TUI average speed and retransmit fallback are no longer biased** — zero-throughput intervals now count toward the live average, and the fallback path for older servers accumulates per-stream retransmit deltas instead of replacing the total with each interval's latest snapshot. (LAN-163)
- **Server preset max duration is applied with CLI precedence** — `max_duration_secs` from `xfr serve --preset` now applies when `--max-duration` is not supplied on the CLI, and `bandwidth_limit` is documented as reserved/unused rather than implied to be enforced. (LAN-173)
- **Explicit `--probe-mtu -t 10s` duration is preserved** — `-t`/`--time` is now tracked as optional input, so an explicit 10-second MTU probe is no longer mistaken for the default and replaced with the longer probe default. (LAN-172)
- **IPv6 MTU probe ladder uses IPv6 overhead for both bounds** — the probe size ladder now brackets IPv6 at 1232..=9168 bytes instead of mixing IPv4-derived payload bounds into IPv6 probes. (LAN-172)

### Maintenance
- **Rate-limit integration coverage now asserts rejection** — the second concurrent client in `test_rate_limit` must fail promptly, and the long-running first client is aborted so the test remains fast. (LAN-172)
- **`--no-default-features` no longer warns on the mDNS fallback stub** — the discovery fallback `register_server` stub is explicitly allowed as dead code when discovery support is compiled out. (LAN-172)
- **Dependency and workflow action updates** — `actions/checkout` v6→v7, `actions/cache` v5→v6, plus Rust lockfile updates for ratatui 0.30.2, mdns-sd 0.20.1, anyhow 1.0.103, uuid 1.23.4, quinn 0.11.11, rustls 0.23.41, and their transitives. (#109, #111, #112)
- **Local review marker is ignored by git** — `.last_review_sha` is now listed in `.gitignore`.

## [0.9.20] - 2026-06-23

### Security
- **Updated `quinn-proto` to 0.11.15** to address [RUSTSEC-2026-0185](https://rustsec.org/advisories/RUSTSEC-2026-0185) (High, 7.5) — a remote memory-exhaustion denial of service in QUIC stream reassembly, where unbounded buffering of out-of-order stream data could let a peer drive a server's memory usage without bound. This affects xfr's QUIC server path (any host running `xfr -s` reachable by an untrusted QUIC client). Lockfile-only update — no source or API changes, and no configuration change is required.

### Maintenance
- **CI: bumped the Docker publish actions** used by the GHCR release job — `setup-buildx-action` v3→v4, `login-action` v3→v4, `setup-qemu-action` v3→v4, `build-push-action` v6→v7, and `metadata-action` v5→v6. These track the actions' Node 24 runtimes; the published multi-arch image is unchanged.

## [0.9.19] - 2026-06-14

### Added
- **Restart a test from the TUI** (issue #100) — the `r` key, and Settings → Test → *Apply & Restart*, now cancel and drain the current run before spawning a fresh one in place, instead of requiring a full client restart. Each restarted run gets its own client task and progress channel, so stale progress from the prior run cannot bleed into the next. A settings restart applies stream count, protocol, duration, direction, and bitrate; QUIC-incompatible knobs (`-b`/`-w`/`--congestion`/`--tcp-nodelay`/`--dscp`) are surfaced in the TUI history the same way the CLI warns about them. The settings modal gains a Bitrate row (stepping through unlimited / 1M / 10M / 100M / 1G / 10G), and the footer and help overlay list the `r` key. Based on the contribution in #101/#102 by @flotpg.

### Fixed
- **Repeated `q` no longer skips the test summary** — while a cancel was already in flight, a second `q` (or key-repeat from a held key) was treated as "exit now" and could leave before the server's final Result arrived. Now `q` stays graceful while cancelling (the existing 3-second deadline still bounds the wait), and a second Ctrl+C remains the explicit force-exit, matching the documented signal behavior.

### Added
- **CSV interval rows carry the bidirectional split** (issue #56 follow-up) — `--csv` interval output gains `bytes_sent`, `bytes_received`, `throughput_send_mbps`, and `throughput_recv_mbps` columns, matching the fields the CSV summary row and final JSON already expose. The new columns are appended at the end of the row so existing parsers that index columns by position keep working; unidirectional tests leave them empty, like the summary row already does.
- **Single-port UDP data plane (`single_port_udp_v1` capability)** (issue #63) — all per-stream UDP test traffic now flows to the server's main port (default 5201/UDP) instead of per-stream ephemeral ports, so firewalls only need one UDP port open. Per stream, the client sends a small token-bearing hello datagram to the main port (retried at 100 ms up to ~5 s, then the test fails loudly); the server creates a fresh UDP socket, binds it to the same port via `SO_REUSEADDR`+`SO_REUSEPORT`, `connect()`s it to the client's source address, and acks from that socket. The kernel's UDP lookup scores connected sockets above the wildcard, so line-rate data rides the connected socket and the shared socket only ever sees hellos. The per-test token (16 random bytes, returned in `TestAck.udp_token`) routes hellos to the right test across NAT and concurrent clients. The main UDP port is owned by the quinn QUIC endpoint; it now gets a tee implementing `quinn::AsyncUdpSocket` that delegates to the runtime's quinn-udp-backed socket (GSO/GRO/ECN paths intact) and diverts only xfr hello datagrams. The classifier (hello framing first, then QUIC long-header bit 0x80, then short-header fixed bit 0x40) is made sound by disabling QUIC-bit greasing on the server endpoint — RFC 9287 greasing is permission-based, so not advertising it stops compliant peers from clearing the fixed bit toward us. The hello/ack magic `b"\x00XFR"` lives in the 0b00 first-byte space QUIC can never use. With `--no-quic`, a plain shared socket serves the hello lane instead. The server advertises the capability only after a startup self-test proves the running kernel actually routes a connected-socket datagram past a same-port wildcard (SO_REUSEPORT semantics vary by platform); on failure — and against older peers — the legacy per-stream ephemeral-port path is used unchanged. Single-port is the default when both peers advertise, mirroring `single_port_tcp`, and covers upload, download, bidir, and `--probe-mtu`.

- **`--connect-timeout <DURATION>`** — bound the control-connection establishment (TCP connect or QUIC handshake) and fail with a clear error instead of waiting on OS defaults, which against a dead or filtered server can mean minutes. Off by default (behavior unchanged unless set); aimed at CI and scripted runs.

### Changed
- **QUIC tests now warn about ignored flags** — `-b/--bitrate`, `-w/--window`, `--congestion`, and `--tcp-nodelay` have never applied to QUIC (pacing and socket tuning are not implemented on that path), but the client accepted them silently, so a rate-limited or buffer-tuned QUIC test ran untuned with no indication. Each now prints a warning, matching what `--dscp` already did. Actually implementing QUIC pacing remains on the roadmap.
- **The server log now answers "which client was that"** — every control connection logs the client software string and protocol version, plus a line listing any server capabilities the client doesn't share (i.e., the features that silently fall back for that session). Previously the log showed only the peer address.
- **`--tcp-nodelay` now propagates to the server** — the flag previously shaped only client-created sockets; the client now forwards the request in `TestStart.tcp_nodelay` (wire-additive, absent = false) and the server ORs it into its data-socket configuration. The server has always enabled `TCP_NODELAY` on its data sockets, so the OR cannot regress existing behavior — the field makes the client's intent explicit on the wire and keeps it honored if the server-side default ever changes. Older servers ignore the field, an acceptable degradation since they already run with nodelay on (same call as `window_size`; no new capability).

### Maintenance
- **Control-channel skew CI test recalibrated** — packet captures showed the test's FIFO-bloat profile has two stable delivery rhythms (~2 s batches of 2 interval lines, ~4 s batches of 3) on a *correctly functioning* NODELAY control channel, and which rhythm a run locks onto is decided by startup phase; the single-port UDP handshake shifted that phase and made the previously-rare rhythm dominant. The assertion now targets what the #70 bug actually does (total collapse to one timestamp: 5+ lines in one bunch or fewer than 3 distinct timestamps) instead of a threshold that sat between the two physical rhythms. Findings documented in KNOWN_ISSUES.md.

### Library API (pre-1.0 break)
- `client::ClientConfig` gains `connect_timeout: Option<Duration>` (`Default` is `None`); struct-literal constructors must supply it.
- `protocol::ControlMessage::TestStart` gains `tcp_nodelay: bool` (serde-default, omitted when false); struct-literal constructors must supply it.
- `protocol::ControlMessage::TestAck` gains `udp_token: Option<String>` (serde-default, wire-additive); struct-literal constructors must supply it.
- `udp::receive_udp` and `udp::respond_mtu_probes` gain a trailing `single_port_ack: Option<[u8; 24]>` parameter.
- New public items: `udp::{UdpHelloPacket, SharedSocketLane, classify_shared_datagram, single_port_udp_handshake, respond_single_port_hellos, encode_hello_token, parse_hello_token, UDP_HELLO_*}`; `net::{create_shared_udp_socket, create_connected_udp_same_port, single_port_udp_self_test}`; `quic::XfrHelloDatagram`; `protocol::{SINGLE_PORT_UDP_CAPABILITY, supported_capabilities, supported_capabilities_without}`; `ControlMessage::{server_hello_with_capabilities, server_hello_with_auth_and_capabilities}`.
- `quic::create_server_endpoint` gains a trailing `xfr_hello_tx: Option<mpsc::Sender<XfrHelloDatagram>>` parameter.

## [0.9.17] - 2026-06-11

### Added
- **Path-MTU probe: `--probe-mtu`** (issue #64) — discovers the largest UDP payload that survives the path in each direction, instead of running a throughput test. The client walks a ladder of common wire MTUs (576, 1280, 1492, 1500, 4352, 9000, 9216) and binary-searches the gap, RFC 8899 style, with the IP don't-fragment flag set so middleboxes must drop oversized packets rather than quietly fragment them. Each probe gets two replies from the server: a small ack (proves the client→server direction) and a same-size echo (proves server→client), so an asymmetric path shows up as ack-without-echo — per-direction attribution was brettowe's suggestion on the issue. Output is a per-size table plus the largest surviving payload and derived path MTU per direction; `--json` carries the full report. Requires a server advertising the new `mtu_probe_v1` capability (the client refuses old servers up front, since they'd silently swallow probes). Validated in a netns harness with a 1500-byte middle hop on a jumbo-framed client (`test-mtu-probe-ns.sh`, now a CI job).

### Changed
- **Zero-copy TCP sends are now on by default** (issue #33 follow-up) — `sendfile(2)` needs only Linux 3.17+ and every unsupported configuration (non-Linux, old kernels, old servers, sockets that reject `sendfile`) already falls back to regular writes, so there is no reason to make users opt in to the cheaper send path. `-Z`/`--zerocopy` is kept: passing it explicitly upgrades the silent fallback to a warning (non-Linux client sends, or a `-R`/`--bidir` server that doesn't advertise `zerocopy_v1`). New `--no-zerocopy` flag opts out. Payload semantics are unchanged, and on links where the sender isn't CPU-bound results are identical — where it is, throughput now reflects the network rather than the sender's copy overhead.

### Fixed
- **TUI sparkline keeps advancing when the control channel stalls** (issue #93, split from #70) — bars were appended only on full TCP `Interval` arrivals, and upload-mode saturation stalls exactly that channel, so the graph froze while the `udp_feedback_v1` loss counter stayed live. `tick()` now synthesizes a bar at the regular cadence once two report intervals pass without one, taking loss tint from feedback deltas not yet rendered into any bar and repeating the last known bar height (a zero-height bar draws no glyph, leaving nothing to tint — the flat tinted plateau reads as "no fresh throughput, loss still arriving"). Cumulative loss is tracked against what's been *drawn*, so a resuming `Interval` never re-counts loss already shown during the stall. Normal 1 Hz delivery is visually unchanged.
- **Bidir UDP no longer reports the server's send rate as download throughput** (issue #91) — same root cause as the `-R` fix in 0.9.16 (issue #81), on the other code path: in bidirectional UDP tests the download half of the split (`bytes_received`, `throughput_recv_mbps`) was computed from the server's send-side counters, which track the requested `-b` rate rather than what arrived. The client now overlays its own receive counters onto both the live split and the final result, and the combined totals are rebuilt from the two corrected halves (upload was always receiver-truth — the server counts what it actually received). Per-stream rows still show the server's combined view; splitting those per-direction needs wire changes and is out of scope here.

### Library API (pre-1.0 break)
- `client::ClientConfig.zerocopy` is now a `ZerocopyMode` enum (`Off` / `Auto` / `Requested`) instead of `bool`; `Default` is `Auto`. `Auto` and `Requested` behave identically except `Requested` warns on downgrade. `tcp::TcpConfig.zerocopy` stays `bool`.
- New public module `probe` (`ProbePacket`, `SizeSearch`, `MtuProbeReport`, `run_probe`); `client::ClientConfig` gains `mtu_probe: bool`; `protocol::ControlMessage::TestStart` gains `mtu_probe: bool` and `protocol::TestResult` gains `mtu_probe: Option<MtuProbeReport>` (both serde-default, wire-additive); `udp::respond_mtu_probes` and `net::set_dont_fragment` are new.

## [0.9.16] - 2026-06-10

> v0.9.15 was tagged but never released: its release CI failed on the aarch64-gnu cross build (glibc < 2.27 lacks the `memfd_create` wrapper; now invoked via raw syscall). All v0.9.15 changes ship here.

### Fixed
- **`-R` UDP no longer reports the server's send rate as throughput** (issue #81) — in download mode the server is the UDP sender, and both the live intervals and the final result carried its `bytes_sent`: every `send_to` the kernel accepted counted, so over a constrained link the display tracked the requested `-b` rate (brettowe's 999.7 Mbps over 100 Mbps Wi-Fi) instead of what arrived. The client — the receiver, the only side that knows wire truth — now overlays its own receive counters onto live intervals (TUI, `--json-stream`, CSV, plain) and the final result (total, per-stream, throughput). Forward mode was always correct because there the server is the receiver. Repro: CPU-starved receiver with a 4 KB receive buffer showed 2.6 Gbps sender-side vs 1.18 Gbps actually received; the display previously claimed the former and now reports the latter, exactly matching received-packets × packet-size.
- **`-R` UDP results now include loss and jitter** — the client's `receive_udp` already computed full receiver-side `UdpStats` (loss %, jitter, out-of-order, max jitter) but discarded them; download mode showed no UDP Stats block at all. They're now recorded and attached to the final result, and live loss in download mode is derived from the client's own packet trackers rather than the server's (empty) receiver counters.

### Added
- **Zero-copy TCP sends (`-Z`/`--zerocopy`, Linux)** (issue #33) — the send payload now lives in a `memfd_create` anonymous file and is pushed to the socket with `sendfile(2)`, skipping the userspace-to-kernel copy that `write(2)` performs on every 128 KB chunk. On CPU-bound senders (embedded routers, SBCs) the copy itself is often the throughput bottleneck — iperf3's equivalent `-Z` measured ~3x on a MIPS router. `sendfile` was chosen over `MSG_ZEROCOPY` for the first step because it works on old kernels, needs no error-queue completion reaping, and is MPTCP-compatible; `MSG_ZEROCOPY` remains on the roadmap under the same flag. Opt-in, TCP-only (conflicts with `-u`/`-Q` at parse time), and never fails a test: non-Linux platforms, kernels without `memfd_create`, and sockets that reject `sendfile` all fall back to regular writes with a warning. Payload semantics are unchanged (`--random` default / `--zeros` fill the memfd exactly as they fill the regular buffer).
- **`zerocopy_v1` capability + `TestStart.zerocopy` field** — for `-R`/`--bidir`, the client forwards the zero-copy request in `TestStart` (wire-additive, absent = false) and the server applies it to its own send paths. Servers predating the field silently ignore it, so the client warns when the server doesn't advertise `zerocopy_v1`.

### Library API (pre-1.0 break)
- `tcp::TcpConfig` and `client::ClientConfig` gain `zerocopy: bool`. Struct-literal constructors must supply it (`Default` is `false`).
- `protocol::ControlMessage::TestStart` gains `zerocopy: bool` (serde-default, omitted when false).
- New module `zerocopy` with `ZerocopyPayload` (memfd construction + async sendfile chunk sends).

### Maintenance
- Rust dependencies group bump (PR #86): ratatui 0.30.1, clap_complete 4.6.5, serde_json 1.0.150, hyper 1.10.1, mdns-sd 0.20.0, uuid 1.23.3, chrono 0.4.45, rcgen 0.14.8, dashmap 6.2.1, socket2 0.6.4. No source changes required.
- Bump `Cargo.toml` to `0.9.16`.

## [0.9.14] - 2026-05-03

### Fixed
- **Live UDP loss counter no longer stalls under upload-mode saturation** (issue #70 final fix) — v0.9.13's `TCP_NODELAY` partially addressed the bug but brettowe's retest showed 8 subsequent intervals still bunched at one end-of-test client-side timestamp. Root cause was `tokio::time::interval` defaulting to `MissedTickBehavior::Burst` on the server's stats sampling timer: when `writer.write_all()` stalled under the back-pressure that the saturated UDP uplink induces on TCP control, missed ticks accumulated and fired as a burst when the writer unblocked, producing stale interval samples with fresh client-side arrival timestamps and misleading throughput numbers. `Skip` now drops the stale ticks; cumulative state in `StreamStats` atomics still surfaces correctly on the next live tick and at end-of-test. Applied unconditionally on both `run_test` interval-loop sites; benefits even pre-v0.9.14 clients pairing with a v0.9.14 server.
- **`--omit` no longer folds hidden UDP loss into the first visible interval** — the new cumulative-loss tracker added below was advancing its cache on every progress arrival, but the printed-line baseline only advanced when a line printed. With `--omit 3`, the first visible interval would report all loss accumulated during seconds 0-3 as one jumbo delta, defeating the purpose of `--omit`. The baseline now advances during the omit window so visible lines reflect only loss observed during printed intervals.

### Added
- **UDP receiver feedback (`udp_feedback_v1` capability)** (issue #70 final fix) — when both peers advertise the capability, the server now emits a 36-byte cumulative `(packets_received, packets_lost)` UDP packet back to the client at 2 Hz on the same data socket, sidestepping the TCP control channel for live UDP loss reporting. Wire format: `b"XFRF"` magic + version + kind + flags + `stream_id` + reserved + `elapsed_ms` + cumulative `packets_received` + cumulative `packets_lost`, all big-endian, fixed 36 bytes. Length-first demux at receive sites distinguishes feedback from data packets without inspecting sequence-number bits. Cumulative-not-delta semantics let the client recover from any dropped feedback packet without needing the lost intermediate state. Capability negotiation gates emission so older clients (which wouldn't know to listen) never see a packet they don't understand.
- **Producer-side monotonic-denominator filter on the client** — both the TCP control `udp_progress` decode site and the UDP feedback aggregator funnel updates through `UdpProgressFilter::apply`, which admits only readings whose `(received + lost)` denominator is at-least-as-fresh as anything we've seen before. Atomic CAS via `fetch_update` so two producers cannot race a stale store after a fresh one. Applies in addition to TUI display: plain text, CSV, and JSON-stream output use the cached cumulative as the source of truth for the per-line `lost` field, so the freshest reading from either source flows through to scripted consumers, not just the TUI live counter.
- **Live UDP loss in non-TUI output paths** — `--no-tui --json-stream` / `--csv` / plain interval output now reflects the freshest `udp_progress` from either TCP control or UDP feedback. Previously these consumers used per-stream `streams[].lost` from the most recent TCP `Interval` only, which under control-channel stalls could be several seconds stale. Falls back to the per-stream sum for sessions where `udp_progress` is never sent (paired with a pre-0.9.11 server, or non-UDP tests).
- **Docker repro harness for issue #70** (`docker/Dockerfile.repro`, `docker/repro-issue-70.sh`, `docker/README.md`) — multi-stage build with the current branch and the released v0.9.13 baseline side-by-side. `docker run --rm --cap-add=NET_ADMIN xfr-repro` runs hard assertions on the new build (max bunch ≤ 2, time-to-first-loss < 5s, live mid-run loss observed); `--baseline` prints diagnostics for narrative comparison without gating on a threshold. Stays out of CI — the existing 2× oversubscription `control-channel-skew` job remains the regression floor; the harness is for human-driven A/B at brettowe's 10× recipe before publishing.

### Changed
- **`TestProgress` schema (pre-1.0 break)** — adds `udp_feedback_only: bool` so consumers can distinguish a feedback-only update (only `udp_progress` carries truth; everything else is sentinel/None) from a full TCP `Interval` update. Three consumers handle the partial variant: `App::on_progress` early-returns after updating UDP loss state and preserves all other field values; `main.rs` print loop skips feedback-only entries entirely (the cumulative cache picks up the freshness for the next full interval); cross-version compat test path adopts the new field.
- **Server bidir mode no longer emits UDP feedback** — feedback is upload-mode-only by design. Bidir's server-side recv half was passing `client_supports_udp_feedback` through to `receive_udp` even though the client's bidir recv has no consumer for those packets; emission was pure overhead on the return path. Bidir always passes `false` now.
- **`receive_udp` skips feedback packets in `bytes_received` accounting** — the length-first demux previously rejected feedback before the data path but bumped `bytes_received` first. With server bidir gating that's a moot path post-fix, but defense-in-depth: feedback bytes never count toward `bytes_received`, which tracks test-data wire bandwidth.
- **Capability list factored into a single `SUPPORTED_CAPABILITIES` const** — `client_hello`, `server_hello`, and `server_hello_with_auth` previously each had a duplicated `Vec<String>` literal. Future capability additions now touch one line. New `capability_advertised(&capabilities, name)` helper centralizes the matcher used at both negotiation sites.

### Library API (pre-1.0 break)
- `client::TestProgress` gains `udp_feedback_only: bool`. Constructors must supply it.
- `client::UdpProgressFilter` and `client::UdpFeedbackAggregator` are new public types backing the producer-side filter and aggregator.
- `udp::receive_udp` signature gains a trailing `feedback_enabled: bool` parameter.
- `udp::receive_udp_feedback_only(socket, aggregator, stream_index, cancel)` is new; spawned per-stream on the client in upload mode.
- `udp::UdpFeedbackPacket` and `UDP_FEEDBACK_SIZE` / `UDP_FEEDBACK_MAGIC` / `UDP_FEEDBACK_VERSION` / `UDP_FEEDBACK_KIND_RECEIVER_PROGRESS` constants exported.
- `protocol::SUPPORTED_CAPABILITIES` and `protocol::capability_advertised` exported.
- `stats::StreamStats::udp_progress_snapshot()` exported for callers that need a coherent `(received, lost)` pair.

### Maintenance
- Bump `Cargo.toml` to `0.9.14`.

## [0.9.13] - 2026-05-03

### Fixed
- **Live UDP loss counter no longer stuck at 0% under saturated links** (issue #70 follow-up) — `TCP_NODELAY` was not being set on the control connection. With Nagle still active, the periodic `Interval` messages (~150-byte 1 Hz writes) coalesced waiting for an MSS-sized payload (which never arrives — they're tiny) or a delayed ACK from the peer. Under heavy parallel UDP data load on a saturated path (Wi-Fi, rate-limited links, anything where ACK turnaround stretches), the kernel held every queued segment for the duration of the test and flushed the entire backlog in a single burst when data traffic stopped. The TUI live counter appeared permanently stuck at 0% during the run, then jumped to the final value at quit. iperf3 sets `TCP_NODELAY` on its control channel for exactly this reason. New `tcp::configure_control_stream` helper applies it before splitting the stream into reader/writer halves; called at three sites (server's accepted control connection, client's connecting control connection, server's auth-handshake fallback path). Reproduced and verified with a `tc netem` 50 Mbps + 50ms-delay simulation: the pre-fix binary collapses 3+ interval lines to a single end-of-test timestamp; the post-fix binary spreads them across the run with at most a 2-line tail collision.

### Added
- **CI regression test for the bunching pattern** (`test-control-channel-skew.sh`, runs as the `Control-channel skew (#70 regression)` job). Applies a 50 Mbps shaper + 50ms each-way delay to `lo`, runs an 8-second UDP test at 100 Mbps target (2× oversubscription), and asserts no 3-or-more interval lines share a client-side timestamp. Catches future regressions where the `TCP_NODELAY` plumbing is dropped from any of the three control-stream sites or a new code path forgets to call the helper.

### Maintenance
- Rust dependency group bump (PR #76): `clap_complete` 4.6.2 → 4.6.3, `rustls` 0.23.39 → 0.23.40. Patch-version updates only, no source changes required.

## [0.9.12] - 2026-05-02

### Added
- **`-w`/`--window` now applies to UDP socket buffers** (#70 follow-up) — previously the flag only set TCP `SO_SNDBUF`/`SO_RCVBUF`. UDP sockets used the kernel default regardless. On high-rate UDP flows where the receiver's kernel UDP buffer can saturate (weak/loaded receivers, default `net.core.rmem_max` lower than line-rate × ~1s), the kernel tail-drops new arrivals and the loss only surfaces as a sequence-gap once traffic stops — rendering as live `Packet Loss: 0.0%` while the test is running and a high final loss percent on quit. `-w 16M` is the immediate workaround for that pattern; the value propagates from client to server in `TestStart` (already wired for TCP since v0.9.9) and now lands on UDP `SO_SNDBUF`/`SO_RCVBUF` on both ends.

### Changed
- **`setsockopt` failures on `-w` now surface as warnings** — previously rejections (typically `net.core.rmem_max` exceeded without `CAP_NET_ADMIN`) were swallowed at debug level. Users running `-w 16M` for #70-style troubleshooting need to see the rejection rather than have it disappear silently. Both `SO_SNDBUF` and `SO_RCVBUF` are still attempted independently, and the warning identifies which buffer failed.

### Maintenance
- Rust dependency group bump (PR #71): clap 4.5 → 4.6, clap_complete 4.5 → 4.6, hyper 1.8 → 1.9, mdns-sd 0.17 → 0.19, toml 0.9 → 1.1, rustls 0.23.37 → 0.23.39, hmac 0.12 → 0.13, sha2 0.10 → 0.11, rand 0.9 → 0.10, plus libc, uuid, once_cell, tracing-subscriber, tracing-appender, tempfile patch bumps. Adapted source to API moves in hmac (constructor moved from `Mac` to `KeyInit`) and rand (user-facing `Rng` methods moved to `RngExt`); no behavior change.

## [0.9.11] - 2026-04-30

### Fixed
- **Live UDP packet-loss counter during the run** (issue #70) — the Packet Loss line in the TUI was stuck at 0.0% for the entire test and only updated to the real value at completion. With `-t 0` (infinite mode) the real value was never visible. Server now ships a cumulative packet-counts snapshot (`UdpIntervalProgress { packets_received, packets_lost }`) on every periodic Interval message; client derives the loss percent locally and the TUI updates it live. Cumulative counts are snapshotted into `IntervalStats` at interval emission time so deltas between consecutive samples correspond to the same window. Reported by @brettowe.
- **Final UDP loss accounting only counts valid xfr packets** — `UdpStats.packets_received` and `packets_sent` now exclude short, malformed, or foreign datagrams that can't be header-decoded. Previously such datagrams inflated `packets_received` and silently understated the final loss percent. `bytes_received` continues to count every byte the wire delivered.

### Added
- **Throughput sparkline tints by per-interval loss severity** (issue #70) — lossy intervals are visually distinct from clean intervals at the same height: clean stays the graph color, light loss (<1% per-interval rate) tints warning, heavy loss (≥1%) tints error. Per-interval rate computed from `udp_progress` deltas, so a single-packet hiccup and a heavy drop burst no longer collapse to the same flat tint. Magnitude unknown (TCP run, pre-0.9.11 server, or first UDP sample) stays the graph color — honest "no signal" rather than a misleading tint. Sparkline widget gains a `.styles(&[Style])` per-sample override.
- **Freshness signal for the Packet Loss line** — the line renders dimmed `--%` when paired against a pre-0.9.11 server, or before any UDP traffic has been observed. Without this, an old server would render a stale `0.0%` next to actual loss bursts in the sparkline. `App.udp_lost_percent` is now `Option<f64>` end-to-end.

### Changed
- **Jitter rolling-window label** (issue #72) — the running display now reads `Jitter: 0.86 ms (10s avg: 0.03 ms)`. The previous `(10s: …)` form read like a stuck timer; @pythonwood opened an issue thinking the display was broken on a v0.9.10-client → v0.9.6-server pairing.

## [0.9.10] - 2026-04-22

### Fixed
- **TCP teardown no longer hangs under rate-limited paths** (issue #54) — the v0.9.8 SO_LINGER=0 fix didn't take effect when the send loop was parked in `stream.write().await` under heavy backpressure (tc rate limiting, slow peers, MPTCP subflows filling). The loop would block past the configured deadline, never reaching the drop/close that would trigger the abortive close. `send_data` and `send_data_half` now race the pending `write()` against cancel and deadline in a `biased tokio::select!`, so either signal breaks the loop and lets `SO_LINGER=0` do its job. Confirmed by @matttbe against his MPTCP + tc reproducer.
- **TUI elapsed time stays live during data gaps** (issue #62) — `app.elapsed` was only updated when the server's `Interval` progress message arrived. On lossy paths that starved the control channel (e.g. brettowe's WiFi test with packet-drop bursts), the elapsed counter — and by extension the progress bar position — could freeze for several seconds until the next message landed, creating the impression of a "stall" even though the TUI was still redrawing at 20 Hz. The loop now refreshes `elapsed` from the wall clock on every iteration; `on_progress` no longer writes `elapsed` (doing so was a visual no-op, since the next tick immediately overwrote the server's value). Pause handling shifts `start_time` forward by the pause duration on resume, so the elapsed counter excludes paused time. Reported by @brettowe.
- **Infinite-duration (`-t 0`) TUI now shows a live elapsed counter** — the `{}s/∞` display was relying on the same `elapsed` field that could go stale, so an infinite test on a flaky link looked frozen. Covered by the same fix.

### Added
- **Client/server version in the Configuration panel** (issue #62) — the TUI now shows `xfr/<client-version> ↔ <server-version>` so cross-version test pairings are obvious at a glance. Server version is captured from the `Hello` handshake (already wire-supported; just wasn't surfaced). Requested by @brettowe.

### Changed
- **TUI jitter line shows latest + smoothed together** (issue #48 follow-up) — the running display now reads `Jitter: 0.02 ms (10s: 0.03 ms)` so the latest per-interval aggregate and the 10-second rolling mean are visible side by side. Resolves the confusion where the rolling mean could stay above any single sample's value, making the live display look inconsistent with the server's authoritative final. Completed state still shows just the final value (no smoothing companion — the final is authoritative). Reported by @brettowe.

### Security
- **Sanitize server-advertised version before rendering** — the `Hello.server` field crosses a network trust boundary and was being rendered verbatim into the Configuration panel. A hostile or compromised server could send ANSI escape sequences (clear screen, move cursor, OSC commands) or a very long string and have the user's terminal act on them. Control characters are now stripped, length capped at 32, and empty or all-control inputs fall back to `(unknown)`.
- **Bump rustls-webpki 0.103.12 → 0.103.13** (RUSTSEC-2026-0104) — reachable panic in CRL parsing. Transitive upgrade via Cargo.lock; no manifest change.

## [0.9.9] - 2026-04-21

### Added
- **Max jitter and packet size in UDP summary** (issue #48 follow-up) — the final UDP summary now reports `Jitter Max` (peak of the RFC 3550 running estimate across the test) alongside the average, and `Packet Size` (UDP payload bytes). Surfaced in plain text and JSON. Requested by brettowe for NFS UDP packet-size tuning context.
- **`-w` short alias for `--window`** (issue #60) — matches iperf3 muscle memory.

### Changed
- **Bare-integer duration arguments mean seconds** (issue #61) — `-t 10`, `--max-duration 60`, `--rate-limit-window 30`, and discover `--timeout 5` now accept plain integers as seconds, matching iperf3 muscle memory. Unit-suffixed forms (`10s`, `1min`, `500ms`) continue to work unchanged.
  - Side effect: `--rate-limit-window` now rejects zero (`0`, `0s`, `0ms`) with `Duration must be greater than 0`. Previously `0s` was accepted and would later panic in the rate-limiter's cleanup task because `tokio::time::interval` requires a non-zero duration. Other duration flags (`-t`, `--max-duration`, `discover --timeout`) still accept `0` for their existing meanings (`-t 0` is infinite duration).
- **Smoothed TUI jitter reading** (issue #48) — the UDP stats panel now shows jitter averaged over a 10-second rolling window rather than the raw per-second sample. The data pipeline is unchanged (samples still arrive every second from the server); only the aggregate display is smoothed. Per-stream jitter in the streams view continues to show the latest interval. While the test is running, the label shows `Jitter (10s):`; once completed, it reverts to `Jitter:` with the authoritative final value from the server's result.

### Fixed
- **Duplicate receive-error log on the server** (issue #54) — `tcp::receive_data` and `tcp::receive_data_half` each warned at the read-error site, and the caller then warned again when it saw the returned `Err`. The duplicate inner `warn!` is removed so receive errors now log exactly once, matching the send path's pattern. Reported by @matttbe.
- **Default to kernel TCP autotuning** (issue #60) — xfr no longer forces `SO_SNDBUF`/`SO_RCVBUF` to 4 MB on either side by default; both ends let the kernel autotune unless the user passes `-w`/`--window`. When set, the client's value propagates to the server over the control protocol so both sides apply the socket option symmetrically (matching iperf3). Reported by @matttbe.

  Caveats:
  - Loopback / intra-host benchmark numbers may decrease by roughly 10% — this is expected; the previous numbers were inflated by the oversized app-applied buffer.
  - On high-RTT paths, very short tests (e.g. `-t 1s` at high bitrate) may now show ramp-up-limited throughput in the final summary because kernel autotune takes a handful of RTTs to grow the window. Use a longer `-t`, or pass an explicit `-w` to skip autotune. Note that `-O`/`--omit` only hides the early intervals from output — the server-side final summary is still computed over the full test duration.
  - Explicit window sizes above `c_int::MAX` (≈2.1 GB on 64-bit) are now rejected with `InvalidInput` instead of silently wrapping before `setsockopt`.

### Removed
- **Library API**: pre-1.0 break — `TcpConfig::high_speed()` and `TcpConfig::with_auto_detect()` are gone; construct `TcpConfig` directly with the fields you want set. The `HIGH_SPEED_BUFFER` and `HIGH_SPEED_WINDOW_THRESHOLD` constants (which were private) are also removed. Downstream code that constructs `ControlMessage::TestStart`, `protocol::UdpStats`, or `tui::app::App` by name now needs to supply the new fields (`window_size`, `jitter_max_ms`, `packet_size`, `jitter_history`); these are additive and have sensible None/default values.

## [0.9.8] - 2026-04-17

### Added
- **Separate send/recv reporting in bidir tests** (issue #56) — `--bidir` now reports per-direction bytes and throughput in the summary instead of just the combined total, which was useless on asymmetric links. Plain text shows `Send: X  Recv: Y  (Total: Z)`; JSON adds `bytes_sent`, `bytes_received`, `throughput_send_mbps`, `throughput_recv_mbps`; CSV gets four new columns; TUI shows `↑ X / ↓ Y` in the throughput panel. Unidirectional tests are unchanged (the existing `bytes_total`/`throughput_mbps` is already the single-direction number).

### Fixed
- **Fast, accurate TCP teardown** (issue #54) — replaced the blocking `shutdown()` drain on the send path with `SO_LINGER=0` on Linux, so cancel and natural end-of-test no longer wait for bufferbloated send buffers to ACK through rate-limited paths. Fixes the "Timed out waiting 2s for N data streams to stop" warning matttbe reported with `-P 4 --mptcp -t 1sec`.
- **Sender-side byte-count accuracy** — `stats.bytes_sent` is now clamped to `tcpi_bytes_acked` before abortive close, removing a quiet ~5-10% overcount where the send-buffer tail discarded by RST was being reported as transferred. Download and bidir tests are the primary beneficiaries.
- **macOS preserves graceful shutdown** — non-Linux platforms lack `tcpi_bytes_acked`, so the Linux abortive-close path is cfg-gated; other platforms still use `shutdown()` for accurate accounting.

## [0.9.7] - 2026-04-16

### Added
- **Early exit summary** (issue #35) — Ctrl+C now displays a test summary with accumulated stats instead of silently exiting. Works in both plain text and TUI modes. Double Ctrl+C force-exits immediately.
- **DSCP server-side propagation** — `--dscp` flag is now sent to the server and applied to server-side TCP/UDP sockets for download and bidirectional tests. Previously only client-side sockets were marked.
- **Non-Unix `--dscp` warning** — platforms without socket TOS support now show a visible warning before the test starts, instead of silently no-oping.

### Fixed
- **Cancel flow waits for server result** — client `Cancelled` handler now waits for the server's `Result` message instead of immediately erroring, providing accurate final stats after cancel.
- **Server result ordering** — server sends `Result` before slow post-processing (push gateway, metrics hooks), preventing false cancel timeouts.
- **Rust 1.95 clippy compatibility** — fixed `manual_checked_ops` and `collapsible_match_arms` lints.

### Changed
- Bump `softprops/action-gh-release` from 2 to 3 in CI release workflow.

## [0.9.6] - 2026-03-18

### Added
- **`--dscp` flag** — set DSCP/TOS marking on TCP and UDP client sockets for QoS policy testing. Accepts numeric values (0-255) or standard DSCP names (EF, AF11-AF43, CS0-CS7). QUIC warns and ignores the flag; non-Unix platforms warn instead of applying socket marking.
- **`omit_secs` config support** (issue #43) — `[client] omit_secs = N` in config file sets default `--omit` value.

## [0.9.5] - 2026-03-17

### Added
- **TCP `--cport` support** (issue #44) — `--cport` now pins client-side TCP data-stream source ports. Multi-stream TCP uses sequential ports (`cport`, `cport+1`, ...), matching UDP behavior.

### Changed
- **TCP `--cport` semantics** — TCP control remains on an ephemeral source port while data streams use the requested source port or range. TCP data binds now match the remote address family the same way UDP/QUIC already do, so dual-stack clients can use `--cport` against IPv6 targets.

## [0.9.4] - 2026-03-11

### Added
- **`--no-mdns` flag** (issue #41) — `xfr serve --no-mdns` disables mDNS service registration for environments where multicast is unwanted or another service already uses mDNS.
- **`server.no_mdns` config support** — mDNS registration can now also be disabled from `~/.config/xfr/config.toml` via `[server] no_mdns = true`.

### Changed
- **Delta retransmits in interval reports** (issue #36) — plain text interval lines now show per-interval retransmit deltas instead of cumulative totals, making it easier to spot when retransmits actually occur. Hidden intervals from `--omit`, `--quiet`, or larger `--interval` settings no longer get folded into the next visible `rtx:` value. Final summary still shows cumulative totals.

## [0.9.3] - 2026-03-10

### Added
- **Server `--bind` flag** (issue #38) — `xfr serve --bind <IP>` binds TCP, QUIC, and UDP data listeners to a specific address. Validates against `-4`/`-6` flags and rejects unspecified addresses (`::`, `0.0.0.0`). Requested by Windows users needing interface-specific binding.

### Changed
- **Server sends random payloads** (issue #34) — server-side TCP and UDP send paths now use random bytes by default in reverse and bidirectional modes, matching the client's default-on behavior. `--zeros` only affects client-sent traffic; server payload mode is not negotiated over the wire (future enhancement).

### Fixed
- **QUIC dual-stack on Windows** (issue #39) — QUIC server endpoint now creates its UDP socket via socket2 with explicit `IPV6_V6ONLY` handling instead of relying on Quinn's `Endpoint::server()`, which uses `std::net::UdpSocket::bind()` without dual-stack configuration. On Windows/macOS where `IPV6_V6ONLY` defaults to `true`, binding to `[::]` would only accept IPv6 connections.
- **Server random payload on single-port TCP reverse** (issue #34) — the single-port TCP handler (DataHello path used by all modern clients) was missing `random_payload = true`, causing reverse-mode downloads to still send zeros. Legacy multi-port handler was correct but is dead code for current clients.

### Security
- **quinn-proto DoS fix** — updated quinn-proto 0.11.13 → 0.11.14 (RUSTSEC-2026-0037, severity 8.7)

## [0.9.2] - 2026-03-06

### Changed
- **Random payloads by default** (issue #34) — TCP/UDP payloads now use random bytes by default to avoid silently inflated results on WAN-optimized or compressing paths. `--random` remains as an explicit no-op for clarity, and new `--zeros` forces zero-filled payloads for compression/dedup testing.

### Fixed
- **Windows build regression** (issue #37) — `pacing_rate_bytes_per_sec()` used `libc::c_ulong` without a `#[cfg(target_os = "linux")]` guard, breaking compilation on Windows. The function is only called from the linux-gated `SO_MAX_PACING_RATE` path.
- **MPTCP namespace test realism** (issue #32) — `test-mptcp-ns.sh` now combines `netem` shaping with `fq_codel` on the shaped transit links, matching common Linux defaults more closely and reducing false-positive high-stream failures caused by shallow unfair queues in the test harness.

## [0.9.1] - 2026-03-05

### Added
- **MPTCP support** (`--mptcp`) - Multi-Path TCP on Linux 5.6+ (issue #24). Uses `IPPROTO_MPTCP` at socket creation via socket2 — all TCP features (nodelay, congestion control, window size, bidir, multi-stream, single-port mode) work transparently. The server automatically creates MPTCP listeners when available (no flag needed) — MPTCP listeners accept both MPTCP and regular TCP clients transparently, with silent fallback to TCP if the kernel lacks MPTCP support. Client uses `--mptcp` to opt in. Clear error message on non-Linux clients or kernels without `CONFIG_MPTCP=y`.
- **Kernel TCP pacing via `SO_MAX_PACING_RATE`** (issue #30) - On Linux, TCP bitrate pacing (`-b`) now uses the kernel's FQ scheduler with EDT (Earliest Departure Time) for precise per-packet timing, eliminating burst behavior from userspace sleep/wake cycles. Falls back to userspace pacing on non-Linux, MPTCP sockets (not yet supported in kernel, see [mptcp_net-next#578](https://github.com/multipath-tcp/mptcp_net-next/issues/578)), or if the setsockopt fails. Note: `-b` sets a global bitrate shared across all parallel streams (unlike iperf3 where `-b` is per-stream). Suggested by the kernel MPTCP maintainer.
- **Random payload mode** (`--random`, issue #34) — client can fill TCP/UDP send buffers with random bytes (once at allocation) to reduce compression/dedup artifacts on shaped/WAN links. Current scope is client-sent payloads only: reverse mode sender remains server-side zeros until protocol negotiation is added.

### Changed
- **Library API** — `create_tcp_listener()`, `connect_tcp()`, and `connect_tcp_with_bind()` now take a `mptcp: bool` parameter. Library consumers should pass `false` to preserve existing behavior.

### Fixed
- **High stream-count TCP robustness** (issues #25, #32) — client now stops local data streams at local duration expiry instead of waiting for server `Result`, scales stream join timeout with stream count (`max(2s, streams*50ms)`), and TCP receivers drain briefly after cancel to reduce reset-on-close bursts. For single-port TCP setup, client also limits concurrent `connect + DataHello` handshakes (max 16 in flight) and server initial first-line read timeout is now adaptive to active stream counts (capped at 20s), reducing mid-test handshake-loss failures on constrained links.
- **Best-effort send shutdown** — `send_data()` shutdown no longer propagates errors during normal teardown races, matching `send_data_half()` behavior.
- **Kernel pacing rate width** — `SO_MAX_PACING_RATE` now uses native `c_ulong` instead of `u32`, removing an unintended ~34 Gbps ceiling on 64-bit Linux.
- **JoinHandle panic with many parallel streams** (issue #24) — removed second `join_all` after aborting timed-out stream tasks, which polled already-completed handles
- **Final summary showing 0 retransmits/RTT/cwnd** (issue #26) — each stream task now captures a final sender-side TCP_INFO snapshot before the socket closes; the Result handler overlays these saved snapshots deterministically instead of racing live fd polls
- **Broken pipe / connection reset at teardown** (issue #25) — client now joins stream task handles with timeout before returning, preventing writes to already-closed sockets
- **MPTCP label in server log** — server now displays "MPTCP" instead of "TCP" in the test info log when client uses `--mptcp`; adds backward-compatible `mptcp` field to TestStart control message

## [0.8.0] - 2026-02-12

### Added
- **Client source port pinning** (`--cport`) - pin the client's local port for firewall traversal (issue #16). Works with UDP and QUIC. Multi-stream UDP (`-P N`) assigns sequential ports starting from the specified port (e.g., `--cport 5300 -P 4` uses ports 5300-5303). QUIC multiplexes all streams on the single specified port. TCP rejects `--cport` since single-port mode already handles firewall traversal. Combines with `--bind` for full control (`--bind 10.0.0.1 --cport 5300`). Automatically matches the remote's address family so `--cport` works transparently with both IPv4 and IPv6 targets.

### Fixed
- **`--bind` with IPv6 targets** — `--bind` with an unspecified IP (e.g., `0.0.0.0:0`) now auto-matches the remote's address family at socket creation time across TCP, UDP, and QUIC. Previously failed when connecting to IPv6 targets from dual-stack clients.
- **UDP data_ports length validation** — server returning mismatched port count could panic on `stats.streams[i]`; now validates length before iterating, matching the existing TCP guard

## [0.7.1] - 2026-02-12

### Fixed
- **Server TUI `-0.0 Mbps` after test ends** (issue #20) - IEEE 754 negative zero now normalized via precision-aware `normalize_for_display()` helper across all throughput display paths
- **TCP RTT/retransmits not updating live** (issue #13) - per-interval retransmits now computed from TCP_INFO deltas instead of a dead atomic counter; client stores socket fds for local TCP_INFO polling so sender-side metrics (upload/bidir) update live; download mode correctly uses server-reported metrics
- **Plain-text zero retransmits dropped** - `rtx: 0` was omitted in plain/JSON/CSV interval output when all streams reported zero retransmits; now preserved
- **`mbps_to_human()` unit-switch boundary** - `999.95 Mbps` displayed as `1000.0 Mbps` instead of `1.00 Gbps`; unit branch now uses rounded value

### Changed
- **Consolidated throughput formatting** - server TUI now uses shared `mbps_to_human()` instead of inline formatting; Gbps display changes from 1 to 2 decimal places for consistency

## [0.7.0] - 2026-02-11

### Added
- **Real pause/resume** (`p` key) - pressing `p` now pauses actual data transfer, not just the TUI display. Uses `Pause`/`Resume` protocol messages and a dedicated `watch` channel to stop/resume data loops across TCP, UDP, and QUIC. Capability-gated via `pause_resume` in Hello messages: older servers without support fall back to display-only pause. TCP bitrate pacing resets its baseline on resume to prevent catch-up bursts. UDP receiver resets its inactivity timer during pause to prevent false timeouts. Resolves issue #19.

## [0.6.1] - 2026-02-10

### Added
- **TCP bitrate pacing** (`-b` for TCP) - `-b` flag now works for TCP, not just UDP. Uses byte-budget sleep pacing with interruptible sleeps for responsive cancellation. Buffer size auto-caps to prevent first-write burst at low bitrates. Resolves issue #14.

## [0.6.0] - 2026-02-06

### Added
- **Congestion control selection** (`--congestion`) - Select TCP congestion control algorithm (e.g. cubic, bbr, reno). Applied on both client and server sockets. Useful for BBR vs CUBIC comparison on WAN/cloud links.
- **Live TCP_INFO polling** - RTT, cwnd now reported per interval during tests, not just in final result. Enables real-time TCP metric monitoring in TUI, plain text (`rtt: X.XXms`), JSON streaming, and CSV output. Essential for `-t 0` infinite tests where results are never finalized (issue #13).

### Fixed
- **Congestion config errors surfaced** - Invalid `--congestion` algorithm is now validated before the test starts; client exits non-zero immediately and server sends an error message back to the client
- **TCP_INFO stale fd cleared** - Stream handlers now clear the stored file descriptor on completion and on early-return error paths, preventing `poll_tcp_info()` from reading an unrelated socket if the OS reuses the fd
- **Update banner double "v" prefix** - Version display now strips leading `v` from update-informer output to avoid showing `vv0.5.0`
- **PSK unwrap panics** - Server no longer panics if PSK is misconfigured during auth; returns error instead
- **UDP encode bounds check** - `UdpPacketHeader::encode()` now validates buffer length before writing
- **Timestamp clock skew** - ISO8601/Unix timestamps now derived from monotonic elapsed time instead of calling `SystemTime::now()`

### Code Quality
- **Named constants** - Replaced 12 hardcoded magic numbers in serve.rs with 6 named constants (STATS_INTERVAL, CANCEL_CHECK_TIMEOUT, RESULT_FLUSH_DELAY, STREAM_ACCEPT_TIMEOUT, STREAM_COLLECTION_TIMEOUT, DEFAULT_BITRATE_BPS)

## [0.5.0] - 2026-02-05

### Changed
- **Single-port TCP mode** (issue #16) - TCP tests now use only port 5201 for all connections, making them firewall-friendly. Data connections identify themselves via `DataHello` message instead of using ephemeral ports.
- **Protocol version bump to 1.1** - Signals DataHello support; adds `single_port_tcp` capability for backward compatibility detection
- **Client capabilities in Hello** - Client now advertises supported capabilities (tcp, udp, quic, multistream, single_port_tcp); server falls back to multi-port TCP for legacy clients without `single_port_tcp`
- **Numeric version comparison** - `versions_compatible()` now parses major version as integer instead of string comparison

### Fixed
- **QUIC IPv6 support** (issue #17) - QUIC clients can now connect to IPv6 addresses without requiring `-6` flag; endpoint now binds to matching address family
- **mDNS discovery** (issue #15) - Server now advertises addresses via `enable_addr_auto()`; client uses non-blocking receive with proper timeout handling
- **TCP RTT and retransmits display** (issue #13) - TUI now shows correct retransmit count from stream results (captured after transfer); TCP_INFO captured after transfer for accurate RTT/cwnd
- **Data connections no longer consume rate-limit/semaphore slots** - Only control (Hello) connections acquire permits; DataHello connections route directly without resource consumption
- **Cancel messages processed during TCP stream collection** - Interval loop now starts immediately; stream collection runs concurrently in background
- **Client OOB panic on port mismatch** - Added bounds check when server returns fewer ports than requested streams
- **DoS guard on oversized lines** - `read_first_line_unbuffered()` now returns error instead of truncating
- **DataHello serialization panic** - Replaced `unwrap()` with proper error handling in spawned task
- **One-off mode deadlock** - `--one-off` no longer blocks the accept loop waiting for test completion; uses shutdown channel to signal exit after test finishes
- **QUIC one-off mode** - QUIC accept loop now responds to shutdown signal for proper `--one-off` exit
- **cancel.changed() busy-loop** - Handle sender-dropped error in stream collection select! to prevent CPU spin
- **IPv4-mapped IPv6 comparison** - DataHello IP validation now normalizes `::ffff:x.x.x.x` addresses for correct matching on dual-stack systems

### Security
- **DataHello IP validation** - Server validates DataHello connections come from same IP as control connection to prevent connection hijacking
- **Slow-loris protection** - Accept loop now spawns per-connection tasks with 5-second initial read timeout; slow clients can no longer block the listener
- **DataHello flood protection** - Server validates test_id exists in active_tests before processing DataHello connections; unknown test_ids are dropped immediately
- **Pre-handshake connection gate** - Limits concurrent unclassified connections (4x max_concurrent) to prevent connection-flood DoS before Hello/DataHello routing
- **Multi-port TCP fallback IP validation** - Per-stream listeners validate connecting peer IP against control connection, preventing unauthorized data stream injection
- **One-off mode hardened** - Failed handshakes and auth failures no longer trigger server shutdown in `--one-off` mode; only successful test completion exits

### Testing
- Added regression test for QUIC IPv6 connectivity
- Added `test_tcp_one_off_multi_stream` - verifies 4-stream TCP in `--one-off` mode with stream count assertion
- Added `test_quic_one_off` - verifies 2-stream QUIC in `--one-off` mode with stream count assertion

### Code Quality
- Log panics from `join_all` in QUIC, UDP, and TCP stream handlers instead of silently discarding JoinErrors
- Multi-port fallback listener tasks cleaned up via cancel signal (no leaked tasks on partial connections)

## [0.4.4] - 2026-02-04

### Changed
- **Lower default max_concurrent** - reduced from 1000 to 100 for safer resource defaults

### Fixed
- **Settings modal text truncation** - increased modal width to prevent help text from being cut off
- **UDP session cleanup on client abort** (issue #12) - server now detects inactive UDP sessions after 30 seconds and cleans them up properly
- **UTF-8 handling in protocol parser** - use lossy conversion to handle partial UTF-8 sequences at buffer boundaries
- **Rate limiter cleanup on panic** - use RAII guard to ensure slot release even if task panics
- **PSK length validation** - reject PSKs over 1024 bytes and empty PSKs to prevent abuse
- **UDP per-stream bitrate underflow** - clamp to minimum 1 bps when total bitrate > 0 to prevent unlimited mode
- **QUIC stream accept timeout** - add 30-second timeout and cancellation support to prevent infinite hangs
- **active_tests cleanup order** - cleanup before result write to prevent stale entries on connection failure

### Testing
- Added regression test for UDP bitrate underflow bug

### Code Quality
- Added SAFETY comments to all 4 unsafe blocks (tcp_info.rs, tcp.rs, net.rs)

### Documentation
- Added server memory footprint guide to README
- Clarified Windows support is experimental (WSL2 recommended)
- Emphasized PSK requirement for QUIC on untrusted networks
- Fixed buffer size documentation (256KB → 128KB)
- Fixed UDP bitrate default documentation (1 Gbps, not unlimited; use `-b 0` for unlimited)
- Documented that data ports are unauthenticated by design
- Updated KNOWN_ISSUES with QUIC cert verification, protocol versioning, Windows limitations
- Added KNOWN_ISSUES entries for UDP data plane spoofing risk and IPv6 zone ID limitation
- Added pre-1.0 roadmap items (structured errors, code refactoring, fuzz testing)
- Expanded roadmap with security enhancements, testing, and optimization items

## [0.4.3] - 2026-02-04

### Added
- **In-app update notifications** - checks GitHub releases in background, shows banner with update command
  - Detects install method (Homebrew, Cargo, binary) and shows appropriate update command
  - Press `u` to dismiss the banner
  - Uses 1-hour cache to avoid GitHub API rate limits
- **Android/Termux support** - pre-built `aarch64-linux-android` binary in releases
- **Infinite duration mode** (`-t 0`) - run test indefinitely until manually stopped with `q` or Ctrl+C
- **Local bind address** (`--bind`) - bind to specific local IP or IP:port for multi-homed hosts
- **Theme hint in footer** - `[t] Theme` now shown in TUI status bar

### Fixed
- **UDP cross-platform compatibility** (issue #10) - UDP mode now works between Linux server and macOS client
  - Client UDP socket now matches server's address family instead of using dual-stack
  - macOS handles dual-stack sockets differently than Linux; this fix ensures compatibility
- **`--bind` with explicit port** - disallows explicit port for TCP (control + data connections would conflict)
- **Non-blocking connect** - added EWOULDBLOCK to Unix error handling for edge cases

## [0.4.2] - 2026-02-03

### Changed
- Removed Intel Mac (x86_64-apple-darwin) pre-built binary - use `cargo install xfr`

### Fixed
- UDP download/bidir race condition: client now retries hello packets

## [0.4.1] - 2026-02-03

### Added
- `-Q` short flag for `--quic` mode (uppercase to distinguish from `-q` quiet)
- Security documentation section in README
- 30-second timeout for control-plane handshakes (prevents DoS from idle connections)
- KNOWN_ISSUES.md documenting edge cases and limitations
- "quic" capability in server Hello message

### Changed
- `-b/--bitrate` help text clarifies it only applies to UDP
- Log messages now go to stderr instead of stdout (allows clean JSON/CSV piping)
- Minimum test duration enforced at 1 second
- Protocol documentation corrected (newline-delimited JSON, not length-prefixed)

### Fixed
- UDP reverse mode (`-u -R`) now works - server learns client address before sending
- UDP bidirectional mode on server now waits for client before sending
- Division by zero guard in throughput calculations for zero-duration tests
- Overflow protection in bitrate/size parsing (checked_mul instead of unchecked)
- Conflicting CLI flags now produce clear errors (`--quic --udp`, `--bidir --reverse`, `--json --csv`)
- Parallel streams validated to 1-128 range (prevents `-P 0` crash)
- Documentation: corrected JSON field names in SCRIPTING.md (bytes_total, duration_ms)
- Documentation: fixed Prometheus flag name in FEATURES.md (--prometheus not --prometheus-port)
- QUIC bitrate warning now logged like TCP when -b flag is ignored

## [0.4.0] - 2026-02-02

### Added
- **Per-stream detail view** (`d` key) - toggle between History and Streams panels for multi-stream tests
  - Shows per-stream throughput bars with retransmit counts
  - Uses existing StreamBar widget for consistent visualization
- **Pause overlay** - prominent "PAUSED" overlay when test is paused (not just footer text)
- **History event logging** - automatically logs significant events:
  - Peak throughput moments (10%+ above previous peak)
  - Retransmit spikes (10+ in an interval)
  - UDP packet loss events (5+ packets lost)
- **Settings modal** (`s` key) - adjust display and test settings on the fly:
  - Display tab: Theme, Timestamp format, Units (auto-persist)
  - Test tab: Streams, Protocol, Duration, Direction (session-only)
  - Vim-style navigation (j/k/h/l) and arrow keys supported
  - Tab key switches between categories
- **QUIC transport** (`--quic`) via quinn crate - built-in TLS 1.3 encryption, stream multiplexing
- **TUI visual overhaul** - cleaner design inspired by ttl:
  - Styled title bar and footer with status display
  - Completion results shown as centered modal overlay
  - Color-coded metrics (retransmits, loss, jitter, RTT)
  - Simplified layout with single outer border
- **Documentation overhaul:**
  - `docs/COMPARISON.md` - Feature matrix vs iperf3/iperf2/rperf/nperf, migration guide
  - `docs/SCRIPTING.md` - CI/CD examples, Docker usage, Prometheus integration
  - `docs/FEATURES.md` - Comprehensive feature reference
  - `docs/ARCHITECTURE.md` - Module structure, data flow, threading model
  - Real-world use cases section in README
  - Enhanced module-level rustdoc for lib.rs, protocol.rs, quic.rs
- `install.sh` - Cross-platform installer script (Linux/macOS)
- `.github/FUNDING.yml` - Ko-fi sponsor link
- QUIC supports upload, download, bidirectional, and multi-stream modes
- QUIC works with PSK authentication

### Removed
- TLS over TCP (`--tls`, `--tls-cert`, `--tls-key`, `--tls-ca`) - replaced by QUIC which provides built-in encryption

### Fixed
- Use VecDeque for interval history to avoid O(n) removal on every interval
- Report per-interval retransmit deltas instead of cumulative totals
- Suppress console logging in TUI mode to prevent log messages corrupting display
- Help modal now opens after test completion (was blocked by key handler)
- Settings modal button cycling when Apply is hidden (only Close visible)
- Rate limiter atomic underflow race condition (use fetch_update instead of fetch_sub)
- Progress bar width now adaptive to terminal size (removed arbitrary 40-char max)

### Security
- Add semaphore to limit concurrent handlers (defense against connection floods on public servers)

### Changed
- Use constant for QUIC buffer size (consistency with TCP module)

### Dependencies
- hyper-util 0.1.19 → 0.1.20
- slab 0.4.11 → 0.4.12
- unicode-width 0.2.0 → 0.2.2
- zmij 1.0.18 → 1.0.19

### Testing
- Add integration tests: UDP bidir, UDP multi-stream, QUIC bidir, ACL deny, IPv6 localhost
- Add protocol parsing benchmarks (serialize/deserialize for Hello, TestStart, Interval, Result)

## [0.3.0] - 2026-01-31

### Added
- **Server TUI dashboard** (`xfr serve --tui`):
  - Real-time display of active tests, bandwidth, and client information
  - Shows blocked connections and authentication failures
  - Uptime counter and total test statistics
  - Help overlay with `?` key
- Timestamp format options (`--timestamp-format`) - relative, iso8601, or unix epoch
- Prometheus Push Gateway support (`--push-gateway`) for pushing metrics at test completion
- File logging with daily rotation (`--log-file`, `--log-level`)
- Integration tests for TCP, UDP, download, bidir, and multi-client modes
- **Security & Enterprise features:**
  - Pre-shared key (PSK) authentication (`--psk`, `--psk-file`)
  - Per-IP rate limiting (`--rate-limit`, `--rate-limit-window`)
  - IP access control lists (`--allow`, `--deny`, `--acl-file`)
- **TUI Improvements:**
  - 11 color themes: default, kawaii, cyber, dracula, monochrome, matrix, nord, gruvbox, catppuccin, tokyo_night, solarized
  - Theme selection via `--theme` flag or config file
  - Press `t` to cycle themes during TUI session
  - UDP-specific stats display (jitter, packet loss %) in TUI
  - Target bitrate shown in header for UDP mode
- **Preferences persistence** (`~/.config/xfr/prefs.toml`)
  - Auto-saves last used theme
  - Remembers user preferences across sessions
- **Full IPv6 support:**
  - `-4`/`--ipv4` and `-6`/`--ipv6` flags for address family selection
  - Dual-stack mode (default): server accepts both IPv4 and IPv6 clients
  - Proper `IPV6_V6ONLY` socket option handling via socket2
  - ACL normalizes IPv4-mapped IPv6 addresses for consistent rule matching
- CSV output format (`--csv`)
- JSON streaming output (`--json-stream`) for real-time per-interval JSON
- Quiet mode (`-q/--quiet`) to suppress interval output
- Custom report interval (`-i/--interval`)
- Omit option (`--omit`) to skip initial TCP ramp-up seconds
- Server max duration (`--max-duration`) for server-side test limits
- Environment variable support: `XFR_PORT` and `XFR_DURATION`
- mDNS service registration on server start for discovery
- send_data_half/receive_data_half for split socket operations

### Fixed
- Stats now shared correctly between handlers and TestStats (real-time intervals)
- Bidirectional mode properly splits sockets for concurrent send/receive
- UDP receive uses recv_from for unconnected sockets
- Server signals cancel when test duration elapses
- Client control loop has 30s timeout to prevent hangs
- Dynamic port allocation prevents multi-client port collisions
- Hostname parsing provides proper error messages
- Interval history bounded to 60 entries to prevent memory growth
- Client cancel() method now functional and sends Cancel to server
- Protocol version checking uses proper comparison function
- Hostname resolution uses resolved IP from control connection for data streams
- TUI stream throughput uses correct 1-second interval calculation
- CSV header columns now match data format
- Socket buffer tuning logs failures at debug level

### Security
- Bounded control channel read to prevent memory DoS (max 8KB lines)
- Validate stream count against MAX_STREAMS (128)
- Enforce MAX_TEST_DURATION (1 hour) and server max_duration
- Add 10s timeout on TCP data connection accept

### Changed
- Replaced emoji indicators with ASCII [OK]/[WARN]/[FAIL] for terminal compatibility
- `BackendType::from_str` and `AddressFamily::from_str` now implement `std::str::FromStr` trait

### Removed
- Unused dependencies: thiserror, bytesize, rand

### Dependencies
- ratatui 0.29 → 0.30 (fixes lru and paste warnings)
- crossterm 0.28 → 0.29
- prometheus 0.13 → 0.14 (fixes protobuf vulnerability RUSTSEC-2024-0437)
- mdns-sd 0.11 → 0.17
- toml 0.8 → 0.9
- dirs 5 → 6
- webpki-roots 0.26 → 1.0
- rand 0.8 → 0.9
- ipnetwork 0.20 → 0.21
- criterion 0.5 → 0.6

## [0.2.0] - 2026-01-31

### Added
- Full Prometheus metrics with per-stream and TCP stats
- Config file support (`~/.config/xfr/config.toml`)
- Server presets for bandwidth limits and client restrictions
- Grafana dashboard template (`examples/grafana-dashboard.json`)
- Man page (`doc/xfr.1`)
- CONTRIBUTING.md guide

### Changed
- Prometheus metrics now properly registered and updated
- CLI arguments can be overridden by config file defaults

## [0.1.2] - 2026-01-31

### Added
- Dual MIT/Apache-2.0 licensing
- README documentation

### Fixed
- License attribution

## [0.1.1] - 2026-01-31 [YANKED]

### Added
- Initial README

## [0.1.0] - 2026-01-31 [YANKED]

### Added
- Initial release
- TCP bandwidth testing with configurable streams
- UDP mode with bitrate limiting and jitter calculation
- Live TUI with real-time throughput graphs
- Multi-client server support
- Reverse and bidirectional testing modes
- JSON output format
- Plain text output format
- Prometheus metrics export (optional feature)
- mDNS LAN discovery (optional feature)
- `xfr diff` command to compare test results
- TCP_INFO stats on Linux and macOS
- Configurable TCP window size and nodelay

[0.9.2]: https://github.com/lance0/xfr/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/lance0/xfr/compare/v0.8.0...v0.9.1
[0.8.0]: https://github.com/lance0/xfr/compare/v0.7.1...v0.8.0
[0.7.1]: https://github.com/lance0/xfr/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/lance0/xfr/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/lance0/xfr/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/lance0/xfr/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/lance0/xfr/compare/v0.4.4...v0.5.0
[0.4.4]: https://github.com/lance0/xfr/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/lance0/xfr/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/lance0/xfr/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/lance0/xfr/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/lance0/xfr/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/lance0/xfr/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/lance0/xfr/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/lance0/xfr/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/lance0/xfr/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/lance0/xfr/releases/tag/v0.1.0
