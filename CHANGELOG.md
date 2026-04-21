# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Max jitter and packet size in UDP summary** (issue #48 follow-up) — the final UDP summary now reports `Jitter Max` (peak of the RFC 3550 running estimate across the test) alongside the average, and `Packet Size` (UDP payload bytes). Surfaced in plain text and JSON. Requested by brettowe for NFS UDP packet-size tuning context.

### Changed
- **Smoothed TUI jitter reading** (issue #48) — the UDP stats panel now shows jitter averaged over a 10-second rolling window rather than the raw per-second sample. The data pipeline is unchanged (samples still arrive every second from the server); only the aggregate display is smoothed. Per-stream jitter in the streams view continues to show the latest interval. While the test is running, the label shows `Jitter (10s):`; once completed, it reverts to `Jitter:` with the authoritative final value from the server's result.

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
