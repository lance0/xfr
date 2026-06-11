//! UDP path-MTU probe (`--probe-mtu`, issue #64).
//!
//! Discovers the largest UDP payload that survives the path in each
//! direction, in the spirit of DPLPMTUD (RFC 8899): walk a ladder of
//! common MTU sizes, then binary-search the gap between the largest
//! passing and smallest failing size. Every probe is sent with the IP
//! don't-fragment flag set so middleboxes must drop oversized packets
//! rather than fragment them — fragmentation would hide exactly the
//! limit we're measuring.
//!
//! Direction attribution uses a three-packet exchange per probe
//! (suggested by brettowe on the issue): the client sends a `Probe` of
//! the trial size; the server answers with a small fixed-size `Ack`
//! (which survives almost any path, proving the forward direction) and
//! a full-size `Echo` padded to the same trial size (proving the
//! reverse direction). Ack-without-echo therefore means the reverse
//! path blocks the size even though the forward path carried it.
//!
//! Wire framing shares the data socket with regular UDP test traffic
//! and the `XFRF` feedback lane; the `XFRP` magic plus a `kind` byte is
//! the discriminator. Probe mode never runs concurrently with bulk
//! traffic, but the magic keeps stray packets from being misparsed.

use serde::{Deserialize, Serialize};

/// Magic prefix on every probe-lane packet. Distinct from `XFRF`
/// (feedback) and from data-packet sequence prefixes (high bits of a
/// u64 counter starting at zero).
pub const PROBE_MAGIC: [u8; 4] = *b"XFRP";
/// Currently-supported probe packet protocol version.
pub const PROBE_VERSION: u8 = 1;
/// Header bytes preceding the padding in `Probe`/`Echo` packets; also
/// the full size of an `Ack`. Must stay below `MIN_PROBE_PAYLOAD`.
pub const PROBE_HEADER_SIZE: usize = 16;
/// Smallest trial payload. 576 is the classic IPv4 minimum-reassembly
/// datagram; nothing real blocks below this, and the header has to fit.
pub const MIN_PROBE_PAYLOAD: usize = 548; // 576 wire - 28 IPv4/UDP overhead
/// Largest trial payload: 9216-byte jumbo wire MTU minus IPv4 overhead.
pub const MAX_PROBE_PAYLOAD: usize = 9188;

/// Packet kinds on the probe lane.
pub const PROBE_KIND_PROBE: u8 = 1;
pub const PROBE_KIND_ACK: u8 = 2;
pub const PROBE_KIND_ECHO: u8 = 3;

/// Per-IP-version overhead between UDP payload size and wire MTU:
/// 20 (IPv4) or 40 (IPv6) IP header + 8 UDP header.
pub fn ip_overhead(is_ipv6: bool) -> usize {
    if is_ipv6 { 48 } else { 28 }
}

/// A probe-lane packet.
///
/// Wire layout, all multi-byte fields big-endian:
/// ```text
/// offset  size  field
///   0      4    magic = b"XFRP"
///   4      1    version = 1
///   5      1    kind (1 = probe, 2 = ack, 3 = echo)
///   6      2    flags = 0 (reserved)
///   8      4    seq (u32)
///  12      2    declared_size (u16) — full payload size of the Probe
///               this packet belongs to (Acks carry it so the client
///               can attribute them without trusting datagram length)
///  14      2    reserved = 0
///  16      —    padding to declared_size (Probe/Echo only)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbePacket {
    pub kind: u8,
    pub seq: u32,
    pub declared_size: u16,
}

impl ProbePacket {
    /// Encode into `buffer`, returning the number of bytes to send:
    /// `declared_size` for Probe/Echo (header + padding), or
    /// `PROBE_HEADER_SIZE` for Ack. Returns `None` when the buffer is
    /// too small or `declared_size` is below the header size for a
    /// padded kind.
    pub fn encode(&self, buffer: &mut [u8]) -> Option<usize> {
        let wire_len = match self.kind {
            PROBE_KIND_ACK => PROBE_HEADER_SIZE,
            _ => usize::from(self.declared_size),
        };
        if wire_len < PROBE_HEADER_SIZE || buffer.len() < wire_len {
            return None;
        }
        buffer[0..4].copy_from_slice(&PROBE_MAGIC);
        buffer[4] = PROBE_VERSION;
        buffer[5] = self.kind;
        buffer[6..8].copy_from_slice(&0u16.to_be_bytes()); // flags
        buffer[8..12].copy_from_slice(&self.seq.to_be_bytes());
        buffer[12..14].copy_from_slice(&self.declared_size.to_be_bytes());
        buffer[14..16].copy_from_slice(&0u16.to_be_bytes()); // reserved
        Some(wire_len)
    }

    /// Decode a probe-lane packet. Returns `None` for wrong magic,
    /// version, unknown kind, or a padded kind whose datagram is
    /// shorter than its header. Padding content is ignored.
    pub fn decode(buffer: &[u8]) -> Option<Self> {
        if buffer.len() < PROBE_HEADER_SIZE || buffer[0..4] != PROBE_MAGIC {
            return None;
        }
        if buffer[4] != PROBE_VERSION {
            return None;
        }
        let kind = buffer[5];
        if !matches!(kind, PROBE_KIND_PROBE | PROBE_KIND_ACK | PROBE_KIND_ECHO) {
            return None;
        }
        let seq = u32::from_be_bytes(buffer[8..12].try_into().ok()?);
        let declared_size = u16::from_be_bytes(buffer[12..14].try_into().ok()?);
        Some(Self {
            kind,
            seq,
            declared_size,
        })
    }
}

/// Outcome of probing one payload size in one direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SizeVerdict {
    /// At least one probe of this size made it through.
    Pass,
    /// Every attempt was lost.
    Fail,
    /// The local kernel refused the send (EMSGSIZE with DF set) —
    /// the local interface MTU is below this size.
    LocalLimit,
    /// Never exercised: the reverse direction is only tested by the
    /// echo a successful forward probe triggers, so a size whose
    /// forward probes all died leaves the reverse path unknown.
    Untested,
}

/// Per-size probe record, kept for the final report table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeResult {
    pub payload: usize,
    /// Probes acked by the server (forward direction successes).
    pub forward_ok: u8,
    /// Full-size echoes received back (reverse direction successes).
    pub reverse_ok: u8,
    /// Probes attempted at this size.
    pub attempts: u8,
    pub forward: SizeVerdict,
    pub reverse: SizeVerdict,
}

/// Final probe report, attached to the client-side `TestResult`
/// (wire-additive; the server never populates it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MtuProbeReport {
    /// Largest payload that passed client→server, if any size did.
    pub forward_max_payload: Option<usize>,
    /// Largest payload that passed server→client, if any size did.
    pub reverse_max_payload: Option<usize>,
    /// `forward_max_payload` + IP/UDP overhead for the address family
    /// used — the conventional "path MTU" number.
    pub forward_path_mtu: Option<usize>,
    pub reverse_path_mtu: Option<usize>,
    /// True when the address family was IPv6 (48-byte overhead).
    pub ipv6: bool,
    /// Every size tried, in probe order.
    pub sizes: Vec<SizeResult>,
}

/// The ladder of payload sizes tried before binary search, derived
/// from wire MTUs seen in the wild: IPv4 minimum (576), IPv6 minimum
/// (1280), PPPoE (1492), Ethernet (1500), FDDI (4352), jumbo (9000,
/// 9216). Clamped to the valid probe range and deduplicated; always
/// ends at `MAX_PROBE_PAYLOAD` so the search space is fully bracketed.
pub fn size_ladder(is_ipv6: bool) -> Vec<usize> {
    let overhead = ip_overhead(is_ipv6);
    let mut ladder: Vec<usize> = [576usize, 1280, 1492, 1500, 4352, 9000, 9216]
        .iter()
        .map(|wire| wire.saturating_sub(overhead))
        .filter(|&p| (MIN_PROBE_PAYLOAD..=MAX_PROBE_PAYLOAD).contains(&p))
        .collect();
    if ladder.first() != Some(&MIN_PROBE_PAYLOAD) {
        ladder.insert(0, MIN_PROBE_PAYLOAD);
    }
    if ladder.last() != Some(&MAX_PROBE_PAYLOAD) {
        ladder.push(MAX_PROBE_PAYLOAD);
    }
    ladder.dedup();
    ladder
}

/// Driver for the size-search state machine, kept free of socket IO so
/// the search logic is unit-testable. Call `next_size()` to get the
/// next payload to probe; report the outcome with `record()`; repeat
/// until `next_size()` returns `None`. Ladder phase first, then binary
/// search between the bracketing pass/fail pair.
///
/// The search key is the *forward* verdict: that is the direction the
/// prober controls the send size of directly. Reverse outcomes are
/// recorded per size for the report but do not steer the search — the
/// echo is always the same size as the probe, so reverse data arrives
/// for free at every step.
#[derive(Debug)]
pub struct SizeSearch {
    ladder: Vec<usize>,
    ladder_idx: usize,
    /// Largest size with a forward pass so far.
    low: Option<usize>,
    /// Smallest size with a forward fail so far.
    high: Option<usize>,
    in_binary: bool,
    pending: Option<usize>,
}

impl SizeSearch {
    pub fn new(is_ipv6: bool) -> Self {
        Self {
            ladder: size_ladder(is_ipv6),
            ladder_idx: 0,
            low: None,
            high: None,
            in_binary: false,
            pending: None,
        }
    }

    /// Next payload size to probe, or `None` when the search has
    /// converged (bracket closed to <= 1 byte or ladder exhausted with
    /// no failures).
    pub fn next_size(&mut self) -> Option<usize> {
        if let Some(p) = self.pending {
            return Some(p);
        }
        let next = if !self.in_binary {
            loop {
                match self.ladder.get(self.ladder_idx) {
                    // A ladder rung above a known failure is pointless;
                    // skip ahead (failures bracket from above).
                    Some(&size) if self.high.is_some_and(|h| size >= h) => {
                        self.ladder_idx += 1;
                    }
                    Some(&size) => break Some(size),
                    None => {
                        self.in_binary = true;
                        break self.binary_midpoint();
                    }
                }
            }
        } else {
            self.binary_midpoint()
        };
        self.pending = next;
        next
    }

    fn binary_midpoint(&self) -> Option<usize> {
        let low = self.low?;
        let high = self.high?;
        if high - low <= 1 {
            return None;
        }
        Some(low + (high - low) / 2)
    }

    /// Record the forward verdict for the size most recently returned
    /// by `next_size()`.
    pub fn record(&mut self, size: usize, forward: SizeVerdict) {
        self.pending = None;
        if !self.in_binary {
            self.ladder_idx += 1;
        }
        match forward {
            SizeVerdict::Pass => {
                if self.low.is_none_or(|l| size > l) {
                    self.low = Some(size);
                }
            }
            // LocalLimit brackets exactly like a path failure: the
            // search should converge below it either way.
            SizeVerdict::Fail | SizeVerdict::LocalLimit => {
                if self.high.is_none_or(|h| size < h) {
                    self.high = Some(size);
                }
            }
            // Reverse-only verdict; the search keys on forward outcomes
            // and a forward probe is always either sent or refused.
            SizeVerdict::Untested => {
                debug_assert!(false, "Untested is never a forward verdict");
            }
        }
    }

    /// Largest forward-passing size found, if any.
    pub fn forward_max(&self) -> Option<usize> {
        self.low
    }
}

/// Attempts per trial size. The first attempt that proves both
/// directions short-circuits, so the cost is paid only on lossy or
/// blocked sizes.
pub const PROBES_PER_SIZE: u8 = 3;
/// How long to wait for the ack/echo pair after each probe send.
pub const PROBE_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// Client-side probe driver: run the size search against a connected
/// UDP socket whose peer is a `mtu_probe_v1` server, and assemble the
/// report. The caller is responsible for having set don't-fragment on
/// the socket — without it the forward direction overstates the path.
///
/// Reverse-direction results are a lower bound capped by the forward
/// path: echoes are only triggered by probes that arrived, so a reverse
/// path *wider* than the forward one can't be observed from this side.
pub async fn run_probe(
    socket: &tokio::net::UdpSocket,
    is_ipv6: bool,
) -> std::io::Result<MtuProbeReport> {
    let mut search = SizeSearch::new(is_ipv6);
    let mut sizes = Vec::new();
    let mut send_buf = vec![0u8; MAX_PROBE_PAYLOAD];
    let mut recv_buf = vec![0u8; MAX_PROBE_PAYLOAD + 100];
    let mut seq: u32 = 0;
    let mut reverse_max: Option<usize> = None;

    while let Some(size) = search.next_size() {
        let mut forward_ok: u8 = 0;
        let mut reverse_ok: u8 = 0;
        let mut attempts: u8 = 0;
        let mut local_limit = false;

        for _ in 0..PROBES_PER_SIZE {
            attempts += 1;
            seq = seq.wrapping_add(1);
            let pkt = ProbePacket {
                kind: PROBE_KIND_PROBE,
                seq,
                declared_size: size as u16,
            };
            let len = pkt
                .encode(&mut send_buf)
                .expect("send buffer sized for MAX_PROBE_PAYLOAD");
            match socket.send(&send_buf[..len]).await {
                Ok(_) => {}
                Err(e) if is_emsgsize(&e) => {
                    local_limit = true;
                    break;
                }
                Err(e) => {
                    // Transient send failures (e.g. ECONNREFUSED surfaced
                    // from an ICMP unreachable on a connected socket)
                    // count as a lost attempt, not a fatal error — the
                    // search needs the verdict either way.
                    tracing::debug!("probe send failed at {}B: {}", size, e);
                    continue;
                }
            }

            let (got_ack, got_echo) = collect_replies(socket, &mut recv_buf, seq, size).await;
            if got_ack {
                forward_ok += 1;
            }
            if got_echo {
                reverse_ok += 1;
            }
            // Both directions proven at this size; no need to keep going.
            if got_ack && got_echo {
                break;
            }
        }

        let forward = if local_limit {
            SizeVerdict::LocalLimit
        } else if forward_ok > 0 {
            SizeVerdict::Pass
        } else {
            SizeVerdict::Fail
        };
        let reverse = if reverse_ok > 0 {
            SizeVerdict::Pass
        } else if forward == SizeVerdict::Pass {
            SizeVerdict::Fail
        } else {
            SizeVerdict::Untested
        };
        if reverse == SizeVerdict::Pass && reverse_max.is_none_or(|m| size > m) {
            reverse_max = Some(size);
        }
        sizes.push(SizeResult {
            payload: size,
            forward_ok,
            reverse_ok,
            attempts,
            forward,
            reverse,
        });
        search.record(size, forward);
    }

    let overhead = ip_overhead(is_ipv6);
    Ok(MtuProbeReport {
        forward_max_payload: search.forward_max(),
        reverse_max_payload: reverse_max,
        forward_path_mtu: search.forward_max().map(|p| p + overhead),
        reverse_path_mtu: reverse_max.map(|p| p + overhead),
        ipv6: is_ipv6,
        sizes,
    })
}

/// Wait up to [`PROBE_REPLY_TIMEOUT`] for the ack/echo pair belonging to
/// `seq`, returning early once both arrive. Echo validity requires the
/// datagram to actually be `size` bytes on the wire — the declared-size
/// field alone would also match a truncated delivery.
async fn collect_replies(
    socket: &tokio::net::UdpSocket,
    recv_buf: &mut [u8],
    seq: u32,
    size: usize,
) -> (bool, bool) {
    let deadline = tokio::time::Instant::now() + PROBE_REPLY_TIMEOUT;
    let mut got_ack = false;
    let mut got_echo = false;
    while !(got_ack && got_echo) {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        match tokio::time::timeout(deadline - now, socket.recv(recv_buf)).await {
            Err(_) => break, // reply window elapsed
            Ok(Err(e)) => {
                // ICMP-derived errors on a connected socket (port
                // unreachable etc.) read as recv failures; treat like a
                // lost reply and let the timeout bound the wait.
                tracing::debug!("probe recv failed for seq {}: {}", seq, e);
            }
            Ok(Ok(n)) => {
                let Some(reply) = ProbePacket::decode(&recv_buf[..n]) else {
                    continue;
                };
                if reply.seq != seq || usize::from(reply.declared_size) != size {
                    continue; // stale reply from an earlier attempt
                }
                match reply.kind {
                    PROBE_KIND_ACK => got_ack = true,
                    PROBE_KIND_ECHO if n == size => got_echo = true,
                    _ => {}
                }
            }
        }
    }
    (got_ack, got_echo)
}

#[cfg(unix)]
fn is_emsgsize(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::EMSGSIZE)
}

#[cfg(not(unix))]
fn is_emsgsize(_e: &std::io::Error) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_packet_roundtrip_all_kinds() {
        let mut buf = vec![0u8; MAX_PROBE_PAYLOAD];
        for kind in [PROBE_KIND_PROBE, PROBE_KIND_ACK, PROBE_KIND_ECHO] {
            let p = ProbePacket {
                kind,
                seq: 0xDEAD_BEEF_u32 >> 8,
                declared_size: 1472,
            };
            let n = p.encode(&mut buf).expect("encode");
            if kind == PROBE_KIND_ACK {
                assert_eq!(n, PROBE_HEADER_SIZE, "acks are always header-sized");
            } else {
                assert_eq!(n, 1472, "padded kinds send declared_size bytes");
            }
            let back = ProbePacket::decode(&buf[..n]).expect("decode");
            assert_eq!(back, p);
        }
    }

    #[test]
    fn probe_packet_rejects_foreign_and_short() {
        assert!(ProbePacket::decode(b"XFRF_not_probe_lane").is_none());
        assert!(ProbePacket::decode(&[0u8; 4]).is_none());
        // Unknown kind
        let mut buf = vec![0u8; 64];
        let p = ProbePacket {
            kind: PROBE_KIND_PROBE,
            seq: 1,
            declared_size: 64,
        };
        let n = p.encode(&mut buf).unwrap();
        buf[5] = 9;
        assert!(ProbePacket::decode(&buf[..n]).is_none());
        // Wrong version
        buf[5] = PROBE_KIND_PROBE;
        buf[4] = 2;
        assert!(ProbePacket::decode(&buf[..n]).is_none());
    }

    #[test]
    fn probe_packet_encode_rejects_undersized_declared() {
        let mut buf = vec![0u8; 64];
        let p = ProbePacket {
            kind: PROBE_KIND_PROBE,
            seq: 1,
            declared_size: (PROBE_HEADER_SIZE - 1) as u16,
        };
        assert!(p.encode(&mut buf).is_none());
    }

    #[test]
    fn ladder_is_sorted_dedup_and_bracketed() {
        for ipv6 in [false, true] {
            let ladder = size_ladder(ipv6);
            assert!(ladder.windows(2).all(|w| w[0] < w[1]), "sorted+dedup");
            assert_eq!(*ladder.first().unwrap(), MIN_PROBE_PAYLOAD);
            assert_eq!(*ladder.last().unwrap(), MAX_PROBE_PAYLOAD);
        }
    }

    /// Simulate a path with a given max payload and verify the search
    /// converges to exactly that size.
    fn run_search(path_max: usize, is_ipv6: bool) -> (Option<usize>, usize) {
        let mut search = SizeSearch::new(is_ipv6);
        let mut steps = 0;
        while let Some(size) = search.next_size() {
            steps += 1;
            assert!(steps < 64, "search must terminate");
            let verdict = if size <= path_max {
                SizeVerdict::Pass
            } else {
                SizeVerdict::Fail
            };
            search.record(size, verdict);
        }
        (search.forward_max(), steps)
    }

    #[test]
    fn search_converges_to_exact_path_limit() {
        // Ethernet-ish, an awkward in-between value, jumbo, and the
        // bracket edges.
        for path_max in [1472, 1473, 2000, 8999, MIN_PROBE_PAYLOAD, MAX_PROBE_PAYLOAD] {
            let (found, _) = run_search(path_max, false);
            let expected = path_max.min(MAX_PROBE_PAYLOAD);
            assert_eq!(
                found,
                Some(expected),
                "path_max {path_max} should converge exactly"
            );
        }
    }

    #[test]
    fn search_with_nothing_passing_returns_none() {
        // Path blocks everything (pathological, but the report must
        // say "no size passed" rather than picking one).
        let mut search = SizeSearch::new(false);
        let mut steps = 0;
        while let Some(size) = search.next_size() {
            steps += 1;
            assert!(steps < 64);
            search.record(size, SizeVerdict::Fail);
        }
        assert_eq!(search.forward_max(), None);
    }

    #[test]
    fn search_skips_ladder_rungs_above_known_failure() {
        let mut search = SizeSearch::new(false);
        let first = search.next_size().unwrap();
        assert_eq!(first, MIN_PROBE_PAYLOAD);
        search.record(first, SizeVerdict::Pass);
        let second = search.next_size().unwrap();
        // Fail the second rung; every later rung is larger and must be
        // skipped, sending the search straight to binary refinement
        // strictly inside (first, second).
        search.record(second, SizeVerdict::Fail);
        let next = search.next_size().unwrap();
        assert!(
            next > first && next < second,
            "expected binary midpoint inside ({first}, {second}), got {next}"
        );
    }

    #[test]
    fn local_limit_brackets_like_failure() {
        let mut search = SizeSearch::new(false);
        let mut last_pass = None;
        while let Some(size) = search.next_size() {
            // Kernel refuses anything over 1300, path is clean below.
            let verdict = if size > 1300 {
                SizeVerdict::LocalLimit
            } else {
                last_pass = Some(size);
                SizeVerdict::Pass
            };
            search.record(size, verdict);
        }
        assert_eq!(search.forward_max(), Some(1300));
        assert_eq!(last_pass, Some(1300));
    }

    #[test]
    fn ip_overhead_values() {
        assert_eq!(ip_overhead(false), 28);
        assert_eq!(ip_overhead(true), 48);
    }

    /// Full driver/responder exchange over loopback. The loopback MTU
    /// (65536 on Linux, ~16k on macOS) exceeds every trial size, so the
    /// search must converge to MAX_PROBE_PAYLOAD in both directions.
    #[tokio::test]
    async fn run_probe_against_responder_on_loopback() {
        let server = std::sync::Arc::new(
            tokio::net::UdpSocket::bind("127.0.0.1:0")
                .await
                .expect("bind server"),
        );
        let server_addr = server.local_addr().unwrap();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let responder = tokio::spawn(crate::udp::respond_mtu_probes(server.clone(), cancel_rx));

        let client = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind client");
        client.connect(server_addr).await.expect("connect");
        let report = run_probe(&client, false).await.expect("probe");

        assert_eq!(report.forward_max_payload, Some(MAX_PROBE_PAYLOAD));
        assert_eq!(report.reverse_max_payload, Some(MAX_PROBE_PAYLOAD));
        assert_eq!(report.forward_path_mtu, Some(MAX_PROBE_PAYLOAD + 28));
        assert!(!report.ipv6);
        assert!(
            report.sizes.iter().all(|s| s.forward == SizeVerdict::Pass),
            "loopback passes every size: {:?}",
            report.sizes
        );
        // Every successful size short-circuits after one attempt.
        assert!(report.sizes.iter().all(|s| s.attempts == 1));

        responder.abort();
    }

    /// A responder that never echoes (acks only) must yield forward
    /// passes with reverse failures — the asymmetric-path shape.
    #[tokio::test]
    async fn run_probe_ack_only_responder_reports_reverse_blocked() {
        use crate::probe::{PROBE_KIND_ACK, PROBE_KIND_PROBE};

        let server = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind server");
        let server_addr = server.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let mut buf = vec![0u8; MAX_PROBE_PAYLOAD + 100];
            let mut out = vec![0u8; PROBE_HEADER_SIZE];
            loop {
                let Ok((n, from)) = server.recv_from(&mut buf).await else {
                    break;
                };
                let Some(pkt) = ProbePacket::decode(&buf[..n]) else {
                    continue;
                };
                if pkt.kind != PROBE_KIND_PROBE {
                    continue;
                }
                let ack = ProbePacket {
                    kind: PROBE_KIND_ACK,
                    seq: pkt.seq,
                    declared_size: pkt.declared_size,
                };
                let len = ack.encode(&mut out).unwrap();
                let _ = server.send_to(&out[..len], from).await;
            }
        });

        let client = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind client");
        client.connect(server_addr).await.expect("connect");
        let report = run_probe(&client, false).await.expect("probe");

        assert_eq!(report.forward_max_payload, Some(MAX_PROBE_PAYLOAD));
        assert_eq!(report.reverse_max_payload, None);
        assert!(
            report.sizes.iter().all(|s| s.reverse == SizeVerdict::Fail),
            "acked-but-never-echoed must read as reverse Fail: {:?}",
            report.sizes
        );

        responder.abort();
    }
}
