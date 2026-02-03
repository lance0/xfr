# Contributing to xfr

## Development Setup

1. Install Rust (1.88+):
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. Clone the repository:
   ```bash
   git clone https://github.com/lance0/xfr.git
   cd xfr
   ```

3. Build:
   ```bash
   cargo build
   ```

4. Run tests:
   ```bash
   cargo test --all-features
   ```

## Code Style

- Run `cargo fmt` before committing
- Run `cargo clippy --all-features` and fix any warnings
- Keep lines under 100 characters when possible

## Testing

```bash
# Run all tests
cargo test --all-features

# Run with specific feature
cargo test --features prometheus

# Run integration tests only
cargo test --test integration
```

## Pull Request Process

1. Fork the repository
2. Create a feature branch from `master`
3. Make your changes
4. Run `cargo fmt` and `cargo clippy --all-features`
5. Ensure all tests pass
6. Submit a pull request

## Feature Flags

| Flag | Description |
|------|-------------|
| `prometheus` | Enable Prometheus metrics endpoint |
| `discovery` | Enable mDNS LAN discovery (default) |

## Architecture Overview

```
src/
├── main.rs          # CLI entry point
├── lib.rs           # Library exports
├── client.rs        # Client implementation
├── serve.rs         # Server implementation
├── protocol.rs      # Control protocol messages
├── tcp.rs           # TCP data transfer
├── udp.rs           # UDP pacing and jitter
├── stats.rs         # Test statistics
├── config.rs        # Config file loading
├── diff.rs          # Result comparison
├── discover.rs      # mDNS discovery
├── tcp_info.rs      # Platform TCP_INFO
├── tui/             # Terminal UI
│   ├── app.rs       # App state
│   ├── ui.rs        # Drawing
│   └── widgets.rs   # Custom widgets
└── output/          # Output formats
    ├── plain.rs     # Plain text
    ├── json.rs      # JSON
    └── prometheus.rs # Prometheus metrics
```

## Adding Features

1. **New output format**: Add to `src/output/` and update `src/output/mod.rs`
2. **New protocol message**: Add to `ControlMessage` enum in `src/protocol.rs`
3. **New CLI flag**: Add to `Cli` struct in `src/main.rs`
4. **New config option**: Add to `src/config.rs` and update `examples/config.toml`

## Questions?

Open an issue on GitHub: https://github.com/lance0/xfr/issues
