//! UDP data transfer with pacing and receiver-progress feedback
//!
//! Implements paced UDP sending to avoid buffer saturation and provides
//! jitter/loss calculation per RFC 3550.
//!
//! Also defines [`UdpFeedbackPacket`], the 36-byte cumulative-counts
//! receiver-progress packet emitted by the server's [`receive_udp`] task
//! at 2 Hz back to the client when both peers advertise the
//! `udp_feedback_v1` capability. The feedback path lets live UDP loss
//! visibility on the client sidestep the TCP control channel that may be
//! competing for ACKs against a saturated UDP uplink (issue #70). Demux
//! between data packets ([`UdpPacketHeader`], 16-byte header + payload)
//! and feedback packets is length-first (36 bytes for feedback vs.
//! 1416 bytes for data), so it never relies on inspecting
//! sequence-number bits.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::time::interval;
use tracing::{debug, warn};

use crate::protocol::{UdpIntervalProgress, UdpStats};
use crate::stats::StreamStats;

pub const UDP_PAYLOAD_SIZE: usize = 1400; // Leave room for IP + UDP headers
const UDP_HEADER_SIZE: usize = 16; // sequence (8) + timestamp_us (8)
const UDP_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(30); // Cleanup stale sessions

/// On-wire size of a v1 receiver-progress feedback packet
/// (`UdpFeedbackPacket`). Length is the primary discriminator between data
/// and feedback packets in the receive demux, and the magic byte sequence
/// is the secondary check; together they give a robust split that does
/// not rely on inspecting sequence-number bits in a data packet.
pub const UDP_FEEDBACK_SIZE: usize = 36;
/// Magic prefix on `UdpFeedbackPacket`. Distinct from any byte pattern
/// produced by `UdpPacketHeader::encode` for realistic test runs (the
/// first 4 bytes of every data packet are the high 32 bits of a u64
/// sequence counter starting at zero).
pub const UDP_FEEDBACK_MAGIC: [u8; 4] = *b"XFRF";
/// Currently-supported feedback packet protocol version.
pub const UDP_FEEDBACK_VERSION: u8 = 1;
/// `kind` byte for receiver-progress packets. Future packet types use a
/// new `kind` value so the parser stays selectable on length+kind.
pub const UDP_FEEDBACK_KIND_RECEIVER_PROGRESS: u8 = 1;

/// UDP receiver-feedback packet (`udp_feedback_v1`).
///
/// The server's UDP receive task emits these back to the client at 2 Hz
/// during upload-mode tests, sidestepping the TCP control channel for
/// live progress reporting. Cumulative counts let the client recover
/// from dropped feedback packets — the next tick carries the latest
/// truth without referring to anything we lost.
///
/// Wire layout, all multi-byte fields big-endian:
/// ```text
/// offset  size  field
///   0      4    magic = b"XFRF"
///   4      1    version = 1
///   5      1    kind = 1 (receiver_progress)
///   6      2    flags = 0 (reserved)
///   8      2    stream_id (u16)
///  10      2    reserved = 0
///  12      8    elapsed_ms
///  20      8    packets_received (cumulative)
///  28      8    packets_lost (cumulative)
/// total: 36 bytes, fixed
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpFeedbackPacket {
    pub stream_id: u16,
    pub elapsed_ms: u64,
    pub packets_received: u64,
    pub packets_lost: u64,
}

impl UdpFeedbackPacket {
    /// Encode the packet into `buffer` in 36 bytes. Returns false if the
    /// buffer is too small.
    pub fn encode(&self, buffer: &mut [u8]) -> bool {
        if buffer.len() < UDP_FEEDBACK_SIZE {
            return false;
        }
        buffer[0..4].copy_from_slice(&UDP_FEEDBACK_MAGIC);
        buffer[4] = UDP_FEEDBACK_VERSION;
        buffer[5] = UDP_FEEDBACK_KIND_RECEIVER_PROGRESS;
        buffer[6..8].copy_from_slice(&0u16.to_be_bytes()); // flags
        buffer[8..10].copy_from_slice(&self.stream_id.to_be_bytes());
        buffer[10..12].copy_from_slice(&0u16.to_be_bytes()); // reserved
        buffer[12..20].copy_from_slice(&self.elapsed_ms.to_be_bytes());
        buffer[20..28].copy_from_slice(&self.packets_received.to_be_bytes());
        buffer[28..36].copy_from_slice(&self.packets_lost.to_be_bytes());
        true
    }

    /// Decode a 36-byte feedback packet. Returns `None` if length, magic,
    /// version, or kind don't match the v1 schema. Forward-compatible
    /// only via new `kind` values, not via larger v1 packets.
    pub fn decode(buffer: &[u8]) -> Option<Self> {
        if buffer.len() != UDP_FEEDBACK_SIZE {
            return None;
        }
        if buffer[0..4] != UDP_FEEDBACK_MAGIC {
            return None;
        }
        if buffer[4] != UDP_FEEDBACK_VERSION {
            return None;
        }
        if buffer[5] != UDP_FEEDBACK_KIND_RECEIVER_PROGRESS {
            return None;
        }
        let stream_id = u16::from_be_bytes(buffer[8..10].try_into().ok()?);
        let elapsed_ms = u64::from_be_bytes(buffer[12..20].try_into().ok()?);
        let packets_received = u64::from_be_bytes(buffer[20..28].try_into().ok()?);
        let packets_lost = u64::from_be_bytes(buffer[28..36].try_into().ok()?);
        Some(Self {
            stream_id,
            elapsed_ms,
            packets_received,
            packets_lost,
        })
    }

    /// Convert the cumulative counts into the shared
    /// `UdpIntervalProgress` shape consumed by the progress aggregator.
    pub fn to_progress(&self) -> UdpIntervalProgress {
        UdpIntervalProgress {
            packets_received: self.packets_received,
            packets_lost: self.packets_lost,
        }
    }
}

/// Magic prefix on single-port UDP hello/ack packets (issue #63). These
/// are the only xfr packets that ever cross the server's shared wildcard
/// socket, where QUIC traffic is demuxed by header bits: the first byte
/// must have the top two bits clear so it can never look like a QUIC
/// long header (0x80, RFC 8999) or short header (0x40 fixed bit). The
/// legacy `XFRF`/`XFRP` magics start with 'X' (0x58 = 0b0101_1000) and
/// stay off the shared socket entirely — they ride connected sockets
/// where the existing demux is unchanged.
pub const UDP_HELLO_MAGIC: [u8; 4] = *b"\x00XFR";
/// Fixed on-wire size of a hello/ack packet.
pub const UDP_HELLO_SIZE: usize = 24;
/// Currently-supported hello packet protocol version.
pub const UDP_HELLO_VERSION: u8 = 1;
/// `kind` byte for the client→server stream hello.
pub const UDP_HELLO_KIND_HELLO: u8 = 1;
/// `kind` byte for the server→client ack, sent from the per-stream
/// connected socket so the client learns the connected path is live.
pub const UDP_HELLO_KIND_ACK: u8 = 2;
/// Length of the per-test routing token carried in every hello/ack.
pub const UDP_HELLO_TOKEN_LEN: usize = 16;
/// How often the client re-sends an unacknowledged hello.
pub const UDP_HELLO_RETRY_INTERVAL: Duration = Duration::from_millis(100);
/// Total hello attempts before the handshake fails loudly (~5 s budget).
pub const UDP_HELLO_MAX_ATTEMPTS: u32 = 50;

/// Single-port UDP hello/ack packet (issue #63).
///
/// The client sends a hello per stream to the server's main port; the
/// server answers with an ack from a freshly connected same-port socket.
/// The token (server-generated, carried back in `TestAck.udp_token`) is
/// what routes the hello to the right test — source address alone would
/// break under NAT or multiple concurrent clients.
///
/// Wire layout, all multi-byte fields big-endian:
/// ```text
/// offset  size  field
///   0      4    magic = b"\x00XFR"
///   4      1    version = 1
///   5      1    kind (1 = hello, 2 = ack)
///   6      2    stream_index (u16)
///   8     16    token
/// total: 24 bytes, fixed
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpHelloPacket {
    pub kind: u8,
    pub stream_index: u16,
    pub token: [u8; UDP_HELLO_TOKEN_LEN],
}

impl UdpHelloPacket {
    /// Encode the packet into a fixed 24-byte buffer.
    pub fn encode(&self) -> [u8; UDP_HELLO_SIZE] {
        let mut buffer = [0u8; UDP_HELLO_SIZE];
        buffer[0..4].copy_from_slice(&UDP_HELLO_MAGIC);
        buffer[4] = UDP_HELLO_VERSION;
        buffer[5] = self.kind;
        buffer[6..8].copy_from_slice(&self.stream_index.to_be_bytes());
        buffer[8..24].copy_from_slice(&self.token);
        buffer
    }

    /// Decode a hello/ack packet. Returns `None` if length, magic,
    /// version, or kind don't match the v1 schema — fixed-size like the
    /// feedback packet, forward-compatible only via new `kind` values.
    pub fn decode(buffer: &[u8]) -> Option<Self> {
        if buffer.len() != UDP_HELLO_SIZE {
            return None;
        }
        if buffer[0..4] != UDP_HELLO_MAGIC {
            return None;
        }
        if buffer[4] != UDP_HELLO_VERSION {
            return None;
        }
        let kind = buffer[5];
        if !matches!(kind, UDP_HELLO_KIND_HELLO | UDP_HELLO_KIND_ACK) {
            return None;
        }
        let stream_index = u16::from_be_bytes(buffer[6..8].try_into().ok()?);
        let token = buffer[8..24].try_into().ok()?;
        Some(Self {
            kind,
            stream_index,
            token,
        })
    }
}

/// Hex-encode a routing token for the `TestAck.udp_token` wire field.
pub fn encode_hello_token(token: &[u8; UDP_HELLO_TOKEN_LEN]) -> String {
    token.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Parse a `TestAck.udp_token` hex string back into raw token bytes.
/// Returns `None` for anything that isn't exactly 32 hex digits.
pub fn parse_hello_token(s: &str) -> Option<[u8; UDP_HELLO_TOKEN_LEN]> {
    if s.len() != UDP_HELLO_TOKEN_LEN * 2 || !s.is_ascii() {
        return None;
    }
    let mut token = [0u8; UDP_HELLO_TOKEN_LEN];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        token[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(token)
}

/// Which lane a datagram on the shared server socket belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedSocketLane {
    /// Single-port UDP hello — divert to the hello dispatcher.
    XfrHello,
    /// QUIC — pass through to quinn untouched.
    Quic,
    /// Neither: shouldn't normally happen, since once a stream's
    /// connected socket exists the kernel routes its data there and the
    /// shared socket never sees it. Logged at debug and dropped.
    XfrStray,
}

/// Classify a datagram arriving on the shared (QUIC + xfr hello) socket.
///
/// Order matters and matches the design for issue #63:
/// 1. exact match on the single-port hello framing → xfr hello lane;
/// 2. first byte with 0x80 set → QUIC long header (version-independent
///    per RFC 8999);
/// 3. first byte with 0x40 set → QUIC short header. Sound only because
///    the server endpoint sets `grease_quic_bit(false)`: RFC 9287
///    greasing is permission-based, so a server that doesn't advertise
///    the grease transport parameter stops compliant peers from clearing
///    the fixed bit toward it;
/// 4. anything else → stray xfr lane.
pub fn classify_shared_datagram(datagram: &[u8]) -> SharedSocketLane {
    if datagram.len() == UDP_HELLO_SIZE && datagram[0..4] == UDP_HELLO_MAGIC {
        return SharedSocketLane::XfrHello;
    }
    match datagram.first() {
        Some(b) if b & 0x80 != 0 => SharedSocketLane::Quic,
        Some(b) if b & 0x40 != 0 => SharedSocketLane::Quic,
        _ => SharedSocketLane::XfrStray,
    }
}

/// Client side of the single-port UDP stream handshake (issue #63).
///
/// Sends a hello datagram on the connected `socket` every
/// [`UDP_HELLO_RETRY_INTERVAL`] until the server's ack arrives, up to
/// [`UDP_HELLO_MAX_ATTEMPTS`]. Inbound datagrams are `peek`ed, not
/// consumed: a matching ack is consumed and completes the handshake; a
/// foreign hello-lane packet is consumed and dropped; anything else
/// (test data in download mode, where the server starts sending as soon
/// as the hello routes) is treated as an implicit ack and left queued
/// for the data path so it isn't lost from the receiver's accounting.
///
/// The caller must not start sending data until this returns Ok — that
/// ordering is what eliminates the race where data reaches the server's
/// shared socket before the per-stream connected socket exists.
pub async fn single_port_udp_handshake(
    socket: &UdpSocket,
    token: &[u8; UDP_HELLO_TOKEN_LEN],
    stream_index: u16,
) -> anyhow::Result<()> {
    let hello = UdpHelloPacket {
        kind: UDP_HELLO_KIND_HELLO,
        stream_index,
        token: *token,
    }
    .encode();
    // Full-size buffer: Windows fails peek/recv with WSAEMSGSIZE when the
    // buffer is smaller than the datagram, and download-mode data packets
    // can arrive here.
    let mut buf = [0u8; UDP_PAYLOAD_SIZE + 100];

    for _ in 0..UDP_HELLO_MAX_ATTEMPTS {
        socket.send(&hello).await?;
        let retry_at = tokio::time::Instant::now() + UDP_HELLO_RETRY_INTERVAL;
        loop {
            let n = tokio::select! {
                result = socket.peek(&mut buf) => result?,
                _ = tokio::time::sleep_until(retry_at) => break,
            };
            if n == UDP_HELLO_SIZE && buf[0..4] == UDP_HELLO_MAGIC {
                let matched = UdpHelloPacket::decode(&buf[..n]).is_some_and(|pkt| {
                    pkt.kind == UDP_HELLO_KIND_ACK
                        && pkt.token == *token
                        && pkt.stream_index == stream_index
                });
                // Consume the hello-lane packet either way; only a full
                // match completes the handshake.
                let _ = socket.recv(&mut buf).await;
                if matched {
                    return Ok(());
                }
                continue;
            }
            // Non-hello-lane traffic from the server means our hello was
            // already routed (data only flows after that). Leave it queued.
            return Ok(());
        }
    }

    Err(anyhow::anyhow!(
        "single-port UDP handshake timed out for stream {}: no ack from the server \
         after {} attempts over {:?}. The server's UDP port may be reachable while \
         replies are filtered, or the server dropped the test.",
        stream_index,
        UDP_HELLO_MAX_ATTEMPTS,
        UDP_HELLO_RETRY_INTERVAL * UDP_HELLO_MAX_ATTEMPTS
    ))
}

/// Server-side ack responder for directions where no receive loop runs
/// on the connected socket (download mode). The first ack from the
/// dispatcher can be lost; the client's retried hellos then arrive on
/// the connected socket (the kernel routes them past the shared
/// wildcard), so someone must keep answering until one ack lands. Runs
/// until `cancel` fires; idles on recv once the client stops retrying.
pub async fn respond_single_port_hellos(
    socket: Arc<UdpSocket>,
    ack: [u8; UDP_HELLO_SIZE],
    mut cancel: watch::Receiver<bool>,
) {
    let mut buf = [0u8; UDP_PAYLOAD_SIZE + 100];

    let cancel_changed = async move {
        loop {
            if *cancel.borrow() {
                return;
            }
            if cancel.changed().await.is_err() {
                return;
            }
        }
    };
    tokio::pin!(cancel_changed);

    loop {
        tokio::select! {
            result = socket.recv(&mut buf) => {
                if let Ok(n) = result
                    && let Some(pkt) = UdpHelloPacket::decode(&buf[..n])
                    && pkt.kind == UDP_HELLO_KIND_HELLO
                {
                    let _ = socket.send(&ack).await;
                }
            }
            _ = &mut cancel_changed => break,
        }
    }
}

/// UDP packet header
/// [seq: u64][timestamp_us: u64][payload...]
///
/// Timestamps are relative to test start (not wall clock) to avoid
/// clock synchronization issues between client and server.
#[derive(Debug, Clone, Copy)]
pub struct UdpPacketHeader {
    pub sequence: u64,
    pub timestamp_us: u64,
}

impl UdpPacketHeader {
    pub fn encode(&self, buffer: &mut [u8]) -> bool {
        if buffer.len() < UDP_HEADER_SIZE {
            return false;
        }
        buffer[0..8].copy_from_slice(&self.sequence.to_be_bytes());
        buffer[8..16].copy_from_slice(&self.timestamp_us.to_be_bytes());
        true
    }

    pub fn decode(buffer: &[u8]) -> Option<Self> {
        if buffer.len() < UDP_HEADER_SIZE {
            return None;
        }
        let sequence = u64::from_be_bytes(buffer[0..8].try_into().ok()?);
        let timestamp_us = u64::from_be_bytes(buffer[8..16].try_into().ok()?);
        Some(Self {
            sequence,
            timestamp_us,
        })
    }
}

#[derive(Debug, Clone)]
pub struct UdpSendStats {
    pub packets_sent: u64,
    pub bytes_sent: u64,
}

/// Receiver-side jitter calculator per RFC 3550
pub struct JitterCalculator {
    last_send_time: Option<u64>,
    last_recv_time: Option<Instant>,
    jitter: f64,
    jitter_max: f64,
}

impl JitterCalculator {
    pub fn new() -> Self {
        Self {
            last_send_time: None,
            last_recv_time: None,
            jitter: 0.0,
            jitter_max: 0.0,
        }
    }

    /// Update jitter using RFC 3550 algorithm:
    /// D(i) = (R(i) - R(i-1)) - (S(i) - S(i-1))
    /// J(i) = J(i-1) + (|D(i)| - J(i-1)) / 16
    pub fn update(&mut self, send_time_us: u64, recv_time: Instant) -> f64 {
        if let (Some(last_send), Some(last_recv)) = (self.last_send_time, self.last_recv_time) {
            let recv_diff = recv_time.duration_since(last_recv).as_micros() as i64;
            let send_diff = (send_time_us as i64) - (last_send as i64);
            let d = (recv_diff - send_diff).abs() as f64;

            self.jitter += (d - self.jitter) / 16.0;
            if self.jitter > self.jitter_max {
                self.jitter_max = self.jitter;
            }
        }

        self.last_send_time = Some(send_time_us);
        self.last_recv_time = Some(recv_time);
        self.jitter
    }

    pub fn jitter_ms(&self) -> f64 {
        self.jitter / 1000.0
    }

    pub fn jitter_max_ms(&self) -> f64 {
        self.jitter_max / 1000.0
    }
}

impl Default for JitterCalculator {
    fn default() -> Self {
        Self::new()
    }
}

/// Receiver-side packet tracker for loss and out-of-order detection
pub struct PacketTracker {
    expected_sequence: u64,
    received: u64,
    lost: AtomicU64,
    out_of_order: AtomicU64,
    highest_seen: u64,
}

impl PacketTracker {
    pub fn new() -> Self {
        Self {
            expected_sequence: 0,
            received: 0,
            lost: AtomicU64::new(0),
            out_of_order: AtomicU64::new(0),
            highest_seen: 0,
        }
    }

    pub fn record(&mut self, sequence: u64) {
        self.received += 1;

        if sequence < self.expected_sequence {
            // Out of order packet
            self.out_of_order.fetch_add(1, Ordering::Relaxed);
        } else if sequence > self.expected_sequence {
            // Gap detected - packets lost
            let gap = sequence - self.expected_sequence;
            self.lost.fetch_add(gap, Ordering::Relaxed);
            self.expected_sequence = sequence + 1;
        } else {
            self.expected_sequence = sequence + 1;
        }

        self.highest_seen = self.highest_seen.max(sequence);
    }

    pub fn stats(&self, packets_sent: u64) -> (u64, u64, f64) {
        let lost = self.lost.load(Ordering::Relaxed);
        let ooo = self.out_of_order.load(Ordering::Relaxed);
        let loss_percent = if packets_sent > 0 {
            (lost as f64 / packets_sent as f64) * 100.0
        } else {
            0.0
        };
        (lost, ooo, loss_percent)
    }
}

impl Default for PacketTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Burst size threshold: batch packets when PPS exceeds this
const HIGH_PPS_THRESHOLD: f64 = 100_000.0;
/// Number of packets to send per burst in high-PPS mode
const BURST_SIZE: u64 = 100;

/// Send UDP data at a paced rate (or unlimited if target_bitrate is 0)
///
/// If `target` is Some, uses send_to() for unconnected sockets (server reverse mode).
/// If `target` is None, uses send() for connected sockets (client mode).
#[allow(clippy::too_many_arguments)]
pub async fn send_udp_paced(
    socket: Arc<UdpSocket>,
    target: Option<SocketAddr>,
    target_bitrate: u64,
    duration: Duration,
    stats: Arc<StreamStats>,
    mut cancel: watch::Receiver<bool>,
    mut pause: watch::Receiver<bool>,
    random_payload: bool,
) -> anyhow::Result<UdpSendStats> {
    let packet_size = UDP_PAYLOAD_SIZE;

    // Unlimited mode: no pacing, send as fast as possible
    if target_bitrate == 0 {
        return send_udp_unlimited(
            socket,
            target,
            duration,
            stats,
            cancel,
            pause,
            random_payload,
        )
        .await;
    }

    let bits_per_packet = (packet_size * 8) as u64;

    // Use floating-point for precision in interval calculation
    let packets_per_sec_f64 = target_bitrate as f64 / bits_per_packet as f64;

    // For high PPS, batch multiple packets per interval to reduce timer overhead
    let (pacing_interval, packets_per_tick) = if packets_per_sec_f64 > HIGH_PPS_THRESHOLD {
        // High PPS: batch BURST_SIZE packets per interval
        let interval = Duration::from_secs_f64(BURST_SIZE as f64 / packets_per_sec_f64);
        (interval, BURST_SIZE)
    } else {
        // Normal PPS: one packet per interval
        let interval = Duration::from_secs_f64(1.0 / packets_per_sec_f64);
        (interval, 1)
    };

    debug!(
        "UDP pacing: {:.0} packets/sec, interval {:?}, {} packets/tick",
        packets_per_sec_f64, pacing_interval, packets_per_tick
    );

    let mut sequence: u64 = 0;
    let mut ticker = {
        let mut t = interval(pacing_interval);
        // Skip stale ticks accumulated during pause so the sender doesn't
        // burst-fire after resume (LAN-160 #14).
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        t
    };
    let start = Instant::now();
    let mut deadline = start + duration;
    let is_infinite = duration == Duration::ZERO;

    let mut packet = vec![0u8; packet_size];
    if random_payload {
        rand::RngExt::fill(&mut rand::rng(), &mut packet[UDP_HEADER_SIZE..]);
    }

    loop {
        if *cancel.borrow() {
            debug!("UDP send cancelled");
            break;
        }
        if crate::pause::is_paused(&pause) {
            let (cancelled, paused) =
                crate::pause::wait_while_paused_timed(&mut pause, &mut cancel).await;
            if cancelled {
                break;
            }
            // Extend the deadline by the time spent paused (LAN-230)
            deadline += paused;
            // Reset the pacing ticker after resume so the first post-pause
            // tick fires a full interval from now, not immediately (the
            // queued tick from before pause would fire instantly and cause
            // a burst).  Combined with MissedTickBehavior::Skip, this
            // ensures no burst after resume (LAN-160 #14).
            ticker.reset();
            continue;
        }

        // Wait for ticker, interruptible by cancel/pause
        tokio::select! {
            biased;
            res = cancel.changed() => {
                if res.is_err() || *cancel.borrow() { break; }
                continue;
            }
            _ = pause.changed(), if crate::pause::channel_is_open(&pause) => { continue; } // re-check at top
            _ = ticker.tick() => {}
        }

        // Duration::ZERO means infinite - only check deadline if finite
        if !is_infinite && Instant::now() >= deadline {
            break;
        }

        // Send packets_per_tick packets in this burst
        for _ in 0..packets_per_tick {
            if *cancel.borrow() || crate::pause::is_paused(&pause) {
                break;
            }
            if !is_infinite && Instant::now() >= deadline {
                break;
            }

            // Build packet with relative timestamp
            let now_us = start.elapsed().as_micros() as u64;
            let header = UdpPacketHeader {
                sequence,
                timestamp_us: now_us,
            };
            header.encode(&mut packet);

            let result = match target {
                Some(addr) => socket.send_to(&packet, addr).await,
                None => socket.send(&packet).await,
            };

            match result {
                Ok(n) => {
                    stats.add_bytes_sent(n as u64);
                    sequence += 1;
                }
                Err(e) => {
                    warn!("UDP send error: {}", e);
                    // Continue sending - UDP is best-effort
                }
            }
        }
    }

    Ok(UdpSendStats {
        packets_sent: sequence,
        bytes_sent: sequence * packet_size as u64,
    })
}

/// Send UDP data as fast as possible (unlimited mode)
async fn send_udp_unlimited(
    socket: Arc<UdpSocket>,
    target: Option<SocketAddr>,
    duration: Duration,
    stats: Arc<StreamStats>,
    mut cancel: watch::Receiver<bool>,
    mut pause: watch::Receiver<bool>,
    random_payload: bool,
) -> anyhow::Result<UdpSendStats> {
    let packet_size = UDP_PAYLOAD_SIZE;
    let mut sequence: u64 = 0;
    let start = Instant::now();
    let mut deadline = start + duration;
    let is_infinite = duration == Duration::ZERO;
    let mut packet = vec![0u8; packet_size];
    if random_payload {
        rand::RngExt::fill(&mut rand::rng(), &mut packet[UDP_HEADER_SIZE..]);
    }

    debug!("UDP unlimited mode: sending as fast as possible");

    // Send in tight loop with periodic yield and cancel check
    loop {
        if *cancel.borrow() {
            debug!("UDP send cancelled");
            break;
        }
        if crate::pause::is_paused(&pause) {
            let (cancelled, paused) =
                crate::pause::wait_while_paused_timed(&mut pause, &mut cancel).await;
            if cancelled {
                break;
            }
            // Extend the deadline by the time spent paused (LAN-230)
            deadline += paused;
            continue;
        }

        // Duration::ZERO means infinite - only check deadline if finite
        if !is_infinite && Instant::now() >= deadline {
            break;
        }

        // Send a burst of packets before yielding
        for _ in 0..BURST_SIZE {
            if *cancel.borrow() || crate::pause::is_paused(&pause) {
                break;
            }
            if !is_infinite && Instant::now() >= deadline {
                break;
            }

            let now_us = start.elapsed().as_micros() as u64;
            let header = UdpPacketHeader {
                sequence,
                timestamp_us: now_us,
            };
            header.encode(&mut packet);

            let result = match target {
                Some(addr) => socket.send_to(&packet, addr).await,
                None => socket.send(&packet).await,
            };

            match result {
                Ok(n) => {
                    stats.add_bytes_sent(n as u64);
                    sequence += 1;
                }
                Err(e) => {
                    warn!("UDP send error: {}", e);
                }
            }
        }

        // Yield to allow other tasks (cancel checks, etc.)
        tokio::task::yield_now().await;
    }

    Ok(UdpSendStats {
        packets_sent: sequence,
        bytes_sent: sequence * packet_size as u64,
    })
}

/// Receive UDP data and track statistics
///
/// `single_port_ack` is `Some` only on the server side of a single-port
/// UDP test (issue #63), where the socket is connected to the client:
/// retried stream hellos arriving here (the kernel routes them past the
/// shared wildcard once the connected socket exists) are answered with
/// the precomputed ack instead of being counted, and feedback packets
/// use `send()` rather than `send_to()` — macOS rejects `send_to` on a
/// connected UDP socket with EISCONN.
pub async fn receive_udp(
    socket: Arc<UdpSocket>,
    stats: Arc<StreamStats>,
    mut cancel: watch::Receiver<bool>,
    mut pause: watch::Receiver<bool>,
    feedback_enabled: bool,
    single_port_ack: Option<[u8; UDP_HELLO_SIZE]>,
) -> anyhow::Result<(UdpStats, u64)> {
    let mut buffer = vec![0u8; UDP_PAYLOAD_SIZE + 100];
    let mut jitter_calc = JitterCalculator::new();
    let mut packet_tracker = PacketTracker::new();
    let mut packets_received: u64 = 0;
    let mut last_recv = Instant::now();
    // Most-recently-observed client source address. The first successful
    // recv populates it; later recvs refresh it (handles NAT rebinding
    // mid-test as a side benefit). Feedback emission targets this address.
    let mut last_client_addr: Option<SocketAddr> = None;

    // 2 Hz feedback emission. Skip missed ticks: every feedback packet
    // carries cumulative counts, so the next tick is self-correcting and
    // there's no value in sending a backlog of stale snapshots when the
    // task wakes up after a busy period.
    let mut feedback_timer = if feedback_enabled {
        let mut t = tokio::time::interval(Duration::from_millis(500));
        // First tick fires immediately; advance past it so we don't emit
        // before any data has arrived.
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Some(t)
    } else {
        None
    };

    loop {
        if *cancel.borrow() {
            debug!("UDP receive cancelled");
            break;
        }

        if crate::pause::is_paused(&pause) {
            if crate::pause::wait_while_paused(&mut pause, &mut cancel).await {
                break;
            }
            last_recv = Instant::now();
            continue;
        }

        // Check for client inactivity (handles abrupt disconnects)
        if last_recv.elapsed() > UDP_INACTIVITY_TIMEOUT {
            debug!(
                "UDP receive timeout: no packets for {:?}",
                UDP_INACTIVITY_TIMEOUT
            );
            break;
        }

        // Use recv_from for unconnected sockets, recv for connected
        let recv_future = socket.recv_from(&mut buffer);
        let timeout_future = tokio::time::sleep(Duration::from_millis(100));

        tokio::select! {
            result = recv_future => {
                match result {
                    Ok((n, addr)) => {
                        let recv_time = Instant::now();
                        last_recv = recv_time;
                        last_client_addr = Some(addr);

                        // Length-first demux: a 36-byte packet whose first
                        // four bytes spell "XFRF" is a feedback packet that
                        // shouldn't be on this socket (server doesn't emit
                        // feedback to itself; client's feedback recv lives
                        // in a separate function). Skip it without feeding
                        // into the data tracker — its sequence-bytes would
                        // record as a wildly-out-of-band number and inflate
                        // PacketTracker's lost count by ~6e18. Also do not
                        // count it toward bytes_received: feedback is a
                        // control-plane sideband and bytes_received tracks
                        // test-data wire bandwidth. Defense-in-depth for
                        // any future change that lets feedback land on a
                        // shared socket.
                        let is_feedback = n == UDP_FEEDBACK_SIZE
                            && buffer[0..4] == UDP_FEEDBACK_MAGIC;
                        if is_feedback {
                            continue;
                        }
                        // Single-port hello lane (issue #63): skipped from all
                        // accounting on both ends for the same reasons as
                        // feedback — a 24-byte hello/ack would otherwise decode
                        // as a data header with a wild sequence number. On the
                        // server (ack configured), a retried hello also gets
                        // re-acked here in case the dispatcher's ack was lost.
                        if n == UDP_HELLO_SIZE && buffer[0..4] == UDP_HELLO_MAGIC {
                            if let Some(ack) = single_port_ack
                                && UdpHelloPacket::decode(&buffer[..n])
                                    .is_some_and(|pkt| pkt.kind == UDP_HELLO_KIND_HELLO)
                                && let Err(e) = socket.send(&ack).await
                            {
                                debug!("single-port hello re-ack failed: {}", e);
                            }
                            continue;
                        }
                        stats.add_bytes_received(n as u64);

                        if let Some(header) = UdpPacketHeader::decode(&buffer[..n]) {
                            // Only count valid xfr packets toward UDP loss
                            // accounting (both live and final). A short or
                            // foreign datagram still adds to `bytes_received`
                            // because it consumed wire, but counting it
                            // toward the loss denominator would understate
                            // the loss percent — the denominator must match
                            // what the sequence tracker actually saw.
                            packets_received += 1;
                            stats.add_udp_received(1);

                            let old_lost = packet_tracker.lost.load(Ordering::Relaxed);
                            packet_tracker.record(header.sequence);
                            let new_lost = packet_tracker.lost.load(Ordering::Relaxed);

                            // Update live stats for interval reporting
                            let jitter_us = jitter_calc.update(header.timestamp_us, recv_time);
                            stats.set_udp_jitter_us(jitter_us as u64);

                            // Add any newly detected lost packets
                            if new_lost > old_lost {
                                stats.add_udp_lost(new_lost - old_lost);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("UDP receive error: {}", e);
                    }
                }
            }
            _ = timeout_future => {
                // Check cancel and inactivity timeout again
            }
            _ = async { feedback_timer.as_mut().unwrap().tick().await }, if feedback_timer.is_some() => {
                // Emit a receiver-progress feedback packet back to the
                // sender. Gates: must have a peer address from at least
                // one prior recv, and must have decoded at least one
                // valid xfr packet (otherwise we're talking to phantom
                // traffic that doesn't expect feedback).
                if let Some(addr) = last_client_addr
                    && packets_received > 0
                {
                    let snap = stats.udp_progress_snapshot();
                    let elapsed_ms = stats.start_time.elapsed().as_millis() as u64;
                    let pkt = UdpFeedbackPacket {
                        stream_id: stats.stream_id as u16,
                        elapsed_ms,
                        packets_received: snap.packets_received,
                        packets_lost: snap.packets_lost,
                    };
                    let mut buf = [0u8; UDP_FEEDBACK_SIZE];
                    if pkt.encode(&mut buf) {
                        // Connected single-port sockets must use send():
                        // the address equals the connected peer anyway,
                        // and macOS rejects send_to with EISCONN.
                        let result = if single_port_ack.is_some() {
                            socket.send(&buf).await
                        } else {
                            socket.send_to(&buf, addr).await
                        };
                        if let Err(e) = result {
                            // Non-fatal: ICMP port-unreachable from a
                            // closed peer surfaces as ConnectionRefused
                            // here. Log at debug; next tick retries.
                            debug!(
                                "UDP feedback send to {} failed: {}",
                                addr, e
                            );
                        }
                    }
                }
            }
        }
    }

    let (lost, out_of_order, _) =
        packet_tracker.stats(packets_received + packet_tracker.lost.load(Ordering::Relaxed));
    let packets_sent = packets_received + lost;
    let loss_percent = if packets_sent > 0 {
        (lost as f64 / packets_sent as f64) * 100.0
    } else {
        0.0
    };

    Ok((
        UdpStats {
            packets_sent,
            packets_received,
            lost,
            lost_percent: loss_percent,
            jitter_ms: jitter_calc.jitter_ms(),
            out_of_order,
            jitter_max_ms: Some(jitter_calc.jitter_max_ms()),
            packet_size: Some(UDP_PAYLOAD_SIZE as u32),
        },
        packets_sent,
    ))
}

/// Receive `UdpFeedbackPacket`s from the server on the client's
/// upload-mode UDP socket.
///
/// Spawned alongside the client-side `send_udp_paced` task so the
/// connected socket carries data in one direction and feedback in the
/// other. Length-first demux: 36-byte packets that decode as feedback
/// are written into `aggregator[stream_id]` for the per-test 1 Hz
/// consumer task to pick up. Anything else (including unexpected data
/// echoes or random short runts) is silently discarded — the client in
/// upload mode owns no PacketTracker for this socket and must not
/// pollute throughput or loss accounting from feedback noise.
///
/// Drains the socket for up to ~100 ms after `cancel` fires before
/// exiting, so feedback packets that landed just before the data
/// streams stopped are still observed in the per-test summary.
pub async fn receive_udp_feedback_only(
    socket: Arc<UdpSocket>,
    aggregator: crate::client::UdpFeedbackAggregator,
    stream_index: usize,
    mut cancel: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut buffer = vec![0u8; UDP_PAYLOAD_SIZE + 100];

    let cancel_changed = async move {
        // Wait until cancel becomes true. `changed()` returns on any
        // value transition, so loop until we see the actual `true`.
        loop {
            if *cancel.borrow() {
                return;
            }
            if cancel.changed().await.is_err() {
                return;
            }
        }
    };
    tokio::pin!(cancel_changed);

    loop {
        tokio::select! {
            result = socket.recv(&mut buffer) => {
                match result {
                    Ok(n) => {
                        // Length-first demux. Anything not exactly 36 bytes
                        // is not a feedback packet; ignore it.
                        if n != UDP_FEEDBACK_SIZE {
                            continue;
                        }
                        if let Some(pkt) = UdpFeedbackPacket::decode(&buffer[..n]) {
                            // Cumulative counts are monotonic per-stream,
                            // so we can safely overwrite our per-stream
                            // slot — older counts can never be ahead.
                            let mut slots = aggregator.lock();
                            if let Some(slot) = slots.get_mut(stream_index) {
                                *slot = pkt.to_progress();
                            }
                        }
                    }
                    Err(e) => {
                        debug!("UDP feedback recv error on stream {}: {}", stream_index, e);
                    }
                }
            }
            _ = &mut cancel_changed => {
                // Cancel fired. Drain for ~100ms so a final in-flight
                // feedback packet can still update our slot before the
                // consumer task stops emitting and the test summary
                // renders.
                let drain_deadline = Instant::now() + Duration::from_millis(100);
                while Instant::now() < drain_deadline {
                    let remaining = drain_deadline.saturating_duration_since(Instant::now());
                    tokio::select! {
                        result = socket.recv(&mut buffer) => {
                            if let Ok(n) = result
                                && n == UDP_FEEDBACK_SIZE
                                && let Some(pkt) = UdpFeedbackPacket::decode(&buffer[..n])
                            {
                                let mut slots = aggregator.lock();
                                if let Some(slot) = slots.get_mut(stream_index) {
                                    *slot = pkt.to_progress();
                                }
                            }
                        }
                        _ = tokio::time::sleep(remaining) => break,
                    }
                }
                break;
            }
        }
    }
    Ok(())
}

/// Server side of `--probe-mtu` (issue #64, `mtu_probe_v1`): answer each
/// incoming `XFRP` probe with a small ack plus a same-size echo, both
/// addressed to wherever the probe came from. The ack is header-sized so
/// it survives nearly any path and proves the forward direction; the
/// echo is padded to the probe's declared size and proves the reverse
/// direction. Runs until `cancel` fires (the control channel owns test
/// lifetime, same as the bulk handlers).
///
/// The echo is sent with don't-fragment set by the caller on the socket;
/// an oversized echo then dies at the constraining hop instead of being
/// fragmented into deceptive success. EMSGSIZE on the echo send means
/// the server's own interface MTU is the reverse-path limit — that's a
/// legitimate probe outcome, not an error, so it's logged at debug and
/// the ack still goes out.
///
/// `single_port_ack` is `Some` when the probe runs in single-port mode
/// (issue #63): the socket is connected, so replies use `send()` (macOS
/// rejects `send_to` on connected sockets), and retried stream hellos
/// that land here after the dispatcher's ack was lost get re-acked so
/// the client's handshake can complete.
pub async fn respond_mtu_probes(
    socket: Arc<UdpSocket>,
    mut cancel: watch::Receiver<bool>,
    single_port_ack: Option<[u8; UDP_HELLO_SIZE]>,
) -> anyhow::Result<()> {
    use crate::probe::{PROBE_KIND_ACK, PROBE_KIND_ECHO, PROBE_KIND_PROBE, ProbePacket};

    let mut recv_buf = vec![0u8; crate::probe::MAX_PROBE_PAYLOAD + 100];
    let mut send_buf = vec![0u8; crate::probe::MAX_PROBE_PAYLOAD];

    let cancel_changed = async move {
        loop {
            if *cancel.borrow() {
                return;
            }
            if cancel.changed().await.is_err() {
                return;
            }
        }
    };
    tokio::pin!(cancel_changed);

    loop {
        tokio::select! {
            result = socket.recv_from(&mut recv_buf) => {
                let (n, from) = match result {
                    Ok(v) => v,
                    Err(e) => {
                        debug!("MTU probe recv error: {}", e);
                        continue;
                    }
                };
                if let Some(hello_ack) = single_port_ack
                    && UdpHelloPacket::decode(&recv_buf[..n])
                        .is_some_and(|pkt| pkt.kind == UDP_HELLO_KIND_HELLO)
                {
                    if let Err(e) = socket.send(&hello_ack).await {
                        debug!("single-port hello re-ack failed during probe: {}", e);
                    }
                    continue;
                }
                let Some(pkt) = ProbePacket::decode(&recv_buf[..n]) else {
                    continue;
                };
                if pkt.kind != PROBE_KIND_PROBE {
                    continue;
                }
                // Connected single-port sockets must reply with send():
                // `from` equals the connected peer, and macOS rejects
                // send_to on a connected socket with EISCONN.
                let connected = single_port_ack.is_some();
                let ack = ProbePacket {
                    kind: PROBE_KIND_ACK,
                    seq: pkt.seq,
                    declared_size: pkt.declared_size,
                };
                if let Some(len) = ack.encode(&mut send_buf) {
                    let result = if connected {
                        socket.send(&send_buf[..len]).await
                    } else {
                        socket.send_to(&send_buf[..len], from).await
                    };
                    if let Err(e) = result {
                        debug!("MTU probe ack send failed (seq {}): {}", pkt.seq, e);
                    }
                }
                let echo = ProbePacket {
                    kind: PROBE_KIND_ECHO,
                    seq: pkt.seq,
                    declared_size: pkt.declared_size,
                };
                if let Some(len) = echo.encode(&mut send_buf) {
                    let result = if connected {
                        socket.send(&send_buf[..len]).await
                    } else {
                        socket.send_to(&send_buf[..len], from).await
                    };
                    if let Err(e) = result {
                        // EMSGSIZE here is the reverse path speaking, not a bug.
                        debug!("MTU probe echo send failed (seq {}, {}B): {}", pkt.seq, len, e);
                    }
                }
            }
            _ = &mut cancel_changed => break,
        }
    }
    Ok(())
}

/// Wait for the first packet from a client and return their address.
/// Used in server reverse mode to learn where to send data.
pub async fn wait_for_client(socket: &UdpSocket, timeout: Duration) -> anyhow::Result<SocketAddr> {
    let mut buffer = [0u8; 64];

    tokio::select! {
        result = socket.recv_from(&mut buffer) => {
            match result {
                Ok((_, addr)) => {
                    debug!("UDP client connected from {}", addr);
                    Ok(addr)
                }
                Err(e) => Err(anyhow::anyhow!("Failed to receive from client: {}", e)),
            }
        }
        _ = tokio::time::sleep(timeout) => {
            Err(anyhow::anyhow!("Timeout waiting for UDP client"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_header_roundtrip() {
        let header = UdpPacketHeader {
            sequence: 12345,
            timestamp_us: 67890,
        };
        let mut buffer = [0u8; 16];
        assert!(header.encode(&mut buffer));

        let decoded = UdpPacketHeader::decode(&buffer).unwrap();
        assert_eq!(decoded.sequence, 12345);
        assert_eq!(decoded.timestamp_us, 67890);
    }

    #[test]
    fn feedback_packet_roundtrip() {
        let pkt = UdpFeedbackPacket {
            stream_id: 7,
            elapsed_ms: 12_345,
            packets_received: 999_999,
            packets_lost: 4_242,
        };
        let mut buffer = [0u8; UDP_FEEDBACK_SIZE];
        assert!(pkt.encode(&mut buffer));
        let decoded = UdpFeedbackPacket::decode(&buffer).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn feedback_packet_rejects_wrong_magic() {
        let pkt = UdpFeedbackPacket {
            stream_id: 0,
            elapsed_ms: 0,
            packets_received: 0,
            packets_lost: 0,
        };
        let mut buffer = [0u8; UDP_FEEDBACK_SIZE];
        assert!(pkt.encode(&mut buffer));
        // Corrupt the magic
        buffer[0..4].copy_from_slice(b"DATA");
        assert!(UdpFeedbackPacket::decode(&buffer).is_none());
    }

    #[test]
    fn feedback_packet_rejects_short_buffer() {
        let buffer = [0u8; UDP_FEEDBACK_SIZE - 1];
        assert!(UdpFeedbackPacket::decode(&buffer).is_none());
    }

    #[test]
    fn feedback_packet_rejects_oversize_buffer() {
        // A 36-byte feedback packet is fixed-size; a 37-byte buffer is
        // either a bug or an attempt to exploit length confusion.
        let buffer = [0u8; UDP_FEEDBACK_SIZE + 1];
        assert!(UdpFeedbackPacket::decode(&buffer).is_none());
    }

    #[test]
    fn feedback_packet_rejects_unknown_version() {
        let pkt = UdpFeedbackPacket {
            stream_id: 0,
            elapsed_ms: 0,
            packets_received: 0,
            packets_lost: 0,
        };
        let mut buffer = [0u8; UDP_FEEDBACK_SIZE];
        assert!(pkt.encode(&mut buffer));
        buffer[4] = 2; // version=2
        assert!(UdpFeedbackPacket::decode(&buffer).is_none());
    }

    #[test]
    fn feedback_packet_rejects_unknown_kind() {
        let pkt = UdpFeedbackPacket {
            stream_id: 0,
            elapsed_ms: 0,
            packets_received: 0,
            packets_lost: 0,
        };
        let mut buffer = [0u8; UDP_FEEDBACK_SIZE];
        assert!(pkt.encode(&mut buffer));
        buffer[5] = 99; // unknown kind
        assert!(UdpFeedbackPacket::decode(&buffer).is_none());
    }

    #[test]
    fn data_packet_at_collision_sequence_is_not_feedback() {
        // Length-first demux means a hypothetical data packet whose first
        // four bytes spell "XFRF" is still routed as data. Synthesize the
        // worst case: a full 1416-byte data packet with sequence chosen so
        // its big-endian top 4 bytes are b"XFRF". A naive magic-only
        // demux would misroute it; ours selects on length first.
        let collision_seq = u64::from_be_bytes([b'X', b'F', b'R', b'F', 0, 0, 0, 0]);
        let header = UdpPacketHeader {
            sequence: collision_seq,
            timestamp_us: 1_000,
        };
        let mut buffer = vec![0u8; UDP_HEADER_SIZE + UDP_PAYLOAD_SIZE];
        assert!(header.encode(&mut buffer));
        // Length is not 36, so feedback decode must reject.
        assert!(UdpFeedbackPacket::decode(&buffer).is_none());
        // Header decode still succeeds.
        let decoded = UdpPacketHeader::decode(&buffer).unwrap();
        assert_eq!(decoded.sequence, collision_seq);
    }

    #[test]
    fn test_jitter_calculator() {
        let mut calc = JitterCalculator::new();
        let start = Instant::now();

        // First packet doesn't produce jitter
        calc.update(0, start);
        assert_eq!(calc.jitter_ms(), 0.0);

        // Second packet with same timing should have minimal jitter
        calc.update(1000, start + Duration::from_micros(1000));
        // Jitter should be close to 0
        assert!(calc.jitter_ms() < 1.0);
    }

    #[test]
    fn test_jitter_max_tracks_peak_not_current() {
        let mut calc = JitterCalculator::new();
        let start = Instant::now();

        // Prime with two in-sync samples so the running jitter starts at zero.
        calc.update(0, start);
        calc.update(1000, start + Duration::from_micros(1000));
        assert_eq!(calc.jitter_max_ms(), 0.0);

        // Introduce a single large timing delta (recv arrives 10ms late).
        calc.update(2000, start + Duration::from_micros(12_000));
        let peak = calc.jitter_max_ms();
        assert!(peak > 0.0, "non-trivial delta should set a max");

        // Continue with *in-sync* samples (send delta == recv delta) so
        // |D| is zero and the running jitter decays by 15/16 each step.
        // jitter_ms should drop below peak, but jitter_max_ms must stick.
        let mut send_us: u64 = 3000;
        let mut recv_us: u64 = 13_000;
        for _ in 0..20 {
            calc.update(send_us, start + Duration::from_micros(recv_us));
            send_us += 1000;
            recv_us += 1000;
        }
        assert!(
            calc.jitter_ms() < peak,
            "running jitter should decay below peak when timing is in sync again"
        );
        assert_eq!(
            calc.jitter_max_ms(),
            peak,
            "jitter_max must retain the prior peak, not track the current value"
        );
    }

    #[test]
    fn hello_packet_roundtrip() {
        let pkt = UdpHelloPacket {
            kind: UDP_HELLO_KIND_HELLO,
            stream_index: 17,
            token: [0xAB; UDP_HELLO_TOKEN_LEN],
        };
        let buffer = pkt.encode();
        assert_eq!(UdpHelloPacket::decode(&buffer), Some(pkt));

        let ack = UdpHelloPacket {
            kind: UDP_HELLO_KIND_ACK,
            stream_index: 0,
            token: [0; UDP_HELLO_TOKEN_LEN],
        };
        let buffer = ack.encode();
        assert_eq!(UdpHelloPacket::decode(&buffer), Some(ack));
    }

    #[test]
    fn hello_packet_rejects_foreign() {
        let pkt = UdpHelloPacket {
            kind: UDP_HELLO_KIND_HELLO,
            stream_index: 1,
            token: [7; UDP_HELLO_TOKEN_LEN],
        };
        let good = pkt.encode();

        // Wrong magic
        let mut bad = good;
        bad[0..4].copy_from_slice(b"XFRF");
        assert!(UdpHelloPacket::decode(&bad).is_none());

        // Unknown version
        let mut bad = good;
        bad[4] = 2;
        assert!(UdpHelloPacket::decode(&bad).is_none());

        // Unknown kind
        let mut bad = good;
        bad[5] = 99;
        assert!(UdpHelloPacket::decode(&bad).is_none());

        // Wrong length, both directions — fixed-size framing
        assert!(UdpHelloPacket::decode(&good[..UDP_HELLO_SIZE - 1]).is_none());
        let mut oversize = [0u8; UDP_HELLO_SIZE + 1];
        oversize[..UDP_HELLO_SIZE].copy_from_slice(&good);
        assert!(UdpHelloPacket::decode(&oversize).is_none());
    }

    #[test]
    fn hello_token_hex_roundtrip() {
        let token: [u8; UDP_HELLO_TOKEN_LEN] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ];
        let hex = encode_hello_token(&token);
        assert_eq!(hex, "00112233445566778899aabbccddeeff");
        assert_eq!(parse_hello_token(&hex), Some(token));

        assert!(parse_hello_token("").is_none());
        assert!(parse_hello_token("0011").is_none()); // too short
        assert!(parse_hello_token("zz112233445566778899aabbccddeeff").is_none()); // non-hex
        assert!(parse_hello_token("00112233445566778899aabbccddeeff00").is_none()); // too long
    }

    #[test]
    fn classify_shared_datagram_all_lanes() {
        // 1. Exact hello framing → xfr hello lane.
        let hello = UdpHelloPacket {
            kind: UDP_HELLO_KIND_HELLO,
            stream_index: 0,
            token: [1; UDP_HELLO_TOKEN_LEN],
        }
        .encode();
        assert_eq!(classify_shared_datagram(&hello), SharedSocketLane::XfrHello);

        // 2. QUIC long header: 0x80 set (RFC 8999, version-independent).
        let long_header = [0xC3u8, 0, 0, 0, 1];
        assert_eq!(
            classify_shared_datagram(&long_header),
            SharedSocketLane::Quic
        );

        // 3. QUIC short header: 0x40 fixed bit set, 0x80 clear.
        let short_header = [0x41u8, 9, 9, 9];
        assert_eq!(
            classify_shared_datagram(&short_header),
            SharedSocketLane::Quic
        );

        // 4. Top two bits clear and not a hello → stray xfr lane. This is
        // what a *greased* short header (fixed bit cleared, RFC 9287)
        // would look like — it must never be possible toward this server
        // because the endpoint sets grease_quic_bit(false), so routing it
        // away from quinn is correct rather than lossy.
        let greased_looking = [0x05u8, 1, 2, 3];
        assert_eq!(
            classify_shared_datagram(&greased_looking),
            SharedSocketLane::XfrStray
        );
        assert_eq!(classify_shared_datagram(&[]), SharedSocketLane::XfrStray);

        // Hello magic with the wrong length is not the hello lane (fixed
        // size is part of the match), and its first byte is 0x00 → stray.
        let truncated_hello = &hello[..UDP_HELLO_SIZE - 4];
        assert_eq!(
            classify_shared_datagram(truncated_hello),
            SharedSocketLane::XfrStray
        );
    }

    /// The client handshake must skip an ack carrying the wrong token or
    /// stream index (token is the route key — a mismatched ack proves
    /// nothing about our stream) and complete on the matching one.
    #[tokio::test]
    async fn handshake_ignores_mismatched_ack() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(server_addr).await.unwrap();

        let token = [0x42u8; UDP_HELLO_TOKEN_LEN];
        let responder = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let (_, from) = server.recv_from(&mut buf).await.unwrap();
            // Wrong token first, then wrong stream, then the real ack.
            let wrong_token = UdpHelloPacket {
                kind: UDP_HELLO_KIND_ACK,
                stream_index: 3,
                token: [0xFFu8; UDP_HELLO_TOKEN_LEN],
            };
            let wrong_stream = UdpHelloPacket {
                kind: UDP_HELLO_KIND_ACK,
                stream_index: 4,
                token,
            };
            let good = UdpHelloPacket {
                kind: UDP_HELLO_KIND_ACK,
                stream_index: 3,
                token,
            };
            for pkt in [wrong_token, wrong_stream, good] {
                server.send_to(&pkt.encode(), from).await.unwrap();
            }
        });

        tokio::time::timeout(
            Duration::from_secs(5),
            single_port_udp_handshake(&client, &token, 3),
        )
        .await
        .expect("handshake should not hit the retry budget")
        .expect("handshake should succeed on the matching ack");
        responder.await.unwrap();
    }

    #[test]
    fn test_packet_tracker() {
        let mut tracker = PacketTracker::new();

        // Sequential packets
        tracker.record(0);
        tracker.record(1);
        tracker.record(2);
        assert_eq!(tracker.lost.load(Ordering::Relaxed), 0);

        // Gap - packet 3 lost
        tracker.record(4);
        assert_eq!(tracker.lost.load(Ordering::Relaxed), 1);

        // Out of order
        tracker.record(3);
        assert_eq!(tracker.out_of_order.load(Ordering::Relaxed), 1);
    }

    /// Verify the pacing ticker doesn't burst after resume from pause.
    ///
    /// `MissedTickBehavior::Skip` drops queued ticks during pause, and
    /// `ticker.reset()` after resume makes the first post-pause tick fire
    /// a full interval from resume.  Without both, the sender would
    /// fire immediately on resume, emitting a burst of packets.
    #[tokio::test]
    async fn test_udp_paced_no_burst_after_resume() {
        use std::sync::Arc;
        use tokio::sync::watch;

        // Bound the test so it can't hang.
        let overall = tokio::time::timeout(Duration::from_secs(10), async {
            // Two connected UDP sockets on localhost.
            let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let server_addr = server.local_addr().unwrap();
            client.connect(server_addr).await.unwrap();

            let socket = Arc::new(client);
            let stats = Arc::new(StreamStats::new(0));

            let (cancel_tx, cancel_rx) = watch::channel(false);
            let (pause_tx, pause_rx) = watch::channel(false);

            // 80kbps, 1200-byte payloads → ~8.3 packets/s → ~120ms interval.
            // Slow enough that a burst is observable; fast enough that the
            // test completes quickly.
            let bitrate: u64 = 80_000;

            let send_socket = socket.clone();
            let send_stats = stats.clone();
            let send_task = tokio::spawn(async move {
                send_udp_paced(
                    send_socket,
                    None,
                    bitrate,
                    Duration::ZERO, // infinite — cancel ends it
                    send_stats,
                    cancel_rx,
                    pause_rx,
                    false,
                )
                .await
            });

            // The client socket is connected, so send_udp_paced must use
            // send(), not send_to(); macOS rejects send_to on connected UDP
            // sockets with EISCONN.
            let mut buf = [0u8; 2048];
            tokio::time::timeout(Duration::from_secs(2), server.recv(&mut buf))
                .await
                .expect("paced sender should deliver before pause")
                .expect("server recv should succeed");

            // Let it send for a bit, then pause.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = pause_tx.send(true);

            // Drain any in-flight packets.
            let drain_deadline = tokio::time::Instant::now() + Duration::from_millis(200);
            while let Ok(Ok(_)) =
                tokio::time::timeout_at(drain_deadline, server.recv(&mut buf)).await
            {}

            // Resume from pause.
            let _ = pause_tx.send(false);
            // The assertions below verify timing: no burst, then a packet.
            // The first post-resume packet must NOT arrive immediately
            // (that would indicate a burst from a queued tick).  It
            // should arrive after roughly one pacing interval (~120ms).
            // We assert nothing arrives in the first half-interval.
            let half_interval = Duration::from_millis(60);
            let early_result = tokio::time::timeout(half_interval, server.recv(&mut buf)).await;
            assert!(
                early_result.is_err(),
                "no packet should arrive in first {:?} after resume (burst detected)",
                half_interval
            );

            // Then a packet should arrive within a generous window.
            // The pacing interval is ~120ms; reset() makes the first
            // tick fire ~120ms from resume, but allow scheduling jitter.
            let recv_window = Duration::from_secs(2);
            let recv_result = tokio::time::timeout(recv_window, server.recv(&mut buf)).await;
            assert!(
                recv_result.is_ok(),
                "a packet should arrive within {:?} after resume",
                recv_window
            );

            // Clean up.
            let _ = cancel_tx.send(true);
            let _ = send_task.await;
        })
        .await;
        assert!(overall.is_ok(), "test should complete within 10s");
    }
}
