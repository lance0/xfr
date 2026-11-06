#!/bin/sh
# xfr installer - https://github.com/lance0/xfr
# Usage: curl -fsSL https://raw.githubusercontent.com/lance0/xfr/master/install.sh | sh
#    or: wget -qO- https://raw.githubusercontent.com/lance0/xfr/master/install.sh | sh

set -e

REPO="lance0/xfr"

# Overridable for scripts/test-install-arch.sh
CPUINFO="${XFR_INSTALL_CPUINFO:-/proc/cpuinfo}"
GLIBC_LOADER="${XFR_INSTALL_GLIBC_LOADER:-/lib/ld-linux-aarch64.so.1}"

# Default install directory (Termux uses $PREFIX/bin; OpenWrt has no /usr/local/bin in PATH)
if [ -n "$TERMUX_VERSION" ]; then
  INSTALL_DIR="${INSTALL_DIR:-$PREFIX/bin}"
elif [ -f /etc/openwrt_release ]; then
  INSTALL_DIR="${INSTALL_DIR:-/usr/bin}"
else
  INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
fi

# Detect OS and architecture
# (no tr: OpenWrt's busybox tr lacks [:upper:] and turns "Linux" into "Linlx")
OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
  Linux)
    # Detect Termux (Android)
    if [ -n "$TERMUX_VERSION" ]; then
      case "$ARCH" in
        aarch64) TARGET="aarch64-linux-android" ;;
        arm64)   TARGET="aarch64-linux-android" ;;
        *) echo "Unsupported Termux architecture: $ARCH"; exit 1 ;;
      esac
    else
      case "$ARCH" in
        x86_64)  TARGET="x86_64-unknown-linux-musl" ;;
        aarch64|arm64)
          # musl-only systems (OpenWrt, Alpine) have no glibc loader for the -gnu build
          if [ -e "$GLIBC_LOADER" ] && ! ldd --version 2>&1 | grep -qi musl; then
            TARGET="aarch64-unknown-linux-gnu"
          else
            TARGET="aarch64-unknown-linux-musl"
          fi
          ;;
        armv7l|armv8l)
          # The ARMv7 build is little-endian and hard-float (musleabihf); big-endian
          # armv7b falls through to unsupported, and it can't run without an FPU
          if grep -i '^features' "$CPUINFO" 2>/dev/null | grep -qwE 'vfp[a-z0-9]*|neon'; then
            TARGET="armv7-unknown-linux-musleabihf"
          else
            echo "Unsupported architecture: $ARCH without a hardware FPU (no vfp/neon in $CPUINFO)"
            echo "Release binaries cover hard-float ARMv7 only."
            exit 1
          fi
          ;;
        *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
      esac
    fi
    ;;
  Darwin)
    case "$ARCH" in
      arm64)   TARGET="aarch64-apple-darwin" ;;
      aarch64) TARGET="aarch64-apple-darwin" ;;
      x86_64)
        echo "Intel macOS (x86_64) release binaries are no longer provided."
        echo "Install from source with: cargo install xfr"
        exit 1
        ;;
      *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
    esac
    ;;
  *)
    echo "Unsupported OS: $OS"
    exit 1
    ;;
esac

if [ -n "$XFR_INSTALL_PRINT_TARGET" ]; then
  echo "$TARGET"
  exit 0
fi

# fetch URL FILE ("-" for stdout). Always name the file: OpenWrt's wget
# (uclient-fetch) otherwise saves a redirected GitHub download under the S3 name.
fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    echo "curl or wget is required" >&2
    exit 1
  fi
}

# Root (OpenWrt, containers) doesn't need sudo, which may not be installed
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

# Get latest version
# (sed, not grep | cut: the API sends one-line JSON to clients such as uclient-fetch)
VERSION=$(fetch "https://api.github.com/repos/$REPO/releases/latest" - | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')
if [ -z "$VERSION" ]; then
  echo "Failed to get latest version"
  exit 1
fi

URL="https://github.com/$REPO/releases/download/$VERSION/xfr-$TARGET.tar.gz"

echo "Installing xfr $VERSION for $TARGET..."

# Download and extract
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

fetch "$URL" "$TMPDIR/xfr.tar.gz"
tar xzf "$TMPDIR/xfr.tar.gz" -C "$TMPDIR"

# Create install directory if it doesn't exist
if [ ! -d "$INSTALL_DIR" ]; then
  echo "Creating $INSTALL_DIR..."
  $SUDO mkdir -p "$INSTALL_DIR"
fi

# Install
if [ -w "$INSTALL_DIR" ]; then
  mv "$TMPDIR/xfr" "$INSTALL_DIR/xfr"
else
  echo "Installing to $INSTALL_DIR (requires sudo)..."
  $SUDO mv "$TMPDIR/xfr" "$INSTALL_DIR/xfr"
fi

echo "Installed xfr to $INSTALL_DIR/xfr"
echo ""
echo "Run 'xfr --help' to get started"
