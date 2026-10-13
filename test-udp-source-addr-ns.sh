#!/usr/bin/env bash
set -euo pipefail

# Server with two addresses per family on one interface (secondary IPv4,
# a second static IPv6). A client that targets the address the kernel
# would NOT pick as the reply source must still get its UDP acks and data
# from the address it targeted: its connected socket drops anything else,
# and the single-port handshake times out. Run with sudo after
# `cargo build --release` (XFR=/path/to/xfr overrides the binary).
XFR="${XFR:-$(pwd)/target/release/xfr}"
[[ -x "$XFR" ]] || { echo "Build target/release/xfr first"; exit 1; }
CLIENT_NS="xfrsrc_cli_$$"
SERVER_NS="xfrsrc_srv_$$"
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
for ns in "$CLIENT_NS" "$SERVER_NS"; do
    ip -n "$ns" link set lo up
done
ip -n "$CLIENT_NS" addr add 10.9.0.2/24 dev v0
ip -n "$CLIENT_NS" -6 addr add fd00::1/64 dev v0 nodad
for addr in 10.9.0.1/24 10.9.0.3/24; do
    ip -n "$SERVER_NS" addr add "$addr" dev v1
done
for addr in fd00::2/64 fd00::3/64; do
    ip -n "$SERVER_NS" -6 addr add "$addr" dev v1 nodad
done
ip -n "$CLIENT_NS" link set v0 up
ip -n "$SERVER_NS" link set v1 up

# The address the server's kernel picks as source toward the client, and
# the other one, which a wildcard-bound socket would never reply from.
preferred_src() {
    ip -n "$SERVER_NS" route get "$1" | grep -o 'src [^ ]*' | cut -d' ' -f2
}
PREF4=$(preferred_src 10.9.0.2)
PREF6=$(preferred_src fd00::1)
OTHER4=10.9.0.3
[[ "$PREF4" == 10.9.0.3 ]] && OTHER4=10.9.0.1
OTHER6=fd00::3
[[ "$PREF6" == fd00::3 ]] && OTHER6=fd00::2
echo "Server source preference: IPv4 $PREF4 (testing $OTHER4), IPv6 $PREF6 (testing $OTHER6)"

ip netns exec "$SERVER_NS" "$XFR" serve -p 5204 >"$TEST_TMP/server.log" 2>&1 &
SERVER_PID=$!
sleep .5

FAILED=0
run() {
    local label=$1 port=$2 host=$3
    shift 3
    local result="$TEST_TMP/result.json"
    if ! ip netns exec "$CLIENT_NS" timeout 20 "$XFR" "$host" -p "$port" -t 1sec --json --no-tui -o "$result" "$@" >"$TEST_TMP/client.log" 2>&1; then
        echo "FAIL: $label"
        grep -v INFO "$TEST_TMP/client.log" || true
        FAILED=1
        return
    fi
    python3 - "$result" "$label" <<'PY'
import json
import sys
with open(sys.argv[1]) as f:
    result = json.load(f)
assert result["throughput_mbps"] > 1, result
if "--bidir" in sys.argv[2]:
    assert result["bytes_sent"] > 0 and result["bytes_received"] > 0, result
print(f"{sys.argv[2]}: {result['throughput_mbps']:.1f} Mbit/s")
PY
}

for host in "$OTHER4" "$OTHER6" "$PREF4" "$PREF6"; do
    run "$host TCP" 5204 "$host"
    run "$host QUIC" 5204 "$host" -Q
    run "$host UDP" 5204 "$host" -u -b 50M
    run "$host UDP -R" 5204 "$host" -u -b 50M -R
    run "$host UDP --bidir" 5204 "$host" -u -b 50M --bidir
    run "$host UDP -P 2 -R" 5204 "$host" -u -b 50M -P 2 -R
done
(( FAILED == 0 )) || { echo "FAIL: replies did not come from the targeted address"; exit 1; }
