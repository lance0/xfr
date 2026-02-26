use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::time::Instant;

use crate::protocol::{AggregateInterval, StreamInterval, StreamResult, TcpInfoSnapshot, UdpStats};

/// Maximum number of intervals to keep in history (1 minute at 1-second intervals)
const MAX_INTERVAL_HISTORY: usize = 60;

pub struct StreamStats {
    pub stream_id: u8,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
    pub start_time: Instant,
    pub intervals: Mutex<VecDeque<IntervalStats>>,
    pub retransmits: AtomicU64,
    pub interval_retransmits_total: AtomicU64,
    pub last_bytes: AtomicU64,
    pub last_tcp_retransmits: AtomicU64,
    // UDP stats (updated live by receiver)
    pub udp_jitter_us: AtomicU64, // Jitter in microseconds (convert to ms when reading)
    pub udp_lost: AtomicU64,      // Total lost packets
    pub last_udp_lost: AtomicU64, // For interval calculation
    /// Raw file descriptor for TCP_INFO polling (-1 = not set)
    pub tcp_info_fd: AtomicI32,
}

#[derive(Debug, Clone)]
pub struct IntervalStats {
    pub timestamp: Instant,
    pub bytes: u64,
    pub throughput_mbps: f64,
    pub retransmits: u64,
    pub jitter_ms: f64,
    pub lost: u64,
    pub rtt_us: Option<u32>,
    pub cwnd: Option<u32>,
}

impl StreamStats {
    pub fn new(stream_id: u8) -> Self {
        Self {
            stream_id,
            bytes_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            start_time: Instant::now(),
            intervals: Mutex::new(VecDeque::new()),
            retransmits: AtomicU64::new(0),
            interval_retransmits_total: AtomicU64::new(0),
            last_bytes: AtomicU64::new(0),
            last_tcp_retransmits: AtomicU64::new(0),
            udp_jitter_us: AtomicU64::new(0),
            udp_lost: AtomicU64::new(0),
            last_udp_lost: AtomicU64::new(0),
            tcp_info_fd: AtomicI32::new(-1),
        }
    }

    /// Store raw file descriptor for TCP_INFO polling.
    /// Must be called before the TcpStream is moved or split.
    pub fn set_tcp_info_fd(&self, fd: i32) {
        self.tcp_info_fd.store(fd, Ordering::Relaxed);
    }

    /// Clear the stored fd to prevent stale fd reuse after stream closes.
    pub fn clear_tcp_info_fd(&self) {
        self.tcp_info_fd.store(-1, Ordering::Relaxed);
    }

    /// Poll current TCP_INFO snapshot using stored fd.
    /// Returns None if fd is not set or getsockopt fails.
    pub fn poll_tcp_info(&self) -> Option<TcpInfoSnapshot> {
        let fd = self.tcp_info_fd.load(Ordering::Relaxed);
        if fd < 0 {
            return None;
        }
        crate::tcp_info::get_tcp_info_from_fd(fd).ok()
    }

    pub fn add_bytes_sent(&self, bytes: u64) {
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn add_bytes_received(&self, bytes: u64) {
        self.bytes_received.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn add_retransmits(&self, count: u64) {
        self.retransmits.fetch_add(count, Ordering::Relaxed);
    }

    /// Update live UDP jitter (in microseconds)
    pub fn set_udp_jitter_us(&self, jitter_us: u64) {
        self.udp_jitter_us.store(jitter_us, Ordering::Relaxed);
    }

    /// Add lost packets to cumulative count
    pub fn add_udp_lost(&self, count: u64) {
        self.udp_lost.fetch_add(count, Ordering::Relaxed);
    }

    /// Get current jitter in milliseconds
    pub fn udp_jitter_ms(&self) -> f64 {
        self.udp_jitter_us.load(Ordering::Relaxed) as f64 / 1000.0
    }

    pub fn total_bytes(&self) -> u64 {
        self.bytes_sent.load(Ordering::Relaxed) + self.bytes_received.load(Ordering::Relaxed)
    }

    pub fn retransmits(&self) -> u64 {
        self.retransmits.load(Ordering::Relaxed)
    }

    pub fn throughput_mbps(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            (self.total_bytes() as f64 * 8.0) / (elapsed * 1_000_000.0)
        } else {
            0.0
        }
    }

    pub fn record_interval(&self) -> IntervalStats {
        let now = Instant::now();
        let total_bytes = self.total_bytes();
        let last_bytes = self.last_bytes.swap(total_bytes, Ordering::Relaxed);
        let interval_bytes = total_bytes.saturating_sub(last_bytes);

        let elapsed = now.duration_since(self.start_time);
        let intervals = self.intervals.lock();
        let interval_duration = if let Some(last) = intervals.back() {
            now.duration_since(last.timestamp)
        } else {
            elapsed
        };
        drop(intervals);

        let throughput_mbps = if interval_duration.as_secs_f64() > 0.0 {
            (interval_bytes as f64 * 8.0) / (interval_duration.as_secs_f64() * 1_000_000.0)
        } else {
            0.0
        };

        // UDP stats for this interval
        let jitter_ms = self.udp_jitter_ms();
        let total_lost = self.udp_lost.load(Ordering::Relaxed);
        let last_lost = self.last_udp_lost.swap(total_lost, Ordering::Relaxed);
        let interval_lost = total_lost.saturating_sub(last_lost);

        // TCP_INFO: live RTT, cwnd, and retransmit deltas
        let tcp_info = self.poll_tcp_info();
        let interval_retransmits = if let Some(ref info) = tcp_info {
            let total = info.retransmits;
            let last = self.last_tcp_retransmits.swap(total, Ordering::Relaxed);
            total.saturating_sub(last)
        } else {
            0
        };
        if interval_retransmits > 0 {
            let _ = self.interval_retransmits_total.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |current| Some(current.saturating_add(interval_retransmits)),
            );
        }
        let stats = IntervalStats {
            timestamp: now,
            bytes: interval_bytes,
            throughput_mbps,
            retransmits: interval_retransmits,
            jitter_ms,
            lost: interval_lost,
            rtt_us: tcp_info.as_ref().map(|t| t.rtt_us),
            cwnd: tcp_info.as_ref().map(|t| t.cwnd),
        };

        let mut intervals = self.intervals.lock();
        intervals.push_back(stats.clone());
        // Keep only last MAX_INTERVAL_HISTORY intervals to bound memory
        if intervals.len() > MAX_INTERVAL_HISTORY {
            intervals.pop_front();
        }
        drop(intervals);
        stats
    }

    pub fn to_interval(&self, interval_stats: &IntervalStats) -> StreamInterval {
        StreamInterval {
            id: self.stream_id,
            bytes: interval_stats.bytes,
            retransmits: Some(interval_stats.retransmits),
            jitter_ms: if interval_stats.jitter_ms > 0.0 {
                Some(interval_stats.jitter_ms)
            } else {
                None
            },
            lost: if interval_stats.lost > 0 {
                Some(interval_stats.lost)
            } else {
                None
            },
            error: None,
            rtt_us: interval_stats.rtt_us,
            cwnd: interval_stats.cwnd,
        }
    }

    pub fn to_result(&self, duration_ms: u64) -> StreamResult {
        let bytes = self.total_bytes();
        let throughput_mbps = if duration_ms > 0 {
            (bytes as f64 * 8.0) / (duration_ms as f64 / 1000.0) / 1_000_000.0
        } else {
            0.0
        };

        StreamResult {
            id: self.stream_id,
            bytes,
            throughput_mbps,
            retransmits: Some(
                self.retransmits()
                    .max(self.interval_retransmits_total.load(Ordering::Relaxed)),
            ),
            jitter_ms: None,
            lost: None,
        }
    }
}

pub struct TestStats {
    pub test_id: String,
    pub streams: Vec<Arc<StreamStats>>,
    pub start_time: Instant,
    /// Per-stream TCP info snapshots (indexed by stream_id)
    pub tcp_info: Mutex<Vec<TcpInfoSnapshot>>,
    /// Per-stream UDP stats (indexed by stream_id)
    pub udp_stats: Mutex<Vec<UdpStats>>,
}

impl TestStats {
    pub fn new(test_id: String, num_streams: u8) -> Self {
        let streams = (0..num_streams)
            .map(|i| Arc::new(StreamStats::new(i)))
            .collect();
        Self {
            test_id,
            streams,
            start_time: Instant::now(),
            tcp_info: Mutex::new(Vec::new()),
            udp_stats: Mutex::new(Vec::new()),
        }
    }

    pub fn add_udp_stats(&self, stats: UdpStats) {
        self.udp_stats.lock().push(stats);
    }

    /// Aggregate all UDP stats across streams
    pub fn aggregate_udp_stats(&self) -> Option<UdpStats> {
        let stats = self.udp_stats.lock();
        if stats.is_empty() {
            return None;
        }

        let mut total = UdpStats {
            packets_sent: 0,
            packets_received: 0,
            lost: 0,
            lost_percent: 0.0,
            jitter_ms: 0.0,
            out_of_order: 0,
        };

        for s in stats.iter() {
            total.packets_sent += s.packets_sent;
            total.packets_received += s.packets_received;
            total.lost += s.lost;
            total.out_of_order += s.out_of_order;
            total.jitter_ms += s.jitter_ms;
        }

        // Average jitter across streams
        total.jitter_ms /= stats.len() as f64;

        // Recalculate loss percent from totals
        if total.packets_sent > 0 {
            total.lost_percent = (total.lost as f64 / total.packets_sent as f64) * 100.0;
        }

        Some(total)
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    pub fn record_intervals(&self) -> Vec<IntervalStats> {
        self.streams.iter().map(|s| s.record_interval()).collect()
    }

    pub fn to_aggregate(&self, intervals: &[IntervalStats]) -> AggregateInterval {
        let total_bytes: u64 = intervals.iter().map(|i| i.bytes).sum();
        let total_throughput: f64 = intervals.iter().map(|i| i.throughput_mbps).sum();
        let total_retransmits: u64 = intervals.iter().map(|i| i.retransmits).sum();
        let total_lost: u64 = intervals.iter().map(|i| i.lost).sum();

        // Average jitter across streams that have jitter data
        let jitter_values: Vec<f64> = intervals
            .iter()
            .filter(|i| i.jitter_ms > 0.0)
            .map(|i| i.jitter_ms)
            .collect();
        let avg_jitter = if jitter_values.is_empty() {
            None
        } else {
            Some(jitter_values.iter().sum::<f64>() / jitter_values.len() as f64)
        };

        // Average RTT across streams that have TCP_INFO
        let rtt_values: Vec<u32> = intervals.iter().filter_map(|i| i.rtt_us).collect();
        let avg_rtt = if rtt_values.is_empty() {
            None
        } else {
            Some(
                (rtt_values.iter().map(|&r| r as u64).sum::<u64>() / rtt_values.len() as u64)
                    as u32,
            )
        };

        // Sum cwnd across streams (total sending capacity)
        let cwnd_values: Vec<u32> = intervals.iter().filter_map(|i| i.cwnd).collect();
        let total_cwnd = if cwnd_values.is_empty() {
            None
        } else {
            Some(cwnd_values.iter().sum())
        };

        AggregateInterval {
            bytes: total_bytes,
            throughput_mbps: total_throughput,
            retransmits: Some(total_retransmits),
            jitter_ms: avg_jitter,
            lost: if total_lost > 0 {
                Some(total_lost)
            } else {
                None
            },
            rtt_us: avg_rtt,
            cwnd: total_cwnd,
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.streams.iter().map(|s| s.total_bytes()).sum()
    }

    pub fn add_tcp_info(&self, info: TcpInfoSnapshot) {
        self.tcp_info.lock().push(info);
    }

    /// Get final TCP info (last snapshot represents post-transfer state)
    pub fn get_tcp_info(&self) -> Option<TcpInfoSnapshot> {
        let infos = self.tcp_info.lock();
        infos.last().cloned()
    }

    /// Poll local TCP_INFO across all streams for live client-side metrics.
    /// Returns (avg_rtt_us, total_retransmits, total_cwnd) if any stream has a valid fd.
    pub fn poll_local_tcp_info(&self) -> Option<(u32, u64, u32)> {
        let mut rtt_sum = 0u64;
        let mut retransmits = 0u64;
        let mut cwnd = 0u32;
        let mut count = 0u64;
        for stream in &self.streams {
            if let Some(info) = stream.poll_tcp_info() {
                rtt_sum += info.rtt_us as u64;
                retransmits += info.retransmits;
                cwnd += info.cwnd;
                count += 1;
            }
        }
        if count > 0 {
            Some(((rtt_sum / count) as u32, retransmits, cwnd))
        } else {
            None
        }
    }
}

pub fn bytes_to_human(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Normalize a float for display at a given decimal precision.
/// Handles both IEEE 754 `-0.0` and tiny negatives (e.g. `-1e-12`)
/// that would round to `-0.0` at the given precision.
#[inline]
pub fn normalize_for_display(v: f64, decimals: i32) -> f64 {
    let scale = 10f64.powi(decimals);
    let rounded = (v * scale).round() / scale;
    if rounded == 0.0 { 0.0 } else { rounded }
}

pub fn mbps_to_human(mbps: f64) -> String {
    // Branch on rounded value so e.g. 999.95 displays as "1.00 Gbps" not "1000.0 Mbps"
    let rounded_1dp = normalize_for_display(mbps, 1);
    if rounded_1dp >= 1000.0 {
        format!("{:.2} Gbps", normalize_for_display(mbps / 1000.0, 2))
    } else {
        format!("{:.1} Mbps", rounded_1dp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_stats_bytes() {
        let stats = StreamStats::new(0);
        stats.add_bytes_sent(1000);
        stats.add_bytes_received(500);
        assert_eq!(stats.total_bytes(), 1500);
    }

    #[test]
    fn test_to_result_uses_max_retransmit_source() {
        let stats = StreamStats::new(0);

        // Interval-based total can be higher when final TCP_INFO snapshot is unavailable.
        stats.retransmits.store(3, Ordering::Relaxed);
        stats.interval_retransmits_total.store(7, Ordering::Relaxed);
        let result = stats.to_result(1000);
        assert_eq!(result.retransmits, Some(7));

        // Final TCP_INFO snapshot can include tail retransmits after last interval.
        stats.retransmits.store(11, Ordering::Relaxed);
        stats.interval_retransmits_total.store(7, Ordering::Relaxed);
        let result = stats.to_result(1000);
        assert_eq!(result.retransmits, Some(11));
    }

    #[test]
    fn test_bytes_to_human() {
        assert_eq!(bytes_to_human(500), "500 B");
        assert_eq!(bytes_to_human(1024), "1.00 KB");
        assert_eq!(bytes_to_human(1024 * 1024), "1.00 MB");
        assert_eq!(bytes_to_human(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn test_mbps_to_human() {
        assert_eq!(mbps_to_human(500.0), "500.0 Mbps");
        assert_eq!(mbps_to_human(1500.0), "1.50 Gbps");
    }

    #[test]
    fn test_mbps_to_human_negative_zero() {
        assert_eq!(mbps_to_human(-0.0), "0.0 Mbps");
        assert_eq!(mbps_to_human(-0.0 / 1.0), "0.0 Mbps");
    }

    #[test]
    fn test_mbps_to_human_boundary() {
        // 999.95 rounds to 1000.0 at 1dp — should switch to Gbps
        assert_eq!(mbps_to_human(999.95), "1.00 Gbps");
        // 999.94 rounds to 999.9 — stays Mbps
        assert_eq!(mbps_to_human(999.94), "999.9 Mbps");
        // Exact boundary
        assert_eq!(mbps_to_human(1000.0), "1.00 Gbps");
        assert_eq!(mbps_to_human(999.9), "999.9 Mbps");
    }

    #[test]
    fn test_normalize_for_display_special_values() {
        // NaN propagates (not a display concern — throughput is never NaN)
        assert!(normalize_for_display(f64::NAN, 1).is_nan());
        // Infinity propagates
        assert_eq!(normalize_for_display(f64::INFINITY, 1), f64::INFINITY);
        assert_eq!(
            normalize_for_display(f64::NEG_INFINITY, 1),
            f64::NEG_INFINITY
        );
    }

    #[test]
    fn test_normalize_for_display() {
        // Exact -0.0 normalizes to 0.0
        assert_eq!(normalize_for_display(-0.0, 1), 0.0);
        assert!(normalize_for_display(-0.0, 1).is_sign_positive());

        // Tiny negative that rounds to zero at given precision
        assert_eq!(normalize_for_display(-1e-12, 1), 0.0);
        assert!(normalize_for_display(-1e-12, 1).is_sign_positive());

        // Normal values pass through
        assert_eq!(normalize_for_display(42.56, 1), 42.6);
        assert_eq!(normalize_for_display(-5.3, 1), -5.3);

        // Zero stays zero
        assert_eq!(normalize_for_display(0.0, 1), 0.0);
        assert!(normalize_for_display(0.0, 1).is_sign_positive());
    }
}
