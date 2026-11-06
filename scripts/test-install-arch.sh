#!/bin/sh
# Check which release asset install.sh picks, with uname, ldd, /proc/cpuinfo
# and the glibc loader stubbed. Downloads nothing.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir "$work/bin"
cat > "$work/bin/uname" <<'EOF'
#!/bin/sh
case "$1" in -m) echo "$FAKE_ARCH" ;; *) echo Linux ;; esac
EOF
cat > "$work/bin/ldd" <<'EOF'
#!/bin/sh
echo "$FAKE_LDD"
EOF
chmod +x "$work/bin/uname" "$work/bin/ldd"

# Features lines as Linux reports them for a Cortex-A7 and a soft-float Cortex-A9
printf 'processor\t: 0\nFeatures\t: half thumb fastmult vfp edsp neon vfpv3 tls vfpv4 idiva idivt lpae evtstrm\n' > "$work/cpuinfo-hard"
printf 'processor\t: 0\nFeatures\t: half thumb fastmult edsp tls\n' > "$work/cpuinfo-soft"
touch "$work/ld-linux"
glibc="ldd (GNU libc) 2.39"

fail=0
# check NAME ARCH CPUINFO LOADER LDD EXPECTED ("refused" = install.sh exits non-zero)
check() {
  if got=$(TERMUX_VERSION="" PATH="$work/bin:$PATH" FAKE_ARCH="$2" FAKE_LDD="$5" \
    XFR_INSTALL_CPUINFO="$3" XFR_INSTALL_GLIBC_LOADER="$4" XFR_INSTALL_PRINT_TARGET=1 \
    sh "$root/install.sh" 2>&1); then :; else got="refused"; fi
  if [ "$got" = "$6" ]; then
    echo "ok   $1: $got"
  else
    echo "FAIL $1: expected $6, got $got"
    fail=1
  fi
}

check "armv7 hard-float"   armv7l "$work/cpuinfo-hard" "$work/none"     "$glibc" armv7-unknown-linux-musleabihf
check "armv7 soft-float"   armv7l "$work/cpuinfo-soft" "$work/none"     "$glibc" refused
check "armv6"              armv6l "$work/cpuinfo-hard" "$work/none"     "$glibc" refused
check "aarch64 glibc"      aarch64 "$work/cpuinfo-hard" "$work/ld-linux" "$glibc" aarch64-unknown-linux-gnu
check "aarch64 musl"       aarch64 "$work/cpuinfo-hard" "$work/none"     "$glibc" aarch64-unknown-linux-musl
check "aarch64 gcompat"    aarch64 "$work/cpuinfo-hard" "$work/ld-linux" "musl libc (aarch64)" aarch64-unknown-linux-musl
check "x86_64"             x86_64 "$work/cpuinfo-hard" "$work/none"     "$glibc" x86_64-unknown-linux-musl

exit "$fail"
