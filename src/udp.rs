//! UDP data transfer with pacing
//!
//! Implements paced UDP sending to avoid buffer saturation and provides
//! jitter/loss calculation per RFC 3550.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::time::interval;
use tracing::{debug, warn};

use crate::protocol::UdpStats;
use crate::stats::StreamStats;

pub const UDP_PAYLOAD_SIZE: usize = 1400; // Leave room for IP + UDP headers
const UDP_HEADER_SIZE: usize = 16; // sequence (8) + timestamp_us (8)

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
    pub fn encode(&self, buffer: &mut [u8]) {
        buffer[0..8].copy_from_slice(&self.sequence.to_be_bytes());
        buffer[8..16].copy_from_slice(&self.timestamp_us.to_be_bytes());
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
}

impl JitterCalculator {
    pub fn new() -> Self {
        Self {
            last_send_time: None,
            last_recv_time: None,
            jitter: 0.0,
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
        }

        self.last_send_time = Some(send_time_us);
        self.last_recv_time = Some(recv_time);
        self.jitter
    }

    pub fn jitter_ms(&self) -> f64 {
        self.jitter / 1000.0
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

/// Send UDP data at a paced rate
pub async fn send_udp_paced(
    socket: Arc<UdpSocket>,
    target_bitrate: u64,
    duration: Duration,
    stats: Arc<StreamStats>,
    cancel: watch::Receiver<bool>,
) -> anyhow::Result<UdpSendStats> {
    let packet_size = UDP_PAYLOAD_SIZE;
    let bits_per_packet = (packet_size * 8) as u64;

    // Use floating-point for precision in interval calculation
    let packets_per_sec_f64 = target_bitrate as f64 / bits_per_packet as f64;

    // For high PPS, batch multiple packets per interval to reduce timer overhead
    let (pacing_interval, packets_per_tick) = if packets_per_sec_f64 > HIGH_PPS_THRESHOLD {
        // High PPS: batch BURST_SIZE packets per interval
        let interval = Duration::from_secs_f64(BURST_SIZE as f64 / packets_per_sec_f64);
        (interval, BURST_SIZE)
    } else if packets_per_sec_f64 > 0.0 {
        // Normal PPS: one packet per interval
        let interval = Duration::from_secs_f64(1.0 / packets_per_sec_f64);
        (interval, 1)
    } else {
        (Duration::from_millis(1), 1)
    };

    debug!(
        "UDP pacing: {:.0} packets/sec, interval {:?}, {} packets/tick",
        packets_per_sec_f64, pacing_interval, packets_per_tick
    );

    let mut sequence: u64 = 0;
    let mut ticker = interval(pacing_interval);
    let start = Instant::now();
    let deadline = start + duration;

    let mut packet = vec![0u8; packet_size];

    loop {
        if *cancel.borrow() {
            debug!("UDP send cancelled");
            break;
        }

        ticker.tick().await;

        if Instant::now() >= deadline {
            break;
        }

        // Send packets_per_tick packets in this burst
        for _ in 0..packets_per_tick {
            if Instant::now() >= deadline {
                break;
            }

            // Build packet with relative timestamp
            let now_us = start.elapsed().as_micros() as u64;
            let header = UdpPacketHeader {
                sequence,
                timestamp_us: now_us,
            };
            header.encode(&mut packet);

            match socket.send(&packet).await {
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

/// Receive UDP data and track statistics
pub async fn receive_udp(
    socket: Arc<UdpSocket>,
    stats: Arc<StreamStats>,
    cancel: watch::Receiver<bool>,
) -> anyhow::Result<(UdpStats, u64)> {
    let mut buffer = vec![0u8; UDP_PAYLOAD_SIZE + 100];
    let mut jitter_calc = JitterCalculator::new();
    let mut packet_tracker = PacketTracker::new();
    let mut packets_received: u64 = 0;

    loop {
        if *cancel.borrow() {
            debug!("UDP receive cancelled");
            break;
        }

        // Use recv_from for unconnected sockets, recv for connected
        let recv_future = socket.recv_from(&mut buffer);
        let timeout_future = tokio::time::sleep(Duration::from_millis(100));

        tokio::select! {
            result = recv_future => {
                match result {
                    Ok((n, _addr)) => {
                        let recv_time = Instant::now();
                        stats.add_bytes_received(n as u64);
                        packets_received += 1;

                        if let Some(header) = UdpPacketHeader::decode(&buffer[..n]) {
                            packet_tracker.record(header.sequence);
                            jitter_calc.update(header.timestamp_us, recv_time);
                        }
                    }
                    Err(e) => {
                        warn!("UDP receive error: {}", e);
                    }
                }
            }
            _ = timeout_future => {
                // Check cancel again
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
        },
        packets_sent,
    ))
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
        header.encode(&mut buffer);

        let decoded = UdpPacketHeader::decode(&buffer).unwrap();
        assert_eq!(decoded.sequence, 12345);
        assert_eq!(decoded.timestamp_us, 67890);
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
