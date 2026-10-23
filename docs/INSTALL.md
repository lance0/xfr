# Installing xfr

[Documentation](README.md) · [Features](FEATURES.md) · [Known issues](../KNOWN_ISSUES.md)

## Installation channels

### From crates.io (Recommended)

Requires [Rust 1.88+](https://www.rust-lang.org/tools/install):

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install xfr
cargo install xfr
```

### Homebrew (macOS/Linux)

Available directly from [Homebrew core](https://formulae.brew.sh/formula/xfr):

```bash
brew install xfr
```

The [lance0/tap](https://github.com/lance0/homebrew-tap) tap remains available
for pre-built release binaries and may receive new releases before the core
formula updates:

```bash
brew install lance0/tap/xfr
```

### Docker (GHCR)

Multi-arch image (amd64/arm64), handy for running a server:

```bash
docker run --rm -p 5201:5201 -p 5201:5201/udp ghcr.io/lance0/xfr:latest serve
```

### Pre-built Binaries

Download from [GitHub Releases](https://github.com/lance0/xfr/releases):

| Platform | Target |
|----------|--------|
| Linux x86_64 | `xfr-x86_64-unknown-linux-musl.tar.gz` |
| Linux ARM64 | `xfr-aarch64-unknown-linux-gnu.tar.gz` (or `-musl` for fully static) |
| macOS Apple Silicon | `xfr-aarch64-apple-darwin.tar.gz` |
| macOS Intel | Use `cargo install xfr` |
| Android (Termux) | `xfr-aarch64-linux-android.tar.gz` |
| Windows | Use WSL2 (native support is experimental) |

```bash
# Example: Linux x86_64
curl -LO https://github.com/lance0/xfr/releases/latest/download/xfr-x86_64-unknown-linux-musl.tar.gz
tar xzf xfr-*.tar.gz && sudo mv xfr /usr/local/bin/
```

### eget

```bash
eget lance0/xfr
```

### Arch Linux (AUR)

```bash
yay -S xfr-bin
```

### From Source

```bash
git clone https://github.com/lance0/xfr
cd xfr && cargo build --release
sudo cp target/release/xfr /usr/local/bin/
```

### Quick Install Script

> **Note**: Review scripts before piping to sh. See the [install script](../install.sh) source.

```bash
curl -fsSL https://raw.githubusercontent.com/lance0/xfr/master/install.sh | sh
```

### Termux (Android)

Download the `aarch64-linux-android` binary from [releases](https://github.com/lance0/xfr/releases), or build from source:

```bash
pkg install rust
cargo install xfr
```

### Nix

```bash
nix run github:lance0/xfr          # Run without installing
nix profile install github:lance0/xfr  # Install to profile
```

Or add to your flake inputs:

```nix
inputs.xfr.url = "github:lance0/xfr";
```

A dev shell is also available via `nix develop`.

### NetBSD

Available via [pkgsrc](https://pkgsrc.se/net/xfr):

```bash
pkgin install xfr
```

### Optional Features

| Feature | Default | Description |
|---------|---------|-------------|
| `discovery` | Yes | mDNS LAN discovery (`xfr discover`) |
| `update-check` | Yes | Background release notification; see [update settings](FEATURES.md#update-notifications) |
| `prometheus` | No | Prometheus metrics HTTP endpoint |

```bash
cargo install xfr --features prometheus    # Metrics endpoint
cargo install xfr --all-features           # All features
```

Push Gateway support is available without the optional `prometheus` feature;
see [metrics integration](SCRIPTING.md#prometheus-integration).

### Shell Completions

```bash
# Bash
xfr --completions bash > ~/.local/share/bash-completion/completions/xfr

# Zsh (add ~/.zfunc to fpath in .zshrc first)
xfr --completions zsh > ~/.zfunc/_xfr

# Fish
xfr --completions fish > ~/.config/fish/completions/xfr.fish

# PowerShell (add to $PROFILE)
xfr --completions powershell >> $PROFILE

# Elvish
xfr --completions elvish > ~/.elvish/lib/xfr.elv
```

## Platform Support

| Platform | Status |
|----------|--------|
| Linux x86_64/ARM64 | Release binaries; CI tests run on Ubuntu x86_64 |
| macOS Apple Silicon | Release binary; CI tests run on macOS |
| macOS Intel | Build with `cargo install xfr`; no release binary |
| Android (Termux) | ARM64 release binary; cross-built, not runtime-tested in CI |
| NetBSD | Community pkgsrc package; not runtime-tested in CI |
| Windows | Experimental; WSL2 recommended; native builds lack TCP_INFO metrics. See [contributor checks](../CONTRIBUTING.md#testing) for validation scope |

CI and release builds do not qualify every protocol, terminal, or network setup.
MPTCP and TCP zero-copy sends require Linux; see [transport details](FEATURES.md#transport-modes).

## Troubleshooting

### Permission denied on port 5201

Port 5201 is above the usual privileged-port range. Check local security policy
and whether the port is already in use; try another unprivileged port:

```bash
xfr serve -p 9000
```

### Connection refused

Ensure the server is running and the port is not blocked by a firewall. TCP, UDP, and QUIC all run entirely on port 5201 (or your custom port) when both ends are current -- no server-side ephemeral data ports are needed (UDP single-port requires both ends ≥ v0.9.18; older pairings fall back to ephemeral server ports automatically). For strict egress policies or ECMP testing, use `--cport` to pin client source ports.

### Low throughput

- Try multiple parallel streams: `-P 4`
- Disable Nagle's algorithm: `--tcp-nodelay`
- Increase TCP socket buffer: `--window 4M`
- On CPU-bound senders (embedded routers, SBCs), the per-write userspace copy is skipped by default via sendfile(2) (TCP, Linux). This applies to client sends, and to server sends in `-R`/`--bidir` when the server supports it; pass `-Z` to get a warning when zero-copy can't take effect, or `--no-zerocopy` to disable it

### UDP packet loss

- Reduce bitrate: `-b 500M`
- Check for network congestion or firewall issues
- If the receiver may be CPU-bound or its kernel UDP buffer is small, increase `--window` (e.g. `-w 16M`) — `-w` applies to UDP `SO_SNDBUF`/`SO_RCVBUF` and propagates to the server, helping high-rate flows avoid kernel tail-drops that hide as live `0.0%` loss
