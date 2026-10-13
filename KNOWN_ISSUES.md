# Known Issues

This document tracks known limitations and edge cases that are documented but not yet addressed.

## Low Priority / Edge Cases

### Timestamp Overflow for Very Long Tests

**Issue:** `elapsed.as_millis()` returns `u128` but is cast to `u64` in interval reporting.

**Impact:** Only affects tests longer than ~584 million years. No practical impact.

**Mitigation:** None needed.

---

### UDP Payload Size Requires Manual Selection

**Issue:** UDP defaults to a 1400-byte payload and does not automatically adapt to path MTU. `-l/--length` selects the complete UDP payload, including the 16-byte xfr header (valid range: 16–65507 bytes). Explicit lengths require a server advertising `udp_payload_size_v1`.

**Impact:** The default can fragment on paths with a smaller MTU, including some tunnels. Larger payloads can also fragment if they exceed the path's available space after IP/UDP headers.

**Workaround:** Choose a length that fits the path; `--probe-mtu` finds the largest payload that survives each direction. For example, `xfr <host> -u -l 1368` fits a 1420-byte MTU with either IPv4 or IPv6 headers. The default remains compatible with older servers.

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

### UDP `Connection refused` Storms at >=50% Loss with Short Tests

**Issue:** Under tc netem `loss 50%` (or higher) on lo with short test durations (`-t 10sec`), the client's connected UDP socket can get stuck returning `ECONNREFUSED` on every send. The mechanism: TCP control handshake under heavy loss takes several seconds (retransmits), eating into test duration; if the server's test-end fires before the client's first UDP packet reaches the server's data port, the kernel returns ICMP port-unreachable, which sticks on the connected socket and rejects all subsequent sends.

**Impact:** A run that should report ~50% loss instead emits a flood of `Connection refused (os error 111)` warnings. Sometimes the test still completes correctly via the TCP Result message; sometimes it ends with `Timeout waiting for server response`. Pre-existing behavior — reproduces on v0.9.13 at the same or higher rate than v0.9.14.

**Workaround:** Use longer test durations (`-t 60sec` or more) so the handshake delay is amortized. The issue does not occur in real-world conditions where tc netem isn't applied to TCP control as well as UDP data.

---

### UDP Bidir Loss Has No Separate Download-Direction Reading

**Issue:** Download-only mode (`-u -R`) builds live throughput, jitter, and cumulative loss from the client's local receive counters on a local interval clock. Bidirectional UDP still exposes one live loss reading from the server's receive side (the upload direction), rather than separate readings for both directions. Its final result also has only one `udp_stats` field.

**Impact:** A bidirectional run does not separately show loss and jitter for the download direction, even though its download byte counts are corrected from the client's receive counters.

**Workaround:** Use a download-only run to measure download loss and jitter. Separate bidirectional direction metrics require protocol additions and remain tracked in ROADMAP.md.

---

### Non-TUI Interval Row Cadence Can Still Bunch Under Extreme Loss

**Issue:** Upload-mode full interval rows still follow TCP control `Interval` arrival, which can bunch under aggressive loss or bufferbloat. UDP feedback updates keep cumulative loss current. When full rows stall for roughly two report periods, plain text emits a `(reports stalled)` loss line and `--json-stream` emits a marked `udp_feedback` event. CSV remains interval-only to preserve its fixed columns. Download-only UDP intervals use local receive counters and a local clock. The source interval clocks sample once per second; `--interval` filters those updates, so values below one second do not create faster sampling.

**Impact:** Upload throughput rows and CSV cadence can lag or arrive in bursts, even while feedback loss events remain current. A feedback event carries cumulative loss rather than a full throughput interval.

**Workaround:** Consume `udp_feedback` events in JSON-stream scripts when live loss matters. Use fair queuing to reduce control-channel stalls; decoupling server stats sampling from control writes remains tracked in ROADMAP.md.

---

### Control-Channel Delivery Stalls Under Deep FIFO Bufferbloat

**Issue:** When the path to the server holds multiple seconds of FIFO queue (no AQM) and a UDP test oversubscribes it, TCP control messages queue behind the test's own flood, tail-drop, and retransmit. Packet captures during the v0.9.18 cycle show interval messages then arriving in batches every ~2 or ~4 seconds (the two stable rhythms of the queue/RTO interaction) instead of at the 1 Hz report cadence. The server's interval emission itself also degrades to ~2 s under this condition because stats sampling is coupled to the blocked control write. Which rhythm a given run locks onto is sensitive to startup phase — the single-port UDP handshake (v0.9.18, issue #63) shifts that phase, which changes the typical batch size in synthetic tests without changing the control channel's behavior.

**Impact:** Live interval rows (plain/CSV/JSON-stream) batch up under pathological bufferbloat. The TUI live loss counter and sparkline are unaffected (they ride `udp_feedback_v1` over UDP and the local-tick sparkline cadence). End-of-test results are always correct.

**Mitigation:** This is inherent to sharing a FIFO-bloated link with the traffic being tested; AQM (fq_codel, the modern Linux default) eliminates it entirely. Feedback-driven plain-text and JSON-stream loss events already provide visibility while full intervals stall. Decoupling stats sampling from control writes remains tracked in ROADMAP.md.

---

### Windows Support is Experimental

**Issue:** Windows is not a first-class platform. TCP_INFO statistics are not available (returns zeros), and some socket options may behave differently.

**Impact:** Basic TCP/UDP/QUIC testing works, but advanced metrics are missing.

**Workaround:** Use WSL2 for full functionality on Windows. Native Windows binaries are not provided.

---

## By Design

### QUIC Server Certificate Not Verified

QUIC transport uses self-signed certificates and does not verify the server's identity. This is intentional for ease of use in trusted environments.

**Impact:** QUIC connections without a PSK are encrypted but do not authenticate the server, leaving them vulnerable to server impersonation or man-in-the-middle attacks on untrusted networks.

**Mitigation:** Do not treat QUIC's self-signed certificate as server identity on unauthenticated sessions. Use `--psk-file` on untrusted networks: current PSK sessions bind mutual authentication and protected-control keys to the exact TLS connection through the RFC 9266 exporter. Both peers must support `quic_channel_binding_v1`; QUIC + PSK fails closed otherwise. The self-signed QUIC certificate is still not a public PKI identity.

### Protocol Extensions Require Capability Negotiation

The control protocol uses a tagged JSON enum for message types. Unknown fields within known message types are silently ignored (serde default), but unrecognized message *types* cause parse errors. New optional message types must be gated on peer capabilities before use. For example, `byte_budget_v1` lets a client send `Finish`; a client refuses a byte-budget test if the server lacks that capability. Breaking changes without a compatible negotiated path still require a major protocol version bump.

**Rationale:** The version handshake protects basic compatibility, while capability checks keep older peers from receiving messages they cannot parse.

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
- **QUIC bitrate limiting** - QUIC previously accepted `-b/--bitrate` on the CLI and dropped it on the send paths, so a rate-limited QUIC test ran at line rate and only logged a warning. Both directions are now paced with the same cumulative byte-budget approach used for TCP and UDP, split per stream, with the send buffer capped so a rate limit cannot be overshot by the first write.
- **Client capabilities negotiation** - Client and server exchange capabilities in the Hello handshake, allowing the server to adapt behavior (e.g., single-port vs multi-port TCP) based on client support.
- **QUIC control-stream wait** - Accepted QUIC peers now have 30 seconds to open their control stream. Idle peers cannot keep a handler slot indefinitely by maintaining a connection without sending a Hello. The subsequent Hello read retains its own 30-second timeout.
- **QUIC client DualStack binding** - The default local endpoint now matches the resolved remote address family, so IPv6 targets work without forcing `-6`. Explicit local binds are matched to the remote family as well. IPv6 and dual-stack client-port regressions cover this behavior.
- **QUIC one-off mode** - QUIC accept loop now responds to the shutdown signal for proper `--one-off` exit after a single test completes.
- **UDP payload size hardcoded to 1400 bytes** - every UDP datagram fragmented on paths with an MTU below 1428 bytes (1448 over IPv6), including WireGuard's 1420 default (issue #213). `-l/--length` now sets the datagram size (16–65507 bytes) for all streams and directions; servers advertise `udp_payload_size_v1`, and the client refuses an explicit size against an older server. The default stays 1400.
- **UDP reverse mode send errors hidden from the client** - in `-u -R` and the server half of `--bidir`, the server only logged failed sends, so the client saw lower throughput with no explanation. The final result now carries `server_udp_send_errors` (count plus the last error message; teardown-window errors excluded), and plain output, `--json` and the TUI history report it. The client's own failed sends in upload/bidir are reported the same way as `client_udp_send_errors`. Older peers ignore or omit the field.
- **TUI settings modal restart path** - Settings → Apply & Restart now cancels the current run, applies test settings, and starts a fresh run in place. QUIC-ignored options are logged in TUI history when applicable.

---

## Future Improvements

Some issues listed here may be addressed in future releases. See the [ROADMAP.md](ROADMAP.md) "Known Limitations" section for items under consideration.

---

## Reporting Issues

Found a bug not listed here? Please report it at: https://github.com/lance0/xfr/issues
