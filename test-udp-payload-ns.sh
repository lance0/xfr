#!/usr/bin/env bash
set -euo pipefail

# Issue #213: 1420-byte MTU, 50 Mbit/s HTB + fq_codel, 60 Mbit/s offered.
# Run with sudo after cargo build --release. No tunnel is needed to reproduce
# fragmentation; check both IP families and every throughput direction.
XFR="$(pwd)/target/release/xfr"
[[ -x "$XFR" ]] || { echo "Build target/release/xfr first"; exit 1; }
CLIENT_NS="xfr213_cli_$$"
SERVER_NS="xfr213_srv_$$"
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
ip -n "$CLIENT_NS" addr add 10.9.0.1/24 dev v0
ip -n "$SERVER_NS" addr add 10.9.0.2/24 dev v1
ip -n "$CLIENT_NS" -6 addr add fd00:213::1/64 dev v0 nodad
ip -n "$SERVER_NS" -6 addr add fd00:213::2/64 dev v1 nodad
for entry in "$CLIENT_NS:v0" "$SERVER_NS:v1"; do
    ns=${entry%:*}
    dev=${entry#*:}
    ip -n "$ns" link set "$dev" mtu 1420 up
    if command -v ethtool >/dev/null; then
        ip netns exec "$ns" ethtool -K "$dev" tso off gso off gro off >/dev/null
    fi
    ip netns exec "$ns" tc qdisc add dev "$dev" root handle 1: htb default 1
    ip netns exec "$ns" tc class add dev "$dev" parent 1: classid 1:1 htb rate 50mbit ceil 50mbit
    ip netns exec "$ns" tc qdisc add dev "$dev" parent 1:1 fq_codel
done
ip netns exec "$SERVER_NS" "$XFR" serve -p 5202 >"$TEST_TMP/server.log" 2>&1 &
SERVER_PID=$!
sleep .5

fragments() {
    local ns
    for ns in "$CLIENT_NS" "$SERVER_NS"; do
        ip netns exec "$ns" nstat -az IpFragCreates Ip6FragCreates
    done | awk '$1 == "IpFragCreates" || $1 == "Ip6FragCreates" {sum += $2} END {print sum+0}'
}

# Prove the fixture detects the original oversized default. Delivery may be
# zero here, as reported in the issue; only require evidence of fragmentation.
before=$(fragments)
ip netns exec "$CLIENT_NS" timeout 20 "$XFR" 10.9.0.2 -4 -p 5202 -u -b 60M -t 1 --json --no-tui >"$TEST_TMP/baseline.json"
after=$(fragments)
(( after > before )) || { echo "FAIL: baseline did not produce IPv4 fragments"; exit 1; }
echo "Baseline: $((after - before)) fragments with the 1400-byte default"

for family in 4 6; do
    host=10.9.0.2
    [[ "$family" == 6 ]] && host=fd00:213::2
    for direction in upload download bidir; do
        flags=()
        [[ "$direction" == download ]] && flags=(-R)
        [[ "$direction" == bidir ]] && flags=(--bidir)
        for run in 1 2 3; do
            before=$(fragments)
            result="$TEST_TMP/result.json"
            ip netns exec "$CLIENT_NS" timeout 20 "$XFR" "$host" "-$family" -p 5202 -u -b 60M -t 2 -l 1368 "${flags[@]}" --json --no-tui >"$result"
            after=$(fragments)
            [[ "$after" == "$before" ]] || { echo "FAIL: IPv$family $direction fragmented"; exit 1; }
            python3 - "$result" "$family" "$direction" "$run" <<'PY'
import json
import sys
with open(sys.argv[1]) as f:
    result = json.load(f)
stats = result["udp_stats"]
assert stats["packet_size"] == 1368, result
assert stats["packets_received"] > 0, result
assert result["throughput_mbps"] > 10, result
if sys.argv[3] == "bidir":
    assert result["bytes_sent"] > 0 and result["bytes_received"] > 0, result
print(f"IPv{sys.argv[2]} {sys.argv[3]} run {sys.argv[4]}: {result['throughput_mbps']:.1f} Mbit/s, no fragments")
PY
        done
    done
done
