//! Keep control commands and sampling independent of a backpressured writer.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncWrite, AsyncWriteExt};
// The active clock runs on Tokio's clock so paused-time tests can drive it.
use tokio::sync::{Mutex, mpsc};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::control_crypto::ControlWriter;
use crate::protocol::{ControlMessage, Direction, Protocol};
use crate::stats::{IntervalStats, TestStats};
use crate::tui::server::ServerEvent;

use super::{
    ActiveTest, ControlCommand, STATS_INTERVAL, budget_transfer_complete, finish_drain_grace,
};

const CANCEL_FLUSH_TIMEOUT: Duration = Duration::from_secs(3);
/// Bound on terminal output after a normal ending: a slow but live client
/// still gets its Result, while a peer that never reads releases the test.
const RESULT_FLUSH_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct ReportLoopConfig<'a> {
    pub(super) id: &'a str,
    pub(super) protocol: Protocol,
    pub(super) duration: Duration,
    pub(super) direction: Direction,
    pub(super) stats: &'a TestStats,
    pub(super) active_tests: &'a Arc<Mutex<HashMap<String, ActiveTest>>>,
    pub(super) tui_tx: Option<&'a mpsc::Sender<ServerEvent>>,
    pub(super) byte_budget_test: bool,
    pub(super) end_on_server_send: bool,
}

/// One wire frame is immutable once encoded: protected encoding consumes a
/// sequence number, and replacing a partially written frame corrupts both
/// plaintext line framing and protected length/sequence framing.
struct Frame {
    bytes: Vec<u8>,
    offset: usize,
}

impl Frame {
    fn encode(codec: &mut ControlWriter, serialized: &str) -> io::Result<Self> {
        Ok(Self {
            bytes: codec.encode_message(serialized)?,
            offset: 0,
        })
    }

    async fn write_once<W: AsyncWrite + Unpin>(&mut self, writer: &mut W) -> io::Result<()> {
        // Unlike write_all, write is cancellation-safe in select!: if another
        // branch wins, no bytes were written. Advance only after its completion.
        let written = writer.write(&self.bytes[self.offset..]).await?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "control writer stopped accepting a frame",
            ));
        }
        self.offset += written;
        Ok(())
    }

    fn complete(&self) -> bool {
        self.offset == self.bytes.len()
    }

    async fn finish<W: AsyncWrite + Unpin>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.bytes[self.offset..]).await?;
        self.offset = self.bytes.len();
        Ok(())
    }
}

/// The pending interval retains deltas rather than serialized frames. Sampling
/// can then continue indefinitely with at most one frame and one pending sample.
struct Sample {
    intervals: Vec<IntervalStats>,
    durations: Vec<Duration>,
    elapsed_ms: u64,
}

impl Sample {
    fn capture(stats: &TestStats, elapsed_ms: u64) -> Self {
        // record_intervals appends the new timestamps. Read each predecessor
        // first, including zero-byte periods, so coalescing keeps the true span.
        let previous: Vec<_> = stats
            .streams
            .iter()
            .map(|stream| {
                stream
                    .intervals
                    .lock()
                    .back()
                    .map_or(stream.start_time, |interval| interval.timestamp)
            })
            .collect();
        let intervals = stats.record_intervals();
        let durations = intervals
            .iter()
            .zip(previous)
            .map(|(interval, previous)| interval.timestamp.saturating_duration_since(previous))
            .collect();
        Self {
            intervals,
            durations,
            elapsed_ms,
        }
    }

    fn merge(&mut self, newer: Self) {
        debug_assert_eq!(self.intervals.len(), newer.intervals.len());
        for ((older, duration), (mut newer, newer_duration)) in self
            .intervals
            .iter_mut()
            .zip(&mut self.durations)
            .zip(newer.intervals.into_iter().zip(newer.durations))
        {
            newer.bytes = older.bytes.saturating_add(newer.bytes);
            newer.retransmits = older.retransmits.saturating_add(newer.retransmits);
            newer.lost = older.lost.saturating_add(newer.lost);
            newer.bytes_sent = older.bytes_sent.saturating_add(newer.bytes_sent);
            newer.bytes_received = older.bytes_received.saturating_add(newer.bytes_received);
            *duration = duration.saturating_add(newer_duration);
            newer.throughput_mbps = if duration.is_zero() {
                0.0
            } else {
                newer.bytes as f64 * 8.0 / duration.as_secs_f64() / 1_000_000.0
            };
            // Gauges and cumulative UDP counts remain the newest snapshot;
            // only interval deltas and their spans are additive.
            *older = newer;
        }
        self.elapsed_ms = newer.elapsed_ms;
    }

    fn serialize(&self, config: &ReportLoopConfig<'_>) -> anyhow::Result<String> {
        let streams = config
            .stats
            .streams
            .iter()
            .zip(&self.intervals)
            .map(|(stream, interval)| {
                stream.to_interval_with_direction(interval, config.direction == Direction::Bidir)
            })
            .collect();
        let aggregate = config
            .stats
            .to_aggregate_with_direction(&self.intervals, config.direction == Direction::Bidir);
        ControlMessage::Interval {
            id: config.id.to_owned(),
            elapsed_ms: self.elapsed_ms,
            streams,
            aggregate,
        }
        .serialize()
    }
}

/// Test time excluding pauses, including one still in progress.
fn active_elapsed(
    start: Instant,
    paused_total: Duration,
    pause_start: Option<Instant>,
) -> Duration {
    let active_pause = pause_start.map(|p| p.elapsed()).unwrap_or_default();
    start.elapsed().saturating_sub(paused_total + active_pause)
}

pub(super) struct Reporting {
    start: Instant,
    paused_total: Duration,
    pause_start: Option<Instant>,
    /// Active time at which a `-n` transfer was complete, when that preceded
    /// the end of the loop (the client's `Finish` trails the last byte).
    transfer_end: Option<Duration>,
    codec: Option<ControlWriter>,
    current: Option<Frame>,
    pending: Option<String>,
    cancelled: Option<String>,
    pub(super) finish_deadline: tokio::time::Instant,
}

impl Reporting {
    /// Active test duration so far. Read it before `finish`, so terminal
    /// flushing does not count as test time.
    ///
    /// A `-n` transfer can finish in under a millisecond on a fast path. Its
    /// duration rounds up, and is at least 1 ms, so every throughput derived
    /// from the whole-millisecond wire field is non-zero and never overstated.
    pub(super) fn duration_ms(&self) -> u64 {
        match self.transfer_end {
            Some(end) => (end.as_micros() as u64).div_ceil(1000).max(1),
            None => {
                active_elapsed(self.start, self.paused_total, self.pause_start).as_millis() as u64
            }
        }
    }

    /// Finish the exact in-flight frame before any terminal messages. A timeout
    /// consumes the codec too: callers must terminate this stream on error,
    /// because abandoning even one byte or sequence number makes reuse unsafe.
    pub(super) async fn finish<W: AsyncWrite + Unpin>(
        &mut self,
        writer: &mut W,
        result_serialized: &str,
    ) -> anyhow::Result<()> {
        let mut codec = self
            .codec
            .take()
            .ok_or_else(|| anyhow::anyhow!("control reporting was already finished"))?;
        let current = self.current.take();
        let pending = self.pending.take();
        let cancelled = self.cancelled.take();
        let flush = async {
            if let Some(mut frame) = current {
                frame.finish(writer).await?;
            }
            for serialized in pending.iter().chain(cancelled.iter()) {
                Frame::encode(&mut codec, serialized)?
                    .finish(writer)
                    .await?;
            }
            Frame::encode(&mut codec, result_serialized)?
                .finish(writer)
                .await?;
            writer.flush().await?;
            Ok::<_, anyhow::Error>(())
        };
        tokio::time::timeout_at(self.finish_deadline, flush)
            .await
            .map_err(|_| anyhow::anyhow!("timed out flushing terminal control messages"))?
    }
}

pub(super) async fn run<W: AsyncWrite + Unpin>(
    writer: &mut W,
    codec: ControlWriter,
    config: ReportLoopConfig<'_>,
    cmd_rx: &mut mpsc::Receiver<ControlCommand>,
    sending_done_rx: &mut mpsc::Receiver<()>,
    recv_done_rx: &mut mpsc::Receiver<()>,
) -> anyhow::Result<Reporting> {
    let result = run_inner(
        writer,
        codec,
        &config,
        cmd_rx,
        sending_done_rx,
        recv_done_rx,
    )
    .await;
    // Stop data tasks before flushing terminal control output, including on a
    // serialization or write error. A peer that never reads cannot keep them alive.
    if let Some(test) = config.active_tests.lock().await.get(config.id) {
        let _ = test.cancel_tx.send(true);
    }
    result
}

async fn run_inner<W: AsyncWrite + Unpin>(
    writer: &mut W,
    mut codec: ControlWriter,
    config: &ReportLoopConfig<'_>,
    cmd_rx: &mut mpsc::Receiver<ControlCommand>,
    sending_done_rx: &mut mpsc::Receiver<()>,
    recv_done_rx: &mut mpsc::Receiver<()>,
) -> anyhow::Result<Reporting> {
    // Ticks share the active clock's origin, so with no pause the final tick
    // lands exactly on the finite deadline.
    let start = Instant::now();
    let mut timer = tokio::time::interval_at(start + STATS_INTERVAL, STATS_INTERVAL);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut paused_total = Duration::ZERO;
    let mut pause_start = None;
    let mut current: Option<Frame> = None;
    let mut pending: Option<Sample> = None;
    let mut cancelled = false;
    let mut client_finished = false;
    let mut server_send_done = false;
    let mut drain_until = None;
    // `-n` completion times, so the reported duration ends at the last byte
    // rather than at whichever control message arrived last.
    let mut received_at: Option<Duration> = None;
    let mut sent_at: Option<Duration> = None;

    loop {
        let elapsed = active_elapsed(start, paused_total, pause_start);
        if !config.duration.is_zero() && elapsed >= config.duration {
            break;
        }
        if current.is_none()
            && let Some(sample) = pending.take()
        {
            current = Some(Frame::encode(&mut codec, &sample.serialize(config)?)?);
        }
        let duration_until = if config.duration.is_zero() || pause_start.is_some() {
            None
        } else {
            // Pauses can leave the deadline between ticks; waking on it keeps
            // the Result duration from overshooting by up to one interval.
            Some(start + paused_total + config.duration)
        };

        tokio::select! {
            _ = async {
                match duration_until {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            }, if duration_until.is_some() => break,
            _ = async {
                match drain_until {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            }, if drain_until.is_some() => {
                warn!(id = config.id, protocol = ?config.protocol,
                    "drain window expired after client Finish; the tail may not have arrived");
                client_finished = true;
                received_at.get_or_insert(active_elapsed(start, paused_total, pause_start));
                drain_until = None;
                if budget_transfer_complete(config.direction, client_finished, server_send_done) {
                    break;
                }
            }
            // TCP and QUIC receive loops can reach EOF before the client's
            // Finish arrives; that, not the Finish, is when the upload ended.
            // UDP has no EOF (its channel is closed from the start).
            _ = recv_done_rx.recv(), if drain_until.is_some()
                || (config.byte_budget_test
                    && config.protocol != Protocol::Udp
                    && received_at.is_none()) =>
            {
                received_at.get_or_insert(active_elapsed(start, paused_total, pause_start));
                if drain_until.is_some() {
                    debug!(id = config.id, "drained after Finish, ending test");
                    client_finished = true;
                    drain_until = None;
                    if budget_transfer_complete(config.direction, client_finished, server_send_done) {
                        break;
                    }
                }
            }
            _ = sending_done_rx.recv(), if config.end_on_server_send && !server_send_done => {
                server_send_done = true;
                sent_at = Some(active_elapsed(start, paused_total, pause_start));
                if budget_transfer_complete(config.direction, client_finished, server_send_done) {
                    debug!(id = config.id, "byte budget delivered, ending test");
                    break;
                }
            }
            _ = timer.tick() => {
                let elapsed = active_elapsed(start, paused_total, pause_start);
                // The final tick coincides with the duration deadline. Whichever
                // select! picks, end without sampling it, so a timed test
                // always reports the same rows; Result carries the tail.
                if !config.duration.is_zero() && elapsed >= config.duration {
                    break;
                }
                let elapsed_ms = elapsed.as_millis() as u64;
                let sample = Sample::capture(config.stats, elapsed_ms);
                if let Some(tx) = config.tui_tx {
                    let aggregate = config.stats.to_aggregate_with_direction(
                        &sample.intervals, config.direction == Direction::Bidir,
                    );
                    let _ = tx.try_send(ServerEvent::TestUpdated {
                        id: config.id.to_owned(),
                        bytes: aggregate.bytes,
                        throughput_mbps: aggregate.throughput_mbps,
                    });
                }
                match pending.as_mut() {
                    Some(pending) => pending.merge(sample),
                    None => pending = Some(sample),
                }
            }
            cmd = cmd_rx.recv() => match cmd {
                Some(ControlCommand::Finish) if config.byte_budget_test => {
                    if !client_finished && drain_until.is_none() {
                        debug!(id = config.id, "client finished sending, draining");
                        drain_until = Some(tokio::time::Instant::now() + finish_drain_grace(config.protocol));
                    }
                }
                Some(ControlCommand::Finish) => {
                    warn!(id = config.id, "ignoring Finish on a test with no byte budget");
                }
                Some(ControlCommand::Cancel) => {
                    info!(id = config.id, "test cancelled by client");
                    cancelled = true;
                    break;
                }
                Some(ControlCommand::Pause) => {
                    info!(id = config.id, "test paused");
                    if pause_start.is_none() {
                        pause_start = Some(Instant::now());
                    }
                    if let Some(test) = config.active_tests.lock().await.get(config.id) {
                        let _ = test.pause_tx.send(true);
                    }
                }
                Some(ControlCommand::Resume) => {
                    info!(id = config.id, "test resumed");
                    if let Some(paused) = pause_start.take() {
                        paused_total = paused_total.saturating_add(paused.elapsed());
                    }
                    if let Some(test) = config.active_tests.lock().await.get(config.id) {
                        let _ = test.pause_tx.send(false);
                    }
                }
                None => {
                    debug!(id = config.id, "control reader exited");
                    break;
                }
            },
            written = async {
                match current.as_mut() {
                    Some(frame) => frame.write_once(writer).await,
                    None => std::future::pending().await,
                }
            }, if current.is_some() => {
                written?;
                if current.as_ref().is_some_and(Frame::complete) {
                    current = None;
                }
            }
        }
    }

    let transfer_end = if cancelled
        || !config.byte_budget_test
        || !budget_transfer_complete(config.direction, client_finished, server_send_done)
    {
        None
    } else {
        match config.direction {
            Direction::Upload => received_at,
            Direction::Download => sent_at,
            Direction::Bidir => received_at.max(sent_at),
        }
    };
    let finish_deadline = tokio::time::Instant::now()
        + if cancelled {
            CANCEL_FLUSH_TIMEOUT
        } else {
            RESULT_FLUSH_TIMEOUT
        };
    let pending = if cancelled {
        None
    } else {
        pending.map(|sample| sample.serialize(config)).transpose()?
    };
    let cancelled = if cancelled {
        Some(
            ControlMessage::Cancelled {
                id: config.id.to_owned(),
            }
            .serialize()?,
        )
    } else {
        None
    };
    Ok(Reporting {
        start,
        paused_total,
        pause_start,
        transfer_end,
        codec: Some(codec),
        current,
        pending,
        cancelled,
        finish_deadline,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_crypto::{ControlCodec, ControlReader};
    use tokio::io::AsyncReadExt;
    use tokio::sync::watch;

    struct BlockedReporter {
        task: tokio::task::JoinHandle<anyhow::Result<Reporting>>,
        commands: mpsc::Sender<ControlCommand>,
        sending_done: mpsc::Sender<()>,
        receiving_done: mpsc::Sender<()>,
        cancelled: watch::Receiver<bool>,
        paused: watch::Receiver<bool>,
        stats: Arc<TestStats>,
        // Keep the reader alive without accepting the rest of the first frame.
        _peer: tokio::io::DuplexStream,
    }

    impl Drop for BlockedReporter {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn blocked_reporter(
        protocol: Protocol,
        direction: Direction,
        duration: Duration,
    ) -> BlockedReporter {
        let stats = Arc::new(TestStats::new("blocked".to_owned(), 1));
        let (cancel_tx, cancelled) = watch::channel(false);
        let (pause_tx, paused) = watch::channel(false);
        let active_tests = Arc::new(Mutex::new(HashMap::from([(
            "blocked".to_owned(),
            ActiveTest {
                stats: stats.clone(),
                cancel_tx,
                pause_tx,
                data_ports: Vec::new(),
                data_stream_tx: None,
                control_peer_ip: std::net::Ipv4Addr::LOCALHOST.into(),
                expected_streams: 1,
            },
        )])));
        let (commands, mut cmd_rx) = mpsc::channel(8);
        let (sending_done, mut sending_done_rx) = mpsc::channel(1);
        let (receiving_done, mut recv_done_rx) = mpsc::channel(1);
        let (mut writer, mut peer) = tokio::io::duplex(1);
        let task_stats = stats.clone();
        let task = tokio::spawn(async move {
            run(
                &mut writer,
                ControlWriter::Plaintext,
                ReportLoopConfig {
                    id: "blocked",
                    protocol,
                    duration,
                    direction,
                    stats: &task_stats,
                    active_tests: &active_tests,
                    tui_tx: None,
                    byte_budget_test: duration.is_zero(),
                    end_on_server_send: duration.is_zero() && direction != Direction::Upload,
                },
                &mut cmd_rx,
                &mut sending_done_rx,
                &mut recv_done_rx,
            )
            .await
        });
        // Reading two bytes from a one-byte stream proves the writer has
        // started the first frame. Leaving the remainder unread blocks it.
        let mut prefix = [0; 2];
        tokio::time::timeout(Duration::from_secs(3), peer.read_exact(&mut prefix))
            .await
            .expect("first interval must begin")
            .unwrap();
        BlockedReporter {
            task,
            commands,
            sending_done,
            receiving_done,
            cancelled,
            paused,
            stats,
            _peer: peer,
        }
    }

    fn interval(timestamp: std::time::Instant, bytes: u64) -> IntervalStats {
        IntervalStats {
            timestamp,
            bytes,
            throughput_mbps: 0.0,
            retransmits: 0,
            jitter_ms: 0.0,
            lost: 0,
            rtt_us: None,
            cwnd: None,
            bytes_sent: 0,
            bytes_received: 0,
            cumulative_packets_received: 0,
            cumulative_packets_lost: 0,
        }
    }

    fn sample(intervals: Vec<IntervalStats>, seconds: &[u64], elapsed_ms: u64) -> Sample {
        Sample {
            intervals,
            durations: seconds.iter().copied().map(Duration::from_secs).collect(),
            elapsed_ms,
        }
    }

    #[test]
    fn capture_uses_predecessor_before_recording_new_interval() {
        let stats = TestStats::new("capture".to_owned(), 2);
        let predecessor = std::time::Instant::now() - Duration::from_secs(2);
        stats.streams[0]
            .intervals
            .lock()
            .push_back(interval(predecessor, 0));
        stats.streams[0].add_bytes_received(1_000_000);
        let captured = Sample::capture(&stats, 2_000);
        assert_eq!(
            captured.durations[0],
            captured.intervals[0].timestamp.duration_since(predecessor)
        );
        assert_eq!(
            captured.durations[1],
            captured.intervals[1]
                .timestamp
                .duration_since(stats.streams[1].start_time)
        );
        assert_eq!(captured.intervals[0].bytes, 1_000_000);
        assert!(captured.durations[0] >= Duration::from_secs(2));
    }

    #[test]
    fn coalescing_keeps_empty_spans_and_latest_gauges() {
        let now = std::time::Instant::now();
        let mut first = interval(now, 1_000_000);
        first.retransmits = 2;
        first.lost = 3;
        first.bytes_sent = 600_000;
        first.bytes_received = 400_000;
        first.jitter_ms = 1.0;
        first.rtt_us = Some(100);
        first.cwnd = Some(10);
        first.cumulative_packets_received = 7;
        first.cumulative_packets_lost = 3;
        let mut merged = sample(vec![first, interval(now, 500_000)], &[1, 2], 1_000);
        merged.merge(sample(
            vec![interval(now + Duration::from_secs(2), 0); 2],
            &[2, 2],
            3_000,
        ));
        let timestamp = now + Duration::from_secs(3);
        let mut newest = interval(timestamp, 1_000_000);
        newest.retransmits = 4;
        newest.lost = 5;
        newest.bytes_sent = 300_000;
        newest.bytes_received = 700_000;
        newest.jitter_ms = 2.5;
        newest.rtt_us = Some(200);
        newest.cwnd = Some(20);
        newest.cumulative_packets_received = 50;
        newest.cumulative_packets_lost = 8;
        merged.merge(sample(vec![newest, interval(timestamp, 0)], &[1, 1], 4_000));

        let interval = &merged.intervals[0];
        assert_eq!(interval.bytes, 2_000_000);
        assert_eq!(interval.retransmits, 6);
        assert_eq!(interval.lost, 8);
        assert_eq!(interval.bytes_sent, 900_000);
        assert_eq!(interval.bytes_received, 1_100_000);
        assert_eq!(
            merged.durations,
            [Duration::from_secs(4), Duration::from_secs(5)]
        );
        assert_eq!(interval.throughput_mbps, 4.0);
        assert_eq!(merged.intervals[1].throughput_mbps, 0.8);
        assert_eq!(interval.jitter_ms, 2.5);
        assert_eq!(interval.rtt_us, Some(200));
        assert_eq!(interval.cwnd, Some(20));
        assert_eq!(interval.cumulative_packets_received, 50);
        assert_eq!(interval.cumulative_packets_lost, 8);
        assert_eq!(interval.timestamp, timestamp);
        assert_eq!(merged.elapsed_ms, 4_000);

        let stats = TestStats::new("coalesced".to_owned(), 2);
        let aggregate = stats.to_aggregate_with_direction(&merged.intervals, true);
        assert_eq!(aggregate.bytes, 2_500_000);
        assert_eq!(aggregate.throughput_mbps, 4.8);
        assert_eq!(aggregate.bytes_sent, Some(1_100_000));
        assert_eq!(aggregate.bytes_received, Some(900_000));
        assert_eq!(aggregate.retransmits, Some(6));
        assert_eq!(aggregate.lost, Some(8));
        let progress = aggregate.udp_progress.unwrap();
        assert_eq!(progress.packets_received, 50);
        assert_eq!(progress.packets_lost, 8);
    }

    #[test]
    fn coalescing_saturates_delta_counters() {
        let now = std::time::Instant::now();
        let mut maximum = interval(now, u64::MAX);
        maximum.retransmits = u64::MAX;
        maximum.lost = u64::MAX;
        maximum.bytes_sent = u64::MAX;
        maximum.bytes_received = u64::MAX;
        let mut newer = interval(now, 1);
        newer.retransmits = 1;
        newer.lost = 1;
        newer.bytes_sent = 1;
        newer.bytes_received = 1;
        let mut merged = sample(vec![maximum], &[1], 1_000);
        merged.merge(sample(vec![newer], &[1], 2_000));
        let interval = &merged.intervals[0];
        assert_eq!(interval.bytes, u64::MAX);
        assert_eq!(interval.retransmits, u64::MAX);
        assert_eq!(interval.lost, u64::MAX);
        assert_eq!(interval.bytes_sent, u64::MAX);
        assert_eq!(interval.bytes_received, u64::MAX);
        assert!(interval.throughput_mbps.is_finite());
    }

    #[tokio::test]
    async fn partial_frames_resume_before_terminal_messages() {
        for protected in [false, true] {
            for cancelled in [false, true] {
                let (codec, mut reader) = if protected {
                    let (_, send) =
                        ControlCodec::derive_pair("client", "server", "secret").unwrap();
                    let (_, recv) =
                        ControlCodec::derive_pair("client", "server", "secret").unwrap();
                    (
                        ControlWriter::Protected { send },
                        ControlReader::Protected { recv },
                    )
                } else {
                    (ControlWriter::Plaintext, ControlReader::Plaintext)
                };
                let mut codec = codec;
                let mut current = Frame::encode(&mut codec, "in-flight interval").unwrap();
                // Enough room for only part of the first plaintext line or the
                // protected length prefix. Terminal output must resume that offset.
                let (mut writer, peer) = tokio::io::duplex(3);
                current.write_once(&mut writer).await.unwrap();
                assert_eq!(current.offset, 3);
                assert!(!current.complete());
                let mut reporting = Reporting {
                    start: Instant::now(),
                    paused_total: Duration::ZERO,
                    pause_start: None,
                    transfer_end: None,
                    codec: Some(codec),
                    current: Some(current),
                    pending: (!cancelled).then(|| "pending interval".to_owned()),
                    cancelled: cancelled.then(|| "cancelled".to_owned()),
                    finish_deadline: tokio::time::Instant::now() + CANCEL_FLUSH_TIMEOUT,
                };
                let receive = async {
                    let mut peer = tokio::io::BufReader::new(peer);
                    let middle = if cancelled {
                        "cancelled"
                    } else {
                        "pending interval"
                    };
                    for expected in ["in-flight interval", middle, "result"] {
                        let mut decoded = String::new();
                        reader.read_message(&mut peer, &mut decoded).await.unwrap();
                        assert_eq!(decoded.trim_end_matches('\n'), expected);
                    }
                };
                let (result, ()) = tokio::join!(reporting.finish(&mut writer, "result"), receive);
                result.unwrap();
                assert!(
                    reporting
                        .finish(&mut writer, "second result")
                        .await
                        .is_err()
                );
            }
        }
    }

    #[tokio::test]
    async fn terminal_timeout_consumes_codec_after_partial_frame() {
        let mut codec = ControlWriter::Plaintext;
        let mut current = Frame::encode(&mut codec, "in-flight interval").unwrap();
        let (mut writer, _peer) = tokio::io::duplex(3);
        current.write_once(&mut writer).await.unwrap();
        let mut reporting = Reporting {
            start: Instant::now(),
            paused_total: Duration::ZERO,
            pause_start: None,
            transfer_end: None,
            codec: Some(codec),
            current: Some(current),
            pending: None,
            cancelled: Some("cancelled".to_owned()),
            finish_deadline: tokio::time::Instant::now(),
        };
        let error = reporting.finish(&mut writer, "result").await.unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(reporting.codec.is_none());
        assert!(
            reporting
                .finish(&mut writer, "second result")
                .await
                .is_err()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn finish_drains_receive_side_and_waits_for_bidir_send_while_blocked() {
        for direction in [Direction::Upload, Direction::Bidir] {
            let mut reporter = blocked_reporter(Protocol::Tcp, direction, Duration::ZERO).await;
            reporter
                .commands
                .send(ControlCommand::Finish)
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(100), reporter.cancelled.changed())
                    .await
                    .is_err(),
                "Finish must not cancel a receive side that is still draining"
            );
            // Tail bytes can still arrive after Finish even though no interval
            // can reach the client. Final totals must retain those bytes.
            reporter.stats.streams[0].add_bytes_received(12_345);
            reporter.receiving_done.send(()).await.unwrap();
            if direction == Direction::Bidir {
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), reporter.cancelled.changed())
                        .await
                        .is_err(),
                    "bidir completion must wait for the server's send budget too"
                );
                reporter.sending_done.send(()).await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(1), reporter.cancelled.changed())
                .await
                .expect("drained transfer must stop despite the blocked writer")
                .unwrap();
            assert!(*reporter.cancelled.borrow());
            let reporting = (&mut reporter.task).await.unwrap().unwrap();
            assert!(reporting.cancelled.is_none(), "Finish is a normal ending");
            let frame = reporting.current.unwrap();
            assert!(frame.offset > 0 && !frame.complete());
            assert_eq!(reporter.stats.total_bytes(), 12_345);
        }
    }

    /// A `-n` test ends at its last byte: receive EOF that precedes the
    /// client's Finish, or the server's own send completing later in bidir.
    #[tokio::test(start_paused = true)]
    async fn byte_budget_duration_ends_at_last_byte_not_finish() {
        for direction in [Direction::Upload, Direction::Bidir] {
            // Returns at t = 1 s, once the first interval frame blocks.
            let mut reporter = blocked_reporter(Protocol::Tcp, direction, Duration::ZERO).await;
            // Every receive stream reached EOF: the channel closes.
            drop(std::mem::replace(
                &mut reporter.receiving_done,
                mpsc::channel(1).0,
            ));
            tokio::time::sleep(Duration::from_millis(300)).await;
            if direction == Direction::Bidir {
                reporter.sending_done.send(()).await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
            reporter
                .commands
                .send(ControlCommand::Finish)
                .await
                .unwrap();
            let reporting = (&mut reporter.task).await.unwrap().unwrap();
            let expected = match direction {
                Direction::Bidir => 1_300,
                _ => 1_000,
            };
            let duration_ms = reporting.duration_ms();
            assert!(
                (expected..expected + 50).contains(&duration_ms),
                "{direction:?}: {duration_ms} ms"
            );
        }
    }

    #[test]
    fn byte_budget_duration_rounds_up_to_a_whole_millisecond() {
        let mut reporting = Reporting {
            start: Instant::now(),
            paused_total: Duration::ZERO,
            pause_start: None,
            transfer_end: None,
            codec: None,
            current: None,
            pending: None,
            cancelled: None,
            finish_deadline: tokio::time::Instant::now(),
        };
        for (end_us, ms) in [(0, 1), (300, 1), (1_000, 1), (1_200, 2), (2_999, 3)] {
            reporting.transfer_end = Some(Duration::from_micros(end_us));
            assert_eq!(reporting.duration_ms(), ms, "{end_us} us");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn udp_finish_grace_expires_while_writer_is_blocked() {
        let mut reporter = blocked_reporter(Protocol::Udp, Direction::Upload, Duration::ZERO).await;
        reporter
            .commands
            .send(ControlCommand::Finish)
            .await
            .unwrap();
        let before = tokio::time::Instant::now();
        tokio::time::timeout(
            finish_drain_grace(Protocol::Udp) + Duration::from_millis(100),
            reporter.cancelled.changed(),
        )
        .await
        .expect("UDP drain grace must not depend on control-write progress")
        .unwrap();
        assert!(before.elapsed() >= finish_drain_grace(Protocol::Udp));
        assert!(*reporter.cancelled.borrow());
        let reporting = (&mut reporter.task).await.unwrap().unwrap();
        assert!(reporting.cancelled.is_none());
        assert!(reporting.current.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn finite_deadline_excludes_pause_while_writer_is_blocked() {
        let duration = Duration::from_secs(2);
        // Returns once the first tick (t = 1 s) starts a frame that then blocks.
        let mut reporter = blocked_reporter(Protocol::Tcp, Direction::Upload, duration).await;
        reporter.commands.send(ControlCommand::Pause).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), reporter.paused.changed())
            .await
            .expect("Pause must be processed while the writer is stalled")
            .unwrap();
        assert!(*reporter.paused.borrow_and_update());
        // Hold the pause for a non-whole interval, so the active deadline
        // falls between ticks: 1 s active before the pause, 1 s after it.
        assert!(
            tokio::time::timeout(Duration::from_millis(1_500), reporter.cancelled.changed())
                .await
                .is_err(),
            "the original wall-clock deadline must not expire a paused test"
        );
        reporter
            .commands
            .send(ControlCommand::Resume)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), reporter.paused.changed())
            .await
            .expect("Resume must be processed while the writer is stalled")
            .unwrap();
        assert!(!*reporter.paused.borrow());
        // The next tick is 1.5 s away; only the deadline wakes the loop at 1 s.
        tokio::time::timeout(Duration::from_millis(1_100), reporter.cancelled.changed())
            .await
            .expect("resumed finite test must stop at its active deadline, not the next tick")
            .unwrap();
        assert!(*reporter.cancelled.borrow());
        let reporting = (&mut reporter.task).await.unwrap().unwrap();
        assert!(reporting.paused_total >= Duration::from_millis(1_500));
        assert!(reporting.pause_start.is_none());
        let duration_ms = reporting.duration_ms();
        assert!((2_000..2_050).contains(&duration_ms), "{duration_ms}");
        assert!(reporting.cancelled.is_none());
        assert!(reporting.current.is_some());
    }
}
