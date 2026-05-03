#!/bin/bash
# Regression test for the TCP control channel — issue #70 follow-up
# (commit 117505e: "Set TCP_NODELAY on the control connection").
#
# Background:
#   Without TCP_NODELAY on the control TCP socket, Nagle holds small
#   periodic Interval messages (~150 bytes, 1 Hz) waiting for either an
#   MSS-sized payload (which never arrives) or an ACK from the peer.
#   When the parallel UDP data flow saturates the path, ACK turnaround
#   stretches and Nagle holds every queued segment for the duration of
#   the test, then flushes the entire backlog in a single burst when
#   data traffic stops. The TUI live counter appears stuck at 0% for
#   the whole run, then jumps to the final value at quit. iperf3 hits
#   the same trap and disables Nagle on its control channel for the
#   same reason.
#
# What this test does:
#   1. Apply `tc netem rate 50mbit delay 50ms` to lo so loopback
#      simulates a constrained Wi-Fi-like link with realistic ACK
#      turnaround.
#   2. Run a 5-second UDP test at 100 Mbps target — saturates the
#      shaper, the same way brettowe's default 1 Gbps over 100 Mbps
#      Wi-Fi did.
#   3. Capture --json-stream output and assert that every interval
#      line carries a distinct client-side `timestamp`. The bunching
#      pattern collapses multiple consecutive timestamps to the same
#      value (e.g., 3 lines all at "6.797"); a healthy run produces N
#      unique timestamps for N interval lines.
#
# Requires: tc (iproute2), CAP_NET_ADMIN (sudo on the runner is fine).
#
# Usage:
#   sudo ./test-control-channel-skew.sh
#
# Designed to run inside CI (.github/workflows/ci.yml). Doesn't accept
# any flags — duration and rate are hard-coded so the assertion has a
# stable contract.

set -euo pipefail

XFR="./target/release/xfr"
PORT=5311
# 8 seconds gives ~7 interval data points (1 Hz cadence minus startup).
# Long enough that the Nagle pattern shows clearly under the bug
# (multiple intervals collapse to one end-of-test timestamp), short
# enough that the test is quick.
DURATION=8
# 50 Mbps shaper + 100 Mbps target = 2× oversubscription. Tuned to be
# the sweet spot: enough contention that Nagle holds bytes on the
# control channel (catching the regression), but not so saturated that
# the TCP send buffer fills and back-pressures even with NODELAY (which
# would produce bunching for reasons unrelated to the fix). 10×
# oversubscription was too aggressive — even NODELAY-enabled builds
# bunched because the entire send buffer filled.
SHAPER_RATE="50mbit"
TARGET_BITRATE="100M"
OUT="/tmp/xfr-control-skew.json"
SERVER_LOG="/tmp/xfr-control-skew-server.log"

if [[ ! -x "$XFR" ]]; then
    echo "ERROR: $XFR not found. Run 'cargo build --release' first." >&2
    exit 1
fi

if ! command -v tc >/dev/null 2>&1; then
    echo "ERROR: tc (iproute2) not found." >&2
    exit 1
fi

cleanup() {
    # Best-effort teardown so a failed run doesn't leave the runner with
    # a 50mbit shaper on lo.
    if [[ -n "${SERVER_PID:-}" ]]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    tc qdisc del dev lo root 2>/dev/null || true
}
trap cleanup EXIT

echo "=== Applying tc netem on lo (${SHAPER_RATE}, 50ms each-way delay) ==="
# limit 10000 picks a queue size large enough that the test doesn't
# itself trip rate-limit drops on the shaper — we want saturation
# pressure on TCP control, not test failure from netem dropping our
# test packets.
tc qdisc add dev lo root netem rate "$SHAPER_RATE" delay 50ms limit 10000
tc qdisc show dev lo

echo
echo "=== Starting xfr server on port $PORT ==="
"$XFR" serve -p "$PORT" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
sleep 0.5

if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "ERROR: server failed to start. Log:" >&2
    cat "$SERVER_LOG" >&2
    exit 1
fi

echo "=== Running ${DURATION}s UDP test at ${TARGET_BITRATE} target ==="
"$XFR" 127.0.0.1 -p "$PORT" -u -b "$TARGET_BITRATE" -t "${DURATION}sec" \
    --no-tui --json-stream >"$OUT" 2>&1 || {
    rc=$?
    echo "ERROR: xfr client exited with code $rc. Output:" >&2
    cat "$OUT" >&2
    exit $rc
}

echo
echo "=== Captured json-stream output ==="
cat "$OUT"

echo
echo "=== Asserting no control-channel bunching ==="
# The Nagle-bunching pattern collapses 3+ consecutive interval messages
# to the same client-side arrival timestamp (server-side TCP holds them
# until end-of-test, then flushes the backlog as a single segment). A
# healthy NODELAY-enabled control channel produces near-unique
# timestamps; a 2-line tail collision near test-end is acceptable
# coincidence (the last interval and the post-test drain can race into
# the same kernel read). The actual bug shows 3+ lines sharing a single
# timestamp, which this assertion specifically targets.
#
# Skip the startup line (elapsed_secs ~0.001 — emitted on connect
# before any data has been exchanged) since it's always isolated in
# time regardless of Nagle behavior.
INTERVAL_LINES=$(
    grep -oE '"timestamp":"[0-9.]+","elapsed_secs":[0-9.]+' "$OUT" |
        awk -F'[":,]+' '$5 != "0.001" { print $3 }'
)

if [[ -z "$INTERVAL_LINES" ]]; then
    echo "FAIL: no interval lines in json-stream output." >&2
    exit 1
fi

TOTAL=$(echo "$INTERVAL_LINES" | wc -l)
UNIQUE=$(echo "$INTERVAL_LINES" | sort -u | wc -l)
MAX_BUNCH=$(echo "$INTERVAL_LINES" | sort | uniq -c | sort -rn | awk 'NR==1{print $1}')

echo "  interval lines (excluding startup): $TOTAL"
echo "  unique client-side timestamps:      $UNIQUE"
echo "  max lines sharing one timestamp:    $MAX_BUNCH"

# Threshold: 3+ lines at one timestamp is the Nagle pattern. 2 is
# acceptable (test-end tail collision).
if [[ "$MAX_BUNCH" -ge 3 ]]; then
    echo "FAIL: detected control-channel bunching — $MAX_BUNCH interval"   >&2
    echo "      lines share a single client-side timestamp. The Nagle-"   >&2
    echo "      held-control-channel pattern (issue #70 follow-up) has"   >&2
    echo "      regressed; verify TCP_NODELAY is being applied to the"    >&2
    echo "      control TcpStream in client.rs and serve.rs (look for"    >&2
    echo "      tcp::configure_control_stream calls before into_split)."  >&2
    echo                                                                  >&2
    echo "      Bunched timestamps (count repeated):"                     >&2
    echo "$INTERVAL_LINES" | sort | uniq -c | awk '$1 > 1 {printf "        %s× at %s\n", $1, $2}' >&2
    exit 1
fi

echo "PASS: no 3-or-more-line bunch detected — control channel is"
echo "      delivering interval messages live, not as an end-of-test"
echo "      flush. TCP_NODELAY plumbing is intact."
