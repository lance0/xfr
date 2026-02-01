# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
  - TLS encryption for control channel (`--tls`, `--tls-cert`, `--tls-key`, `--tls-ca`)
  - Per-IP rate limiting (`--rate-limit`, `--rate-limit-window`)
  - IP access control lists (`--allow`, `--deny`, `--acl-file`)
  - Audit logging with JSON/text formats (`--audit-log`, `--audit-format`)
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
- **io_uring backend infrastructure** (`--features io-uring`):
  - DataBackend trait abstraction for I/O operations
  - TokioBackend (default) using epoll/kqueue
  - UringBackend stub for Linux 5.10+ (falls back to tokio currently)
  - Auto-detection of best available backend
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

[Unreleased]: https://github.com/lance0/xfr/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/lance0/xfr/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/lance0/xfr/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/lance0/xfr/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/lance0/xfr/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/lance0/xfr/releases/tag/v0.1.0
