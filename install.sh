#!/bin/sh
# xfr installer - https://github.com/lance0/xfr
# Usage: curl -fsSL https://raw.githubusercontent.com/lance0/xfr/master/install.sh | sh

set -e

REPO="lance0/xfr"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

# Detect OS and architecture
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  linux)
    case "$ARCH" in
      x86_64)  TARGET="x86_64-unknown-linux-musl" ;;
      aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
      arm64)   TARGET="aarch64-unknown-linux-gnu" ;;
      *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
    esac
    ;;
  darwin)
    case "$ARCH" in
      arm64)   TARGET="aarch64-apple-darwin" ;;
      aarch64) TARGET="aarch64-apple-darwin" ;;
      x86_64)  TARGET="x86_64-apple-darwin" ;;
      *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
    esac
    ;;
  *)
    echo "Unsupported OS: $OS"
    exit 1
    ;;
esac

# Get latest version
VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | cut -d'"' -f4)
if [ -z "$VERSION" ]; then
  echo "Failed to get latest version"
  exit 1
fi

URL="https://github.com/$REPO/releases/download/$VERSION/xfr-$TARGET.tar.gz"

echo "Installing xfr $VERSION for $TARGET..."

# Download and extract
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

curl -fsSL "$URL" | tar xz -C "$TMPDIR"

# Install
if [ -w "$INSTALL_DIR" ]; then
  mv "$TMPDIR/xfr" "$INSTALL_DIR/xfr"
else
  echo "Installing to $INSTALL_DIR (requires sudo)..."
  sudo mv "$TMPDIR/xfr" "$INSTALL_DIR/xfr"
fi

echo "Installed xfr to $INSTALL_DIR/xfr"
echo ""
echo "Run 'xfr --help' to get started"
