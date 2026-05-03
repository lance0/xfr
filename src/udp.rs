//! UDP data transfer with pacing
//!
//! Implements paced UDP sending to avoid buffer saturation and provides
//! jitter/loss calculation per RFC 3550.

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
    let mut ticker = interval(pacing_interval);
    let start = Instant::now();
    let deadline = start + duration;
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

        if *pause.borrow() {
            if crate::pause::wait_while_paused(&mut pause, &mut cancel).await {
                break;
            }
            continue;
        }

        // Wait for ticker, interruptible by cancel/pause
        tokio::select! {
            biased;
            _ = cancel.changed() => {
                if *cancel.borrow() { break; }
                continue;
            }
            _ = pause.changed() => { continue; } // re-check at top
            _ = ticker.tick() => {}
        }

        // Duration::ZERO means infinite - only check deadline if finite
        if !is_infinite && Instant::now() >= deadline {
            break;
        }

        // Send packets_per_tick packets in this burst
        for _ in 0..packets_per_tick {
            if *cancel.borrow() || *pause.borrow() {
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
    let deadline = start + duration;
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

        if *pause.borrow() {
            if crate::pause::wait_while_paused(&mut pause, &mut cancel).await {
                break;
            }
            continue;
        }

        // Duration::ZERO means infinite - only check deadline if finite
        if !is_infinite && Instant::now() >= deadline {
            break;
        }

        // Send a burst of packets before yielding
        for _ in 0..BURST_SIZE {
            if *cancel.borrow() || *pause.borrow() {
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
pub async fn receive_udp(
    socket: Arc<UdpSocket>,
    stats: Arc<StreamStats>,
    mut cancel: watch::Receiver<bool>,
    mut pause: watch::Receiver<bool>,
    feedback_enabled: bool,
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

        if *pause.borrow() {
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
                        stats.add_bytes_received(n as u64);

                        // Length-first demux: a 36-byte packet whose first
                        // four bytes spell "XFRF" is a feedback packet that
                        // shouldn't be on this socket (server doesn't emit
                        // feedback to itself; client's feedback recv lives
                        // in a separate function). Skip it without feeding
                        // into the data tracker — its sequence-bytes would
                        // record as a wildly-out-of-band number and inflate
                        // PacketTracker's lost count by ~6e18.
                        let is_feedback = n == UDP_FEEDBACK_SIZE
                            && buffer[0..4] == UDP_FEEDBACK_MAGIC;

                        if !is_feedback
                            && let Some(header) = UdpPacketHeader::decode(&buffer[..n])
                        {
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
                    if pkt.encode(&mut buf)
                        && let Err(e) = socket.send_to(&buf, addr).await
                    {
                        // Non-fatal: ICMP port-unreachable from a
                        // closed peer surfaces as ConnectionRefused
                        // here. Log at debug; next tick retries.
                        debug!(
                            "UDP feedback send_to({}) failed: {}",
                            addr, e
                        );
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
}
