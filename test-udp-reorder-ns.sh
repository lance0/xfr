#!/usr/bin/env bash
set -euo pipefail

# UDP reordering must not be reported as loss. netem jitter (delay 10ms 3ms)
# on UDP only reorders datagrams without dropping any, so every receiver
# should see out-of-order arrivals and essentially zero loss: download (-R)
# and both directions of --bidir. The TCP control channel stays unshaped.
# Run with sudo after cargo build --release.
XFR="$(pwd)/target/release/xfr"
[[ -x "$XFR" ]] || { echo "Build target/release/xfr first"; exit 1; }
CLIENT_NS="xfrreo_cli_$$"
SERVER_NS="xfrreo_srv_$$"
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
ip -n "$CLIENT_NS" addr add 10.9.1.1/24 dev v0
ip -n "$SERVER_NS" addr add 10.9.1.2/24 dev v1
for entry in "$CLIENT_NS:v0" "$SERVER_NS:v1"; do
    ns=${entry%:*}
    dev=${entry#*:}
    ip -n "$ns" link set lo up
    ip -n "$ns" link set "$dev" up
    # Jitter on UDP only: band 3 of a prio qdisc, selected by IP protocol 17.
    tc -n "$ns" qdisc add dev "$dev" root handle 1: prio
    tc -n "$ns" qdisc add dev "$dev" parent 1:3 handle 30: netem delay 10ms 3ms
    tc -n "$ns" filter add dev "$dev" parent 1: protocol ip u32 \
        match ip protocol 17 0xff flowid 1:3
done
ip netns exec "$SERVER_NS" "$XFR" serve -p 5203 >"$TEST_TMP/server.log" 2>&1 &
SERVER_PID=$!
sleep .5

for mode in download bidir; do
    flag=-R
    [[ "$mode" == bidir ]] && flag=--bidir
    result="$TEST_TMP/$mode.json"
    ip netns exec "$CLIENT_NS" timeout 30 "$XFR" 10.9.1.2 -p 5203 -u -b 50M -t 5sec "$flag" \
        --json --no-tui -o "$result" >/dev/null
    python3 - "$result" "$mode" <<'PY'
import json
import sys
with open(sys.argv[1]) as f:
    result = json.load(f)
mode = sys.argv[2]
keys = ["udp_stats"] if mode == "download" else ["udp_stats_send", "udp_stats_recv"]
for key in keys:
    stats = result[key]
    line = (f"{mode} {key}: received={stats['packets_received']} lost={stats['lost']} "
            f"({stats['lost_percent']:.3f}%) out_of_order={stats['out_of_order']}")
    print(line)
    assert stats["packets_received"] > 10_000, line
    # Without out-of-order arrivals the fixture proved nothing.
    assert stats["out_of_order"] > 100, line
    # Reordered packets repair their gaps; only real drops may remain.
    assert stats["lost_percent"] < 0.5, line
PY
done
