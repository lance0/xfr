#!/usr/bin/env bash
# Verify actual systemd credentials and unsafe-file controls without printing keys.
set -euo pipefail

binary=$(realpath "${1:-target/debug/xfr}")
for command in systemd-run systemd-creds getfacl setfacl python3; do
    command -v "$command" >/dev/null
done
sudo -n true
work=$(sudo mktemp -d /run/xfr-credential-smoke.XXXXXX)
unit=""
unit_pid=""
cleanup() {
    if [[ -n "$unit" ]]; then
        sudo systemctl stop "$unit" >/dev/null 2>&1 || true
    fi
    if [[ -n "$unit_pid" ]]; then
        wait "$unit_pid" 2>/dev/null || true
    fi
    if [[ "$work" =~ ^/run/xfr-credential-smoke\.[[:alnum:]]{6}$ ]]; then
        sudo rm -rf -- "$work"
    else
        echo 'refusing to remove an unexpected credential smoke path' >&2
    fi
}
trap cleanup EXIT
trap 'cat "$work/service.log" 2>/dev/null || true' ERR
sudo chown "$(id -u):$(id -g)" "$work"
chmod 755 "$work"
install -m 755 "$binary" "$work/xfr"
printf '%s\n' 'systemd-credential-smoke-key' > "$work/key"
chmod 600 "$work/key"

expect_permission_failure() {
    if CREDENTIALS_DIRECTORY="$work" "$work/xfr" 127.0.0.1 -p 1 \
        --no-update-check --json --psk-file "$1" > "$work/control.log" 2>&1; then
        echo "unsafe PSK unexpectedly accepted: $1" >&2
        exit 1
    fi
    grep -q 'overly broad permissions' "$work/control.log"
}

for mode in 440 644; do
    chmod "$mode" "$work/key"
    expect_permission_failure "$work/key"
done
ln -s "$work/key" "$work/unsafe-link"
expect_permission_failure "$work/unsafe-link"
chmod 600 "$work/key"

# This also exercises the exact ACL branch when the credential fs uses 0400 fallback.
sudo install -m 400 "$work/key" "$work/root-acl"
sudo setfacl -m "u:$(id -u):r--" "$work/root-acl"
[[ $(stat -c '%u:%a' "$work/root-acl") == 0:440 ]]
getfacl -n -c "$work/root-acl"
if "$work/xfr" 127.0.0.1 -p 1 --no-update-check --json \
    --psk-file "$work/root-acl" > "$work/control.log" 2>&1; then
    echo 'closed control port unexpectedly connected' >&2
    exit 1
fi
grep -q 'Connection refused' "$work/control.log"
sudo setfacl -m u:65534:r-- "$work/root-acl"
expect_permission_failure "$work/root-acl"
sudo setfacl -x u:65534 "$work/root-acl"
sudo setfacl -m m::rw- "$work/root-acl"
expect_permission_failure "$work/root-acl"

cat > "$work/run-server" <<'SERVER'
#!/bin/sh
set -eu
stat -c 'credential owner=%u mode=%a' "$CREDENTIALS_DIRECTORY/psk"
getfacl -n -c "$CREDENTIALS_DIRECTORY/psk"
exec "$1" serve --bind 127.0.0.1 -p "$2" --one-off --no-mdns \
    --psk-file "$CREDENTIALS_DIRECTORY/psk"
SERVER
chmod 755 "$work/run-server"

for mode in plain encrypted; do
    property=LoadCredential
    credential="$work/key"
    if [[ "$mode" == encrypted ]]; then
        property=LoadCredentialEncrypted
        sudo systemd-creds encrypt --with-key=host --name=psk \
            "$work/key" "$work/encrypted-key"
        credential="$work/encrypted-key"
    fi
    port=$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    print(sock.getsockname()[1])
PY
)
    unit="xfr-credential-smoke-$$-$mode"
    {
        sudo systemd-run --quiet --wait --pipe --collect --unit="$unit" \
            --property=DynamicUser=yes \
            --property=RuntimeMaxSec=30 --property=TimeoutStopSec=5 \
            --property="$property=psk:$credential" \
            "$work/run-server" "$work/xfr" "$port"
    } > "$work/service.log" 2>&1 &
    unit_pid=$!
    python3 - "$port" <<'PY'
import socket
import sys
import time
port = int(sys.argv[1])
deadline = time.monotonic() + 15
while True:
    try:
        with socket.create_connection(('127.0.0.1', port), timeout=.1):
            break
    except OSError:
        if time.monotonic() >= deadline:
            raise RuntimeError('systemd xfr listener did not start')
        time.sleep(.1)
PY
    "$work/xfr" 127.0.0.1 -p "$port" -t 1 -b 1M --json --no-update-check \
        --psk-file "$work/key" > "$work/result.json"
    wait "$unit_pid"
    unit_pid=""
    cat "$work/service.log"
    python3 - "$work/result.json" <<'PY'
import json
import sys
with open(sys.argv[1]) as result_file:
    result = json.load(result_file)
assert result['bytes_total'] > 0, result
PY
    echo "PASS DynamicUser $property authenticated test"
    unit=""
done
