# xfr Roadmap

## Completed (v0.1.0)

### Core Features
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

## v0.2 - Polish & Platform Support

### Windows Support
- [ ] Basic TCP/UDP testing (no TCP_INFO)
- [ ] TUI compatibility with Windows Terminal
- [ ] Pre-built binaries for Windows

### Prometheus Metrics
- [ ] Full `/metrics` endpoint implementation
- [ ] Grafana dashboard template
- [ ] Push gateway support

### Configuration
- [ ] Config file support (`~/.config/xfr/config.toml`)
- [ ] Server presets (bandwidth limits, allowed clients)
- [ ] Environment variable overrides

### Documentation
- [ ] man page
- [ ] README with examples and screenshots
- [ ] Contributing guide

## v0.3 - Security & Enterprise

### Authentication
- [ ] Pre-shared key authentication
- [ ] Optional TLS for control channel
- [ ] Rate limiting to prevent abuse

### Enterprise Features
- [ ] Server access control lists
- [ ] Bandwidth quotas per client
- [ ] Audit logging

## v0.4 - Advanced Protocols

### QUIC Support
- [ ] QUIC transport via `quinn` crate
- [ ] 0-RTT connection establishment
- [ ] Stream multiplexing tests

### IPv6
- [ ] Full IPv6 support
- [ ] Dual-stack testing
- [ ] IPv6-only mode

## Future Ideas

- **Web UI**: Browser-based dashboard for long-running servers
- **Scheduled tests**: Cron-like recurring bandwidth checks
- **Historical data**: SQLite storage for trend analysis
- **Cloud integration**: AWS/GCP/Azure endpoint discovery
- **Mobile apps**: iOS/Android clients for field testing

## Testing Improvements

- [ ] Performance regression CI (ensure 10G+ capability)
- [ ] Cross-platform CI (Linux, macOS, Windows)
- [ ] Long-duration stability tests (1+ hour)
- [ ] High packet loss scenarios (UDP)
- [ ] Real hardware testing (10G NICs)

## Contributing

See issues labeled `good first issue` for entry points. PRs welcome for any roadmap item.
