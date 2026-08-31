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

3. Install [just](https://just.systems/) once:
   ```bash
   cargo install --locked just
   ```

4. Build:
   ```bash
   just build
   ```

5. Run the standard local checks before opening a pull request:
   ```bash
   just check
   ```

## Code Style

- Run `just fmt` before committing
- Run `just lint` and fix any warnings
- Keep lines under 100 characters when possible

### Pre-commit hooks

We ship a `.pre-commit-config.yaml` that runs `cargo fmt` and `cargo clippy`
on every commit and the all-features test suite before each push. Set up both
hook stages once:

```bash
# Recommended: prek (fast Rust port, drop-in compatible)
cargo install --locked prek

# Or via standalone installer (no Rust toolchain needed)
curl -LsSf https://github.com/j178/prek/releases/latest/download/prek-installer.sh | sh

# Install both hook stages configured by the repository
just install-hooks

# Or with the original Python pre-commit
pipx install pre-commit
pre-commit install --hook-type pre-commit --hook-type pre-push
```

After installation, the hooks run automatically. Use `just check` for the
standard local check before opening a pull request.

## Testing

```bash
# Run formatting, lint, test feature matrices, and rustdoc checks
just check

# Run a focused test
cargo test --locked test_name

# Run benchmarks
just bench

# Linux-only network namespace tests (require root)
just netns
```

## Pull Request Process

1. Fork the repository
2. Create a feature branch from `master`
3. Make your changes
4. Run `just check`
5. Ensure any relevant benchmarks or network namespace tests pass
6. Submit a pull request

## Feature Flags

| Flag | Description |
|------|-------------|
| `prometheus` | Enable Prometheus metrics endpoint |
| `discovery` | Enable mDNS LAN discovery (default) |

## Architecture Overview

See [Architecture](docs/ARCHITECTURE.md) for the maintained overview of the
protocol, data paths, and source layout.

## Adding Features

1. **New output format**: Add to `src/output/` and update `src/output/mod.rs`
2. **New protocol message**: Add to `ControlMessage` enum in `src/protocol.rs`
3. **New CLI flag**: Add to `Cli` struct in `src/main.rs`
4. **New config option**: Add to `src/config.rs` and update `examples/config.toml`

## Questions?

Open an issue on GitHub: https://github.com/lance0/xfr/issues
