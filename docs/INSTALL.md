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
| Linux ARMv7 (32-bit, hard-float) | `xfr-armv7-unknown-linux-musleabihf.tar.gz` (fully static) |
| Routers and other small systems | `xfr-armv7-unknown-linux-musleabihf-minimal.tar.gz`, `xfr-aarch64-unknown-linux-musl-minimal.tar.gz` (see [OpenWrt](#openwrt)) |
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

The script picks the release binary for Linux x86_64, ARM64 (the static
`-musl` build on systems without glibc, such as Alpine and OpenWrt), hard-float
ARMv7, macOS Apple Silicon, and Termux. It works with `curl` or `wget` and
installs to `/usr/local/bin` (`/usr/bin` on OpenWrt, `$PREFIX/bin` on Termux);
set `INSTALL_DIR` to change that.

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

### OpenWrt

There is no OpenWrt package (`opkg`/`apk`). The static musl release binaries
run unchanged on OpenWrt, which has no glibc, so pick the `-musl` build for
the router's package architecture, never the `-gnu` one:

| OpenWrt architecture | Release binary |
|----------------------|----------------|
| `aarch64_*` (e.g. `aarch64_cortex-a53`) | `xfr-aarch64-unknown-linux-musl.tar.gz` |
| ARMv7 with an FPU, i.e. `vfpv3`, `vfpv4` or `neon` in the name (e.g. `arm_cortex-a7_neon-vfpv4`, `arm_cortex-a9_vfpv3-d16`) | `xfr-armv7-unknown-linux-musleabihf.tar.gz` |
| `x86_64` | `xfr-x86_64-unknown-linux-musl.tar.gz` |

ARM architectures without an FPU suffix (plain `arm_cortex-a9`, `arm_arm926ej-s`)
and MIPS (`mips_*`, `mipsel_*`) have no release binary; MIPS is a tier 3 Rust
target without a prebuilt standard library.

The install script handles this choice and refuses ARMv7 CPUs without an FPU.
On OpenWrt it installs to `/usr/bin` by default, which is on the flash
overlay. The example below sets `INSTALL_DIR=/tmp` to keep the binary in RAM
instead (gone after a reboot); drop it to install to `/usr/bin` if the overlay
has room:

```sh
wget -qO- https://raw.githubusercontent.com/lance0/xfr/master/install.sh | INSTALL_DIR=/tmp sh
```

To pick the binary by hand:

```sh
# Show the package architecture
. /etc/openwrt_release && echo "$DISTRIB_ARCH"

# Example: aarch64; /tmp is RAM, so the binary is gone after a reboot
cd /tmp
# -O: OpenWrt's wget would otherwise name the file after GitHub's redirect target
wget -O xfr-aarch64-unknown-linux-musl.tar.gz https://github.com/lance0/xfr/releases/latest/download/xfr-aarch64-unknown-linux-musl.tar.gz
tar xzf xfr-aarch64-unknown-linux-musl.tar.gz
./xfr --version
```

The binary is about 9 MB. Run it from `/tmp` as above, or copy it to
`/usr/bin` if the overlay has room (`df -h /overlay`).

For less flash, ARMv7 and aarch64 also have `-minimal` builds of about 7.4 MB,
such as `xfr-aarch64-unknown-linux-musl-minimal.tar.gz`. They have no TUI (the
client uses plain output and `serve --tui` runs the plain server), no mDNS
discovery (`xfr discover` and the server's announcement), no update check and
no Prometheus endpoint. Push Gateway support stays. `install.sh` installs the
full build, so download these by hand.

Only the client TUI checks GitHub for a new release, in the background;
`xfr serve` and plain, JSON or CSV output never do. To turn the check off, set
`XFR_NO_UPDATE_CHECK=1` or pass `--no-update-check`, a global flag that goes
before any subcommand. Neither makes the binary smaller; the `-minimal` builds
leave the checker out, as does a source build with
`cargo build --release --no-default-features`.

The client needs nothing else. `xfr serve` on the router is reachable from
the LAN zone as-is, since the default LAN zone accepts input. Clients in a
zone that rejects input, such as a guest network or a VPN, need a rule for
TCP and UDP 5201 (or your `--port`). Replace `guest` with the zone the
clients come from:

```sh
uci add firewall rule
uci set firewall.@rule[-1].name='Allow-xfr'
uci set firewall.@rule[-1].src='guest'
uci set firewall.@rule[-1].dest_port='5201'
uci set firewall.@rule[-1].proto='tcp udp'
uci set firewall.@rule[-1].target='ACCEPT'
uci commit firewall
/etc/init.d/firewall reload
```

Avoid opening the port on `wan`; if you must, set a PSK (`--psk`/`--psk-file`)
first.

### Optional Features

| Feature | Default | Description |
|---------|---------|-------------|
| `discovery` | Yes | mDNS LAN discovery (`xfr discover`) |
| `tui` | Yes | Client and server dashboards; without it the client always uses plain output |
| `update-check` | Yes | Background release notification, needs `tui`; see [update settings](FEATURES.md#update-notifications) |
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
| Linux ARMv7 | Static musl release binary; cross-built, not runtime-tested in CI |
| macOS Apple Silicon | Release binary; CI tests run on macOS |
| macOS Intel | Build with `cargo install xfr`; no release binary |
| Android (Termux) | ARM64 release binary; cross-built, not runtime-tested in CI |
| NetBSD | Community pkgsrc package; not runtime-tested in CI |
| OpenWrt | Static musl release binaries; not runtime-tested in CI |
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
