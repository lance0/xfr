use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
    pub last_bytes: AtomicU64,
    pub last_retransmits: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct IntervalStats {
    pub timestamp: Instant,
    pub bytes: u64,
    pub throughput_mbps: f64,
    pub retransmits: u64,
    pub jitter_ms: f64,
    pub lost: u64,
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
            last_bytes: AtomicU64::new(0),
            last_retransmits: AtomicU64::new(0),
        }
    }

    pub fn add_bytes_sent(&self, bytes: u64) {
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn add_bytes_received(&self, bytes: u64) {
        self.bytes_received.fetch_add(bytes, Ordering::Relaxed);
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

        let total_retransmits = self.retransmits.load(Ordering::Relaxed);
        let last_retransmits = self
            .last_retransmits
            .swap(total_retransmits, Ordering::Relaxed);
        let interval_retransmits = total_retransmits.saturating_sub(last_retransmits);

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

        let stats = IntervalStats {
            timestamp: now,
            bytes: interval_bytes,
            throughput_mbps,
            retransmits: interval_retransmits,
            jitter_ms: 0.0,
            lost: 0,
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
            jitter_ms: None,
            lost: None,
            error: None,
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
            retransmits: Some(self.retransmits.load(Ordering::Relaxed)),
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

        AggregateInterval {
            bytes: total_bytes,
            throughput_mbps: total_throughput,
            retransmits: Some(total_retransmits),
            jitter_ms: None,
            lost: None,
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.streams.iter().map(|s| s.total_bytes()).sum()
    }

    pub fn add_tcp_info(&self, info: TcpInfoSnapshot) {
        self.tcp_info.lock().push(info);
    }

    /// Get a single representative TCP info (first stream's info, or aggregated)
    pub fn get_tcp_info(&self) -> Option<TcpInfoSnapshot> {
        let infos = self.tcp_info.lock();
        infos.first().cloned()
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

pub fn mbps_to_human(mbps: f64) -> String {
    if mbps >= 1000.0 {
        format!("{:.2} Gbps", mbps / 1000.0)
    } else {
        format!("{:.1} Mbps", mbps)
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
}
