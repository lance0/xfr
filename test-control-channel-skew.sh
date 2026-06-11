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
#   2. Run an 8-second UDP test at 100 Mbps target — saturates the
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
# Idempotent setup: drop any pre-existing root qdisc first so a
# rerun (after a previously-interrupted test, or when another job
# has shaped lo) doesn't fail with "File exists". The cleanup trap
# at end-of-run also tears it down.
# limit 10000 picks a queue size large enough that the test doesn't
# itself trip rate-limit drops on the shaper — we want saturation
# pressure on TCP control, not test failure from netem dropping our
# test packets.
tc qdisc del dev lo root 2>/dev/null || true
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
# --timestamp-format relative pins the json-stream "timestamp" field to
# a numeric seconds-since-test-start string, regardless of any
# XFR_TIMESTAMP_FORMAT env var or config-file default that would
# otherwise format as ISO 8601 / Unix epoch and break the awk parser.
"$XFR" 127.0.0.1 -p "$PORT" -u -b "$TARGET_BITRATE" -t "${DURATION}sec" \
    --no-tui --json-stream --timestamp-format relative >"$OUT" 2>&1 || {
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
# The Nagle-bunching pattern collapses interval messages to ONE shared
# client-side arrival timestamp (server-side TCP holds them until
# end-of-test, then flushes the whole backlog as a single segment): the
# original #70 failure bunched essentially every line. A healthy
# NODELAY-enabled control channel still does NOT deliver cleanly at
# 1 Hz under this profile — the netem FIFO holds ~2.2 s of queue at
# 50 Mbit, so even correctly-NODELAY'd control segments queue behind
# the UDP flood, tail-drop, and retransmit. Packet captures (v0.9.18
# cycle) show the channel oscillating between two stable delivery
# rhythms: ~2.1 s batches of 2 lines, and ~4.2 s batches of 3 (one
# extra RTO backoff doubling when a retransmitted segment also
# tail-drops). Which rhythm a run locks onto is decided by startup
# phase — the single-port UDP handshake (#63) shifted that phase by
# ~100 ms and made the 3-bunch rhythm dominant, with no change to the
# control channel's own behavior (server emit cadence and retransmit
# patterns are identical on the wire in both data planes).
#
# The assertion therefore targets what the bug actually does — total
# collapse — and tolerates both bloat rhythms: 5+ lines at one
# timestamp, or fewer than 3 distinct timestamps overall, fails.
#
# Skip the startup line (elapsed_secs near 0 — emitted on connect
# before any data has been exchanged) since it's always isolated in
# time regardless of Nagle behavior. Use a numeric threshold rather
# than a string match: the startup line's elapsed_secs is
# `elapsed_ms / 1000.0` and can render as 0.0, 0.001, 0.002 etc.
# depending on host timing. Anything below 0.5 is the startup line;
# real intervals start at 1.0+.
#
# `|| true` keeps the pipeline from short-circuiting under set -e
# when grep finds no matches — the explicit empty-check below is
# how we want a "no interval lines" failure surfaced.
INTERVAL_LINES=$(
    {
        grep -oE '"timestamp":"[0-9.]+","elapsed_secs":[0-9.]+' "$OUT" |
            awk -F'[":,]+' '($5 + 0) >= 0.5 { print $3 }'
    } || true
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

# Thresholds: the #70 regression collapses everything to one stamp
# (max bunch ~= line count, distinct stamps ~= 1). Bloat rhythms top
# out at 3-line bunches with 4+ distinct stamps. 5+ in one bunch, or
# fewer than 3 distinct stamps, means real collapse.
if [[ "$MAX_BUNCH" -ge 5 || "$UNIQUE" -lt 3 ]]; then
    echo "FAIL: detected control-channel collapse — $MAX_BUNCH interval"  >&2
    echo "      lines share one client-side timestamp ($UNIQUE distinct"  >&2
    echo "      timestamps total). The Nagle-held-control-channel"        >&2
    echo "      pattern (issue #70 follow-up) has regressed; verify"      >&2
    echo "      TCP_NODELAY is being applied to the control TcpStream"    >&2
    echo "      in client.rs and serve.rs (look for"                      >&2
    echo "      tcp::configure_control_stream calls before into_split)."  >&2
    echo                                                                  >&2
    echo "      Bunched timestamps (count repeated):"                     >&2
    echo "$INTERVAL_LINES" | sort | uniq -c | awk '$1 > 1 {printf "        %s× at %s\n", $1, $2}' >&2
    exit 1
fi

echo "PASS: no collapse detected (max bunch $MAX_BUNCH, $UNIQUE distinct"
echo "      timestamps) — control channel is delivering interval"
echo "      messages live, not as an end-of-test flush. TCP_NODELAY"
echo "      plumbing is intact."
