#!/usr/bin/env bash
# Locate the built `xfr` binary for a (target, triple, profile) and verify its
# platform ABI, then print the resolved path on stdout (diagnostics go to
# stderr, so callers can `bin=$(verify-target-binary.sh ...)`).
#
# Handles cargo-zigbuild's glibc-suffixed target dir: `--target foo-gnu.2.17`
# may emit under `target/foo-gnu.2.17/` or the normalized `target/foo-gnu/`.
#
# ABI checks (fail the build if breached):
#   *-gnu   -> no GLIBC symbol newer than the pinned floor (2.17)
#   *-musl  -> statically linked (no GLIBC symbols, no dynamic interpreter)
#   others  -> no ABI check (android NDK, macOS Mach-O)
#
# Usage: verify-target-binary.sh <rustup-target> <build-triple> [profile]
set -euo pipefail

target="$1"
triple="$2"
profile="${3:-release}"
glibc_floor="2.17"

bin=""
for d in "target/${triple}/${profile}" "target/${target}/${profile}"; do
    if [[ -x "$d/xfr" ]]; then
        bin="$d/xfr"
        break
    fi
done

if [[ -z "$bin" ]]; then
    echo "::error::xfr binary not found for ${target} / ${triple} (${profile})" >&2
    echo "searched: target/${triple}/${profile}/xfr and target/${target}/${profile}/xfr" >&2
    find target -maxdepth 4 -type f -name xfr -print >&2 || true
    exit 1
fi

echo "resolved binary: ${bin}" >&2
file "$bin" >&2 || true

case "$triple" in
    *-musl*)
        # Static musl: must not reference glibc or carry a dynamic interpreter.
        if readelf -W --version-info "$bin" 2>/dev/null | grep -q 'GLIBC_'; then
            echo "::error::${bin} references GLIBC symbols but should be static musl" >&2
            exit 1
        fi
        if readelf -W -l "$bin" 2>/dev/null | grep -qi 'program interpreter'; then
            echo "::error::${bin} has a dynamic interpreter but should be static musl" >&2
            exit 1
        fi
        echo "OK: static musl (no glibc, no interpreter)" >&2
        ;;
    *-gnu*)
        # glibc floor: no required GLIBC symbol above ${glibc_floor}.
        maxglibc="$(readelf -W --version-info "$bin" 2>/dev/null \
            | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sed 's/GLIBC_//' | sort -V | tail -1)"
        echo "max GLIBC required: ${maxglibc:-none} (floor ${glibc_floor})" >&2
        if [[ -n "$maxglibc" \
            && "$(printf '%s\n%s\n' "$maxglibc" "$glibc_floor" | sort -V | tail -1)" != "$glibc_floor" ]]; then
            echo "::error::${bin} requires glibc ${maxglibc}, above the ${glibc_floor} floor" >&2
            exit 1
        fi
        echo "OK: glibc floor <= ${glibc_floor}" >&2
        ;;
    *)
        echo "no ABI check for ${triple}" >&2
        ;;
esac

# stdout: the resolved path, for the caller to package.
echo "$bin"
