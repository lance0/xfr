# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed
- **Settings modal text truncation** - increased modal width to prevent help text from being cut off
- **UDP session cleanup on client abort** (issue #12) - server now detects inactive UDP sessions after 30 seconds and cleans them up properly

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

[Unreleased]: https://github.com/lance0/xfr/compare/v0.4.2...HEAD
[0.4.2]: https://github.com/lance0/xfr/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/lance0/xfr/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/lance0/xfr/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/lance0/xfr/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/lance0/xfr/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/lance0/xfr/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/lance0/xfr/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/lance0/xfr/releases/tag/v0.1.0
