#!/bin/bash
# Local Docker reproduction of issue #70 — live UDP loss counter stuck
# at 0% during upload-mode saturation, jumps to actual value at end.
#
# Recipe matches brettowe's real-world setup as closely as a loopback
# shaper allows: 100 Mbps cap, 50 ms one-way delay, 1 Gbps target
# (10× oversubscription). The asymmetry is what reveals the bug —
# upload-mode under saturation, with the TCP control channel competing
# for ACKs against the UDP uplink, would bunch interval messages at
# end-of-test in v0.9.13 and earlier.
#
# Usage (inside the xfr-repro Docker image):
#   /usr/local/bin/repro-issue-70.sh             # new build (hard assertions)
#   /usr/local/bin/repro-issue-70.sh --baseline  # v0.9.13 (diagnostic only)
#
# The new-build assertions are the contract: live loss must appear
# during the run AND no client-side timestamp may collapse 3+ interval
# lines onto it. The --baseline mode is diagnostic — it prints the
# bunching count and time-to-first-nonzero-loss for narrative evidence
# but does not fail on a threshold (Docker host networking and qdisc
# behavior vary too much for that).
#
# Requires CAP_NET_ADMIN inside the container to manage tc qdiscs on lo.
# `docker run --cap-add=NET_ADMIN` is the supported invocation.

set -euo pipefail

MODE="new"
if [[ "${1:-}" == "--baseline" ]]; then
    MODE="baseline"
    shift
fi

case "$MODE" in
    new)
        XFR_BIN="/usr/local/bin/xfr"
        LABEL="new"
        ;;
    baseline)
        XFR_BIN="/usr/local/bin/xfr-baseline"
        LABEL="baseline (v0.9.13)"
        ;;
esac

PORT=5311
DURATION=30
SHAPER_RATE="100mbit"
SHAPER_DELAY="50ms"
TARGET_BITRATE="1G"
OUT="/tmp/xfr-issue-70.json"
SERVER_LOG="/tmp/xfr-issue-70-server.log"

echo "=== xfr issue #70 reproduction ==="
echo "Mode:      $LABEL"
echo "Binary:    $XFR_BIN"
echo "Recipe:    tc netem rate $SHAPER_RATE delay $SHAPER_DELAY on lo"
echo "Target:    $TARGET_BITRATE for ${DURATION}s ($([ "$TARGET_BITRATE" = "1G" ] && echo "10× oversubscription"))"
echo

cleanup() {
    if [[ -n "${SERVER_PID:-}" ]]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    tc qdisc del dev lo root 2>/dev/null || true
}
trap cleanup EXIT

echo "=== Applying tc netem on lo ==="
tc qdisc del dev lo root 2>/dev/null || true
tc qdisc add dev lo root netem rate "$SHAPER_RATE" delay "$SHAPER_DELAY" limit 10000
tc qdisc show dev lo

echo
echo "=== Starting xfr server on port $PORT ==="
"$XFR_BIN" serve -p "$PORT" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
sleep 0.5
if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "ERROR: server failed to start. Log:" >&2
    cat "$SERVER_LOG" >&2
    exit 1
fi

echo "=== Running ${DURATION}s UDP test at ${TARGET_BITRATE} target ==="
"$XFR_BIN" 127.0.0.1 -p "$PORT" -u -b "$TARGET_BITRATE" -t "${DURATION}sec" \
    --no-tui --json-stream --timestamp-format relative >"$OUT" 2>&1 || {
    rc=$?
    echo "ERROR: xfr client exited with code $rc. Output:" >&2
    cat "$OUT" >&2
    exit "$rc"
}

echo
echo "=== Captured json-stream output ($(wc -l <"$OUT") lines) ==="
cat "$OUT"

echo
echo "=== Analysis ==="

# Extract per-interval timestamp + elapsed_secs + per-interval lost
# count (from the streams[].lost field; aggregate from per-stream).
# Skip the startup line (elapsed_secs near 0).
INTERVAL_TIMESTAMPS=$(
    grep -oE '"timestamp":"[0-9.]+","elapsed_secs":[0-9.]+' "$OUT" |
        awk -F'[":,]+' '($5 + 0) >= 0.5 { print $3 }' || true
)

if [[ -z "$INTERVAL_TIMESTAMPS" ]]; then
    echo "ERROR: no interval lines in json-stream output." >&2
    exit 1
fi

TOTAL=$(echo "$INTERVAL_TIMESTAMPS" | wc -l)
UNIQUE=$(echo "$INTERVAL_TIMESTAMPS" | sort -u | wc -l)
MAX_BUNCH=$(echo "$INTERVAL_TIMESTAMPS" | sort | uniq -c | sort -rn | awk 'NR==1{print $1}')

# Time-to-first-nonzero-loss: the elapsed_secs of the earliest interval
# line whose `lost` field exceeds 0. The TCP-control udp_progress path
# emits aggregate.lost at server interval cadence; under bunching the
# whole nonzero-loss series collapses to end-of-test.
FIRST_LOSS=$(grep -oE '"elapsed_secs":[0-9.]+,[^}]*"lost":[1-9][0-9]*' "$OUT" |
    awk -F'[":,]+' 'NR==1 {print $3}' || true)

echo "  interval lines (excluding startup): $TOTAL"
echo "  unique client-side timestamps:      $UNIQUE"
echo "  max lines sharing one timestamp:    $MAX_BUNCH"
echo "  time-to-first-nonzero-loss:         ${FIRST_LOSS:-(no loss observed)}"

if [[ "$MODE" == "new" ]]; then
    echo
    FAIL=0
    if [[ "$MAX_BUNCH" -ge 3 ]]; then
        echo "FAIL: detected control-channel bunching — $MAX_BUNCH interval lines"
        echo "      share a single client-side timestamp. The new-build contract"
        echo "      is max bunch ≤ 2; this branch has regressed."
        FAIL=1
    fi
    if [[ -z "$FIRST_LOSS" ]]; then
        echo "FAIL: no nonzero loss observed at any interval."
        echo "      Either the recipe didn't actually saturate (host-dependent)"
        echo "      or live loss isn't being surfaced."
        FAIL=1
    elif awk -v fl="$FIRST_LOSS" -v d="$DURATION" 'BEGIN { exit !(fl >= d - 2) }'; then
        echo "FAIL: first-nonzero-loss at ${FIRST_LOSS}s is too close to test"
        echo "      end (${DURATION}s). Live loss must appear MID-RUN, not just"
        echo "      flushed at the end — that's the bunching pattern this fix"
        echo "      addresses."
        FAIL=1
    fi
    if [[ "$FAIL" -eq 1 ]]; then
        exit 1
    fi
    echo "PASS: max-bunch ≤ 2 AND live loss appears mid-run."
    echo "      Server-side TCP_NODELAY (v0.9.13), unconditional Skip on the"
    echo "      interval timer, and udp_feedback_v1 are doing their jobs."
else
    echo
    echo "DIAG: baseline mode is informational. Expected pre-fix behavior is"
    echo "      max bunch >= 3 and first-nonzero-loss near ${DURATION}s."
    echo "      Variance is normal — Docker host networking and qdisc"
    echo "      behavior aren't deterministic enough to gate on a threshold."
fi
