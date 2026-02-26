#!/bin/bash -e

# MPTCP network namespace test
# Original script by Matthieu Baerts (matttbe) — issue #24
# Creates a virtual network with 2 paths:
#   Path 0: 20 Mbps, 10ms delay each way
#   Path 1: 10 Mbps, 20ms delay each way
# Expected aggregate with MPTCP: ~30 Mbps
#
# Usage:
#   sudo ./test-mptcp-ns.sh          # Interactive: runs all tests with human output
#   sudo ./test-mptcp-ns.sh --ci     # CI mode: shorter tests, JSON output, assertions

# set -x

CI=false
if [[ "$1" == "--ci" ]]; then
	CI=true
	shift
fi

XFR="./target/release/xfr"
if [[ ! -x "$XFR" ]]; then
	echo "ERROR: $XFR not found. Run 'cargo build --release' first."
	exit 1
fi

# Check MPTCP support
if ! sysctl -n net.mptcp.enabled >/dev/null 2>&1; then
	echo "SKIP: MPTCP not available in this kernel"
	exit 0
fi
if [[ "$(sysctl -n net.mptcp.enabled)" != "1" ]]; then
	echo "SKIP: MPTCP is disabled (sysctl net.mptcp.enabled=0)"
	exit 0
fi

# Check netem support
if ! modprobe sch_netem 2>/dev/null; then
	echo "SKIP: sch_netem kernel module not available"
	exit 0
fi

export NS=xfr
HOSTS=(cli net0 net1 srv)
NSS=()
FAILED=0

cleanup()
{
	local ns
	for ns in "${NSS[@]}"; do
		ip netns pids "${ns}" 2>/dev/null | xargs -r kill 2>/dev/null
		ip netns del "${ns}" >/dev/null 2>&1
	done
}

trap cleanup EXIT

xfr_srv()
{
	ip netns exec "${NS}_srv" "$XFR" serve &
	sleep .5
}

xfr_cli()
{
	ip netns exec "${NS}_cli" "$XFR" 10.0.0.1 --mptcp --no-tui "$@"
}

xfr_cli_tcp()
{
	ip netns exec "${NS}_cli" "$XFR" 10.0.0.1 --no-tui "$@"
}

# Run a test and extract throughput_mbps from JSON output
# Usage: run_json_test <description> <args...>
# Sets $LAST_MBPS to the throughput value
run_json_test()
{
	local desc="$1"
	shift
	local output
	# JSON goes to stdout, INFO logs go to stderr — capture only stdout
	output=$("$@" --json 2>/dev/null)
	local result
	result=$(echo "$output" | grep -o '"throughput_mbps": *[0-9.]*' | head -1 | sed 's/.*: *//')
	if [[ -z "$result" ]]; then
		echo "FAIL: $desc — no throughput in output"
		echo "  raw output: $output" | head -5
		FAILED=1
		LAST_MBPS=0
		return
	fi
	LAST_MBPS="$result"
	echo "  $desc: ${LAST_MBPS} Mbps"
}

setup()
{
	local suffix
	for suffix in "${HOSTS[@]}"; do
		local ns="${NS}_${suffix}"
		ip netns add "${ns}"
		NSS+=("${ns}")
		ip -n "${ns}" link set lo up
		ip netns exec "${ns}" sysctl -wq net.ipv4.ip_forward=1
	done

	ip link add "cli" netns "${NS}_net0" type veth peer name "net0" netns "${NS}_cli"
	ip link add "cli" netns "${NS}_net1" type veth peer name "net1" netns "${NS}_cli"

	ip link add "srv" netns "${NS}_net0" type veth peer name "net0" netns "${NS}_srv"
	ip link add "srv" netns "${NS}_net1" type veth peer name "net1" netns "${NS}_srv"

	ip -n "${NS}_cli" link set "net0" up
	ip -n "${NS}_cli" addr add dev "net0" "10.0.10.2/24"
	ip -n "${NS}_cli" route add dev "net0" default via "10.0.10.1" metric "100"
	ip -n "${NS}_cli" mptcp endpoint add "10.0.10.2" dev "net0" subflow
	ip -n "${NS}_cli" mptcp limits set add_addr_accepted 2

	ip -n "${NS}_cli" link set "net1" up
	ip -n "${NS}_cli" addr add dev "net1" "10.0.11.2/24"
	ip -n "${NS}_cli" route add dev "net1" "10.0.1.0/24" via "10.0.11.1"
	ip -n "${NS}_cli" mptcp endpoint add "10.0.11.2" dev "net1" subflow

	ip -n "${NS}_srv" link set "net0" up
	ip -n "${NS}_srv" addr add dev "net0" "10.0.0.1/24"
	ip -n "${NS}_srv" route add dev "net0" default via "10.0.0.2" metric "100"
	ip -n "${NS}_srv" mptcp endpoint add "10.0.0.1" dev "net0" signal
	ip netns exec "${NS}_srv" sysctl -wq net.mptcp.allow_join_initial_addr_port=0

	ip -n "${NS}_srv" link set "net1" up
	ip -n "${NS}_srv" addr add dev "net1" "10.0.1.1/24"
	ip -n "${NS}_srv" route add dev "net1" "10.0.11.0/24" via "10.0.1.2"
	ip -n "${NS}_srv" mptcp endpoint add "10.0.1.1" dev "net1" signal

	ip -n "${NS}_net0" link set "cli" up
	ip -n "${NS}_net0" addr add dev "cli" "10.0.10.1/24"
	tc -n "${NS}_net0" qdisc add dev "cli" root netem rate 20mbit delay 10ms limit 100
	ip -n "${NS}_net0" link set "srv" up
	ip -n "${NS}_net0" addr add dev "srv" "10.0.0.2/24"
	tc -n "${NS}_net0" qdisc add dev "srv" root netem rate 20mbit delay 10ms limit 100

	ip -n "${NS}_net1" link set "cli" up
	ip -n "${NS}_net1" addr add dev "cli" "10.0.11.1/24"
	tc -n "${NS}_net1" qdisc add dev "cli" root netem rate 10mbit delay 20ms limit 100
	ip -n "${NS}_net1" link set "srv" up
	ip -n "${NS}_net1" addr add dev "srv" "10.0.1.2/24"
	tc -n "${NS}_net1" qdisc add dev "srv" root netem rate 10mbit delay 20ms limit 100
}

# Assert that $1 > $2 (floating point comparison)
assert_gt()
{
	local actual="$1" threshold="$2" label="$3"
	if ! echo "$actual > $threshold" | bc -l | grep -q '^1'; then
		echo "FAIL: $label — expected > ${threshold} Mbps, got ${actual} Mbps"
		FAILED=1
	else
		echo "  PASS: $label (${actual} > ${threshold} Mbps)"
	fi
}

setup "${@}"
xfr_srv

if [[ "$CI" == "true" ]]; then
	# CI mode: shorter tests, JSON output, throughput assertions
	DURATION="5sec"

	echo "=== MPTCP CI Tests ==="
	echo ""

	echo "--- TCP baseline (single path, expect ~20 Mbps) ---"
	run_json_test "TCP upload" xfr_cli_tcp -t "$DURATION"
	TCP_MBPS="$LAST_MBPS"
	assert_gt "$TCP_MBPS" 10 "TCP throughput > 10 Mbps"

	echo ""
	echo "--- MPTCP upload (both paths, expect ~30 Mbps) ---"
	run_json_test "MPTCP upload" xfr_cli -t "$DURATION"
	MPTCP_MBPS="$LAST_MBPS"
	assert_gt "$MPTCP_MBPS" "$TCP_MBPS" "MPTCP throughput > TCP throughput"
	assert_gt "$MPTCP_MBPS" 20 "MPTCP throughput > 20 Mbps (proves multi-path)"

	echo ""
	echo "--- MPTCP reverse (download, expect ~30 Mbps) ---"
	run_json_test "MPTCP download" xfr_cli -t "$DURATION" -R
	assert_gt "$LAST_MBPS" 20 "MPTCP download > 20 Mbps"

	echo ""
	echo "--- MPTCP multi-stream (expect ~30 Mbps) ---"
	run_json_test "MPTCP 4-stream" xfr_cli -t "$DURATION" -P 4
	assert_gt "$LAST_MBPS" 20 "MPTCP multi-stream > 20 Mbps"

	echo ""
	if [[ "$FAILED" -eq 0 ]]; then
		echo "=== All MPTCP tests passed ==="
	else
		echo "=== MPTCP tests FAILED ==="
		exit 1
	fi
else
	# Interactive mode: longer tests, human-readable output
	echo ""
	echo "=== Test 1: TCP (single path, expect ~20 Mbps) ==="
	xfr_cli_tcp -t 10sec

	echo ""
	echo "=== Test 2: MPTCP (both paths, expect ~30 Mbps) ==="
	xfr_cli -t 10sec

	echo ""
	echo "=== Test 3: MPTCP reverse (download, expect ~30 Mbps) ==="
	xfr_cli -t 10sec -R

	echo ""
	echo "=== Test 4: MPTCP multi-stream (expect ~30 Mbps) ==="
	xfr_cli -t 10sec -P 4
fi
