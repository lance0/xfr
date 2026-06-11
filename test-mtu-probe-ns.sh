#!/bin/bash -e

# MTU probe network namespace test (issue #64)
# Builds a 3-namespace path with a constrained middle hop:
#
#   cli ---(veth, MTU 9000)--- mid ---(veth, MTU 1500)--- srv
#
# The client's own interface is jumbo, so oversized probes leave the
# client fine and die at the middle hop (or, once the kernel caches the
# ICMP frag-needed PMTU, get EMSGSIZE locally — both verdicts must
# bracket the search identically). Expected convergence: 1472-byte
# payload, 1500-byte path MTU, in both directions.
#
# Usage:
#   sudo ./test-mtu-probe-ns.sh          # human output
#   sudo ./test-mtu-probe-ns.sh --ci     # JSON + assertions

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

export NS=xfrmtu
HOSTS=(cli mid srv)
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

	ip link add "mid" netns "${NS}_cli" type veth peer name "cli" netns "${NS}_mid"
	ip link add "srv" netns "${NS}_mid" type veth peer name "mid" netns "${NS}_srv"

	# Client side: jumbo, so oversized probes are not refused locally
	# at first — they must die in the path.
	ip -n "${NS}_cli" link set "mid" mtu 9000 up
	ip -n "${NS}_cli" addr add dev "mid" "10.0.20.2/24"
	ip -n "${NS}_cli" route add default via "10.0.20.1"

	ip -n "${NS}_mid" link set "cli" mtu 9000 up
	ip -n "${NS}_mid" addr add dev "cli" "10.0.20.1/24"

	# Constrained hop: standard Ethernet MTU.
	ip -n "${NS}_mid" link set "srv" mtu 1500 up
	ip -n "${NS}_mid" addr add dev "srv" "10.0.21.1/24"

	ip -n "${NS}_srv" link set "mid" mtu 1500 up
	ip -n "${NS}_srv" addr add dev "mid" "10.0.21.2/24"
	ip -n "${NS}_srv" route add default via "10.0.21.1"
}

assert_eq()
{
	local actual="$1" expected="$2" label="$3"
	if [[ "$actual" != "$expected" ]]; then
		echo "FAIL: $label — expected ${expected}, got ${actual:-<missing>}"
		FAILED=1
	else
		echo "  PASS: $label (${actual})"
	fi
}

setup

ip netns exec "${NS}_srv" "$XFR" serve &
sleep .5

echo "=== MTU probe through a 1500-byte middle hop (expect payload 1472) ==="
OUTPUT=$(ip netns exec "${NS}_cli" "$XFR" 10.0.21.2 --probe-mtu --json 2>/dev/null)

FWD=$(echo "$OUTPUT" | grep -o '"forward_max_payload": *[0-9]*' | head -1 | grep -o '[0-9]*$')
REV=$(echo "$OUTPUT" | grep -o '"reverse_max_payload": *[0-9]*' | head -1 | grep -o '[0-9]*$')
FWD_MTU=$(echo "$OUTPUT" | grep -o '"forward_path_mtu": *[0-9]*' | head -1 | grep -o '[0-9]*$')

if [[ -z "$FWD" ]]; then
	echo "FAIL: no mtu_probe report in output"
	echo "$OUTPUT" | head -10
	FAILED=1
else
	assert_eq "$FWD" 1472 "client→server max payload"
	assert_eq "$REV" 1472 "server→client max payload"
	assert_eq "$FWD_MTU" 1500 "derived path MTU"
fi

if [[ "$FAILED" -eq 0 ]]; then
	echo "=== MTU probe test passed ==="
	exit 0
else
	echo "=== MTU probe test FAILED ==="
	[[ "$CI" == "true" ]] && exit 1
	exit 1
fi
