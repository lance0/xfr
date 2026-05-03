# Known Issues

This document tracks known limitations and edge cases that are documented but not yet addressed.

## Low Priority / Edge Cases

### Timestamp Overflow for Very Long Tests

**Issue:** `elapsed.as_millis()` returns `u128` but is cast to `u64` in interval reporting.

**Impact:** Only affects tests longer than ~584 million years. No practical impact.

**Mitigation:** None needed.

---

### UDP Payload Size Hardcoded to 1400 Bytes

**Issue:** UDP payload size is hardcoded to 1400 bytes, leaving headroom for IP/UDP headers and tunneling overhead.

**Impact:** May not be optimal for jumbo frames or networks with different MTUs.

**Workaround:** Works correctly for the vast majority of networks.

---

### Bitrate Division Truncation

**Issue:** When calculating packets per second from bitrate, integer division may cause minor precision loss.

**Impact:** Actual bitrate may be slightly lower than requested (< 0.1% difference).

**Workaround:** None needed for typical use cases.

---

### High Stream Counts Without Fair Queuing

**Issue:** Very high stream counts (`64+`) over bandwidth-limited links with shallow non-fair queues can still drop enough handshake/data packets to kill some TCP streams.

**Impact:** You may see zero-byte streams, retransmit spikes, or mid-test `Broken pipe` errors in synthetic setups that use bare `netem rate ... limit ...` without fair queuing.

**Workaround:** Use fair queuing such as `fq_codel`, reduce stream count, or increase queue depth appropriately. This is primarily a queueing/test-harness limitation, not specific to xfr; most modern Linux distributions already default to `fq_codel`.

---

### QUIC Client DualStack Binding

**Issue:** QUIC client endpoint in DualStack mode binds to IPv4 (`0.0.0.0:0`) by default, which may not work when connecting to an IPv6-only server.

**Impact:** Edge case - only affects DualStack clients connecting to IPv6-only QUIC servers.

**Workaround:** Use `-6` flag to force IPv6 mode when connecting to IPv6-only servers.

---

### QUIC Accept Loop Timeout

**Issue:** If a QUIC connection is accepted but the client never opens a stream, the server waits indefinitely in `accept_bi()`.

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

### UDP `Connection refused` Storms at >=50% Loss with Short Tests

**Issue:** Under tc netem `loss 50%` (or higher) on lo with short test durations (`-t 10sec`), the client's connected UDP socket can get stuck returning `ECONNREFUSED` on every send. The mechanism: TCP control handshake under heavy loss takes several seconds (retransmits), eating into test duration; if the server's test-end fires before the client's first UDP packet reaches the server's data port, the kernel returns ICMP port-unreachable, which sticks on the connected socket and rejects all subsequent sends.

**Impact:** A run that should report ~50% loss instead emits a flood of `Connection refused (os error 111)` warnings. Sometimes the test still completes correctly via the TCP Result message; sometimes it ends with `Timeout waiting for server response`. Pre-existing behavior — reproduces on v0.9.13 at the same or higher rate than v0.9.14.

**Workaround:** Use longer test durations (`-t 60sec` or more) so the handshake delay is amortized. The issue does not occur in real-world conditions where tc netem isn't applied to TCP control as well as UDP data.

---

### UDP Download-Mode Live Loss Still Routed Through TCP Control

**Issue:** v0.9.14's `udp_feedback_v1` capability surfaces live UDP loss over UDP for upload-mode tests, sidestepping the TCP control channel. Download mode (`-R`) and the receive half of bidir still rely on TCP control `Interval` messages for live loss visibility. The client is the receiver in download mode and has authoritative receive-side stats locally, but those stats are not plumbed to consumers (TUI/plain/JSON-stream).

**Impact:** Under a saturated download path with TCP control back-pressure, download-mode live loss can lag the actual receive-side state. The end-of-test summary remains correct.

**Mitigation:** Use upload mode for the most responsive live-loss reporting. A follow-up will plumb client-side recv stats directly to consumers (tracked in ROADMAP.md).

---

### Non-TUI Interval Row Cadence Can Still Bunch Under Extreme Loss

**Issue:** v0.9.14's upload-mode UDP feedback path keeps the TUI live Packet Loss counter current and refreshes the cumulative loss cache used by plain text, CSV, and JSON-stream output. Those non-TUI formats still print rows only when a TCP control `Interval` message reaches the client. Under aggressive synthetic loss, the kernel can still deliver already-sent TCP interval messages in bursts.

**Impact:** The `lost` value on each printed non-TUI row reflects the freshest accepted cumulative UDP reading, but the row timestamps/cadence can still bunch under pathological loss. This is most visible with `--json-stream` diagnostics.

**Mitigation:** Use the TUI for the most responsive live Packet Loss display. A follow-up will either emit explicit feedback-only stream events or decouple scripted interval output cadence from TCP control-message arrival.

---

### Windows Support is Experimental

**Issue:** Windows is not a first-class platform. TCP_INFO statistics are not available (returns zeros), and some socket options may behave differently.

**Impact:** Basic TCP/UDP/QUIC testing works, but advanced metrics are missing.

**Workaround:** Use WSL2 for full functionality on Windows. Native Windows binaries are not provided.

---

## By Design

### QUIC Bitrate Limiting Not Implemented

QUIC mode ignores the `-b/--bitrate` flag. Pacing support may be added in a future release.

A warning is logged when `-b` is used with QUIC.

### QUIC Server Certificate Not Verified

QUIC transport uses self-signed certificates and does not verify the server's identity. This is intentional for ease of use in trusted environments.

**Impact:** QUIC connections are encrypted but not authenticated without PSK, leaving them vulnerable to man-in-the-middle attacks on untrusted networks.

**Mitigation:** Always use `--psk` for QUIC on untrusted networks. PSK provides mutual authentication via HMAC-SHA256 challenge-response.

### Protocol Extensions Require Major Version Bump

The control protocol uses a tagged JSON enum for message types. Unknown fields within known message types are silently ignored (serde default), but unrecognized message *types* cause parse errors. Adding new message types requires a protocol version bump.

**Rationale:** This favors simplicity over complex version negotiation. The version handshake and client capabilities negotiation ensure compatibility.

### UDP Data Plane Unauthenticated

In UDP reverse/bidir mode, the server uses the source address of the first received packet to determine where to send data. Without PSK authentication, a spoofed packet could redirect traffic to a third party.

**Impact:** Potential reflection/amplification attack if server is exposed to untrusted networks.

**Mitigation:** Use `--psk` authentication on untrusted networks. The control-plane PSK challenge validates client identity before data transfer begins.

### IPv6 Zone IDs Not Supported

IPv6 link-local addresses with zone IDs (e.g., `fe80::1%eth0`) are not supported in socket addresses.

**Impact:** Cannot bind to or connect to link-local addresses that require zone specification.

**Workaround:** Use global or unique-local IPv6 addresses instead.

---

## Previously Known Issues (Resolved)

The following issues have been fixed and are listed here for reference.

- **One-off mode deadlock** - `--one-off` previously blocked the accept loop waiting for test completion. Now uses a `tokio::sync::watch` shutdown channel to signal exit after the test finishes. Both TCP and QUIC accept loops respond to the shutdown signal.
- **IPv4-mapped IPv6 comparison** - DataHello IP validation previously failed on dual-stack systems where control and data connections used different address representations (`::ffff:x.x.x.x` vs `x.x.x.x`). Fixed by `normalize_ip()` which converts IPv4-mapped IPv6 addresses to their IPv4 form before comparison.
- **DataHello IP validation** - The server now validates that DataHello connections originate from the same IP as the control connection, preventing connection hijacking.
- **Slow-loris protection** - The accept loop now spawns per-connection tasks immediately, with a 5-second initial read timeout (`INITIAL_READ_TIMEOUT`). Slow clients can no longer block the listener or other connections.
- **DataHello flood protection** - The server validates that the `test_id` in a DataHello message corresponds to an active test before processing, rejecting unknown test IDs immediately.
- **cancel.changed() busy-loop** - The stream collection `select!` loop now handles the sender-dropped error from `cancel.changed()` instead of spinning on `Err`, preventing CPU spin when the cancel sender is dropped.
- **TCP bitrate limiting** - TCP bitrate pacing (`-b` for TCP) was added in v0.6.1 using a byte-budget sleep approach with interruptible sleeps and buffer auto-capping.
- **Client capabilities negotiation** - Client and server exchange capabilities in the Hello handshake, allowing the server to adapt behavior (e.g., single-port vs multi-port TCP) based on client support.
- **QUIC one-off mode** - QUIC accept loop now responds to the shutdown signal for proper `--one-off` exit after a single test completes.

---

## Future Improvements

Some issues listed here may be addressed in future releases. See the [ROADMAP.md](ROADMAP.md) "Known Limitations" section for items under consideration.

---

## Reporting Issues

Found a bug not listed here? Please report it at: https://github.com/lance0/xfr/issues
