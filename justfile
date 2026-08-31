set shell := ["bash", "-euo", "pipefail", "-c"]

# List the available maintainer commands.
default:
    @just --list

# Build with the checked-in dependency lockfile.
build:
    cargo build --locked

# Run the standard pre-PR checks.
check: fmt-check lint test doc

# Format the workspace.
fmt:
    cargo fmt --all

# Check formatting without changing files.
fmt-check:
    cargo fmt --all -- --check

# Lint both supported feature configurations.
lint:
    cargo clippy --locked --all-targets --all-features -- -D warnings
    cargo clippy --locked --all-targets --no-default-features -- -D warnings

# Test both supported feature configurations.
test:
    cargo test --locked --all-features
    cargo test --locked --no-default-features

# Build documentation with warnings denied.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --all-features --no-deps

# Run the Criterion benchmarks.
bench:
    cargo bench --locked

# Install the configured pre-commit and pre-push hooks.
install-hooks:
    prek install

# Run the privileged Linux network namespace tests.
netns:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" != "Linux" ]]; then
        printf '%s\n' "The network namespace tests require Linux."
        exit 1
    fi
    cargo build --release --locked
    sudo ./test-mptcp-ns.sh
    sudo ./test-control-channel-skew.sh
    sudo ./test-mtu-probe-ns.sh
