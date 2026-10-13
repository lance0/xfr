#!/usr/bin/env bash
set -euo pipefail

# IPv6 link-local hosts with a zone ID (fe80::2%v0, fe80::2%<ifindex>).
# Without the zone the kernel cannot route a link-local address, so every
# mode must carry the scope id through the control connection, the TCP/UDP
# data sockets and the QUIC endpoint. Run with sudo after
# `cargo build --release`.
XFR="$(pwd)/target/release/xfr"
[[ -x "$XFR" ]] || { echo "Build target/release/xfr first"; exit 1; }
CLIENT_NS="xfrll_cli_$$"
SERVER_NS="xfrll_srv_$$"
TEST_TMP=$(mktemp -d)
SERVER_PID=""
cleanup() {
    if [[ -n "$SERVER_PID" ]]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    ip netns del "$CLIENT_NS" 2>/dev/null || true
    ip netns del "$SERVER_NS" 2>/dev/null || true
    rm -rf "$TEST_TMP"
}
trap cleanup EXIT

ip netns add "$CLIENT_NS"
ip netns add "$SERVER_NS"
ip link add v0 netns "$CLIENT_NS" type veth peer name v1 netns "$SERVER_NS"
# One link-local address per side: suppress the kernel's EUI-64 address so
# the fixed fe80::1/fe80::2 pair is the only choice for source selection.
for spec in "$CLIENT_NS v0 fe80::1" "$SERVER_NS v1 fe80::2"; do
    read -r ns dev addr <<<"$spec"
    ip -n "$ns" link set lo up
    ip -n "$ns" link set "$dev" addrgenmode none
    ip -n "$ns" -6 addr add "$addr/64" dev "$dev" nodad
    ip -n "$ns" link set "$dev" up
done
IFINDEX=$(ip netns exec "$CLIENT_NS" cat /sys/class/net/v0/ifindex)

ip netns exec "$SERVER_NS" "$XFR" serve -p 5203 >"$TEST_TMP/server.log" 2>&1 &
SERVER_PID=$!
sleep .5

run() {
    local label=$1
    shift
    local result="$TEST_TMP/result.json"
    if ! ip netns exec "$CLIENT_NS" timeout 20 "$XFR" "$@" -p 5203 -t 2sec --json --no-tui -o "$result" >"$TEST_TMP/client.log" 2>&1; then
        echo "FAIL: $label"
        cat "$TEST_TMP/client.log"
        exit 1
    fi
    python3 - "$result" "$label" <<'PY'
import json
import sys
with open(sys.argv[1]) as f:
    result = json.load(f)
assert result["throughput_mbps"] > 1, result
print(f"{sys.argv[2]}: {result['throughput_mbps']:.1f} Mbit/s")
PY
}

for host in "fe80::2%v0" "fe80::2%$IFINDEX"; do
    run "$host TCP" "$host"
    run "$host TCP -R -P 2" "$host" -R -P 2
    run "$host UDP" "$host" -u -b 200M
    run "$host UDP -R" "$host" -u -b 200M -R
    run "$host UDP --bidir" "$host" -u -b 200M --bidir
    run "$host QUIC" "$host" -Q
    run "$host QUIC -R" "$host" -Q -R
done
run "TCP --bind fe80::1%v0 --cport" "[fe80::2%v0]" --bind "fe80::1%v0" --cport 6100 -P 2
run "UDP --bind fe80::1%v0" "fe80::2%v0" -u -b 200M --bind "fe80::1%v0"

if ip netns exec "$CLIENT_NS" "$XFR" "fe80::2%nosuch0" -p 5203 -t 1sec --no-tui >"$TEST_TMP/client.log" 2>&1; then
    echo "FAIL: unknown zone was accepted"
    exit 1
fi
grep -q "nosuch0" "$TEST_TMP/client.log" || { echo "FAIL: unknown-zone error does not name the zone"; cat "$TEST_TMP/client.log"; exit 1; }
echo "Unknown zone rejected: $(grep -o 'IPv6 zone.*' "$TEST_TMP/client.log")"
