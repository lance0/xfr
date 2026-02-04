# Known Issues

This document tracks known limitations and edge cases that are documented but not yet addressed.

## Low Priority / Edge Cases

### Timestamp Overflow for Very Long Tests

**Issue:** `elapsed.as_millis()` returns `u128` but is cast to `u64` in interval reporting.

**Impact:** Only affects tests longer than ~584 million years. No practical impact.

**Mitigation:** None needed.

---

### UDP MTU Hardcoded to 1472 Bytes

**Issue:** UDP packet size is hardcoded assuming standard Ethernet MTU (1500 - 20 IP - 8 UDP = 1472).

**Impact:** May not be optimal for jumbo frames or networks with different MTUs.

**Workaround:** Works correctly for the vast majority of networks.

---

### Bitrate Division Truncation

**Issue:** When calculating packets per second from bitrate, integer division may cause minor precision loss.

**Impact:** Actual bitrate may be slightly lower than requested (< 0.1% difference).

**Workaround:** None needed for typical use cases.

---

### QUIC Client DualStack Binding

**Issue:** QUIC client endpoint in DualStack mode binds to IPv4 (`0.0.0.0:0`) by default, which may not work when connecting to an IPv6-only server.

**Impact:** Edge case - only affects DualStack clients connecting to IPv6-only QUIC servers.

**Workaround:** Use `-6` flag to force IPv6 mode when connecting to IPv6-only servers.

---

### QUIC Accept Loop Timeout

**Issue:** If a QUIC connection is accepted but the client never opens a stream, the server waits indefinitely in `accept_uni()`.

**Impact:** Low probability - requires malicious client holding connections.

**Mitigation:** Handshake timeout covers most DoS scenarios. Connection eventually times out at QUIC layer.

---

### Settings Modal Doesn't Apply Changes

**Issue:** The TUI settings modal shows options but doesn't apply changes mid-test.

**Impact:** UI shows "restart required" - this is by design for current release.

**Workaround:** Restart test with new settings via CLI flags.

---

### UDP Reverse Mode Error Handling

**Issue:** In UDP reverse (download) mode, send errors on the server side are logged but not reported back to the client.

**Impact:** Client may see lower throughput without explicit error indication.

**Workaround:** Check server logs if UDP reverse shows unexpected results.

---

### Rate Limiter Slowloris Vulnerability

**Issue:** A malicious client could hold open a test slot by connecting but sending data very slowly.

**Impact:** Could exhaust rate limit slots, denying service to legitimate clients.

**Mitigation:**
- Handshake timeout (30s) limits connection phase
- Test duration limits via `--max-duration`
- Semaphore prevents unbounded connections
- For public servers, consider firewall-level rate limiting

---

### Windows Support is Experimental

**Issue:** Windows is not a first-class platform. TCP_INFO statistics are not available (returns zeros), and some socket options may behave differently.

**Impact:** Basic TCP/UDP/QUIC testing works, but advanced metrics are missing.

**Workaround:** Use WSL2 for full functionality on Windows. Native Windows binaries are not provided.

---

## By Design

### TCP Bitrate Limiting Not Implemented

TCP mode ignores the `-b/--bitrate` flag. This is intentional - TCP should run at maximum sustainable rate.

A warning is logged when `-b` is used with TCP.

### QUIC Bitrate Limiting Not Implemented

QUIC mode ignores the `-b/--bitrate` flag. Pacing support may be added in a future release.

A warning is logged when `-b` is used with QUIC.

### QUIC Server Certificate Not Verified

QUIC transport uses self-signed certificates and does not verify the server's identity. This is intentional for ease of use in trusted environments.

**Impact:** QUIC connections are encrypted but not authenticated without PSK, leaving them vulnerable to man-in-the-middle attacks on untrusted networks.

**Mitigation:** Always use `--psk` for QUIC on untrusted networks. PSK provides mutual authentication via HMAC-SHA256 challenge-response.

### Protocol Extensions Require Major Version Bump

The control protocol uses strict JSON deserialization. Unrecognized message types or fields cause parse errors, meaning protocol extensions require a major version bump.

**Rationale:** This favors simplicity over complex version negotiation. The version handshake ensures compatibility.

---

## Future Improvements

Some issues listed here may be addressed in future releases. See the [ROADMAP.md](ROADMAP.md) "Known Limitations" section for items under consideration.

---

## Reporting Issues

Found a bug not listed here? Please report it at: https://github.com/lance0/xfr/issues
