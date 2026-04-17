//! TCP data transfer implementation
//!
//! Handles bulk TCP data transfer for bandwidth testing, with support for
//! configurable buffer sizes and TCP tuning options.

use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::watch;
use tracing::{debug, error, warn};

use crate::stats::StreamStats;
use crate::tcp_info::get_tcp_info;

const DEFAULT_BUFFER_SIZE: usize = 128 * 1024; // 128 KB
const HIGH_SPEED_BUFFER: usize = 4 * 1024 * 1024; // 4 MB for 10G+
const SEND_TEARDOWN_GRACE: Duration = Duration::from_millis(250);
// Post-cancel read window: covers the race where our receive side signals
// cancel and drops the socket before the peer's own cancel_tx has fired
// (i.e., the Cancel control message hasn't yet been processed by the peer's
// data loop). 200ms handles moderate WAN RTTs; same-host is sub-ms. Drained
// bytes aren't counted in stats, so no accuracy impact on throughput.
const RECEIVE_CANCEL_DRAIN_GRACE: Duration = Duration::from_millis(200);

#[inline]
fn is_peer_closed_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
    )
}

/// Drain readable bytes briefly after cancel to give the peer's own cancel
/// signal time to propagate before we drop the socket (which would RST).
/// Drained bytes are not added to stats — this is purely for teardown hygiene.
async fn drain_after_cancel<R: AsyncRead + Unpin>(reader: &mut R, stream_id: u8) {
    let mut buffer = [0u8; 16 * 1024];
    let deadline = tokio::time::Instant::now() + RECEIVE_CANCEL_DRAIN_GRACE;
    let mut drained = 0u64;

    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, reader.read(&mut buffer)).await {
            Ok(Ok(0)) => break, // EOF
            Ok(Ok(n)) => drained += n as u64,
            Ok(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Ok(Err(e)) if is_peer_closed_error(&e) => break,
            Ok(Err(_)) => break,
            Err(_) => break, // grace window elapsed
        }
    }

    if drained > 0 {
        debug!(
            "Stream {} drained {} bytes after cancel before close",
            stream_id, drained
        );
    }
}

/// Clamp `stats.bytes_sent` to `bytes_acked` before an abortive close. On Linux
/// with SO_LINGER=0, unACK'd send-buffer data is discarded by RST; reporting it
/// as "sent" would overcount. Called at every stream-return site in send paths.
fn clamp_bytes_sent_to_acked(stats: &StreamStats, info: &crate::protocol::TcpInfoSnapshot) {
    if let Some(acked) = info.bytes_acked {
        let sent = stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed);
        if acked < sent {
            stats
                .bytes_sent
                .store(acked, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

#[derive(Clone)]
pub struct TcpConfig {
    pub buffer_size: usize,
    pub nodelay: bool,
    pub window_size: Option<usize>,
    pub congestion: Option<String>,
    pub random_payload: bool,
}

impl Default for TcpConfig {
    fn default() -> Self {
        Self {
            buffer_size: DEFAULT_BUFFER_SIZE,
            nodelay: false,
            window_size: None,
            congestion: None,
            random_payload: false,
        }
    }
}

/// Threshold for auto-selecting high-speed configuration (1 MB)
const HIGH_SPEED_WINDOW_THRESHOLD: usize = 1_000_000;

impl TcpConfig {
    pub fn high_speed() -> Self {
        Self {
            buffer_size: HIGH_SPEED_BUFFER,
            nodelay: true,
            window_size: Some(HIGH_SPEED_BUFFER),
            congestion: None,
            random_payload: false,
        }
    }

    /// Create a config with auto-detection for high-speed mode.
    /// Selects high_speed() when window_size > 1MB or when there's no bitrate limit.
    pub fn with_auto_detect(
        nodelay: bool,
        window_size: Option<usize>,
        bitrate_limit: Option<u64>,
    ) -> Self {
        // Auto-select high-speed mode when:
        // 1. Window size is explicitly set to > 1MB, or
        // 2. No bitrate limit is set (unlimited speed, including explicit -b 0)
        let use_high_speed = window_size
            .map(|w| w > HIGH_SPEED_WINDOW_THRESHOLD)
            .unwrap_or(false)
            || !matches!(bitrate_limit, Some(bps) if bps > 0);

        if use_high_speed && window_size.is_none() {
            // Use full high-speed config with large buffers
            let mut config = Self::high_speed();
            config.nodelay = nodelay;
            config
        } else {
            // Use user-specified or default settings
            Self {
                buffer_size: window_size.unwrap_or(DEFAULT_BUFFER_SIZE),
                nodelay,
                window_size,
                congestion: None,
                random_payload: false,
            }
        }
    }
}

#[cfg(unix)]
fn configure_socket_buffers(stream: &TcpStream, buffer_size: usize) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    use tracing::debug;

    let fd = stream.as_raw_fd();
    let size = buffer_size as libc::c_int;

    // SAFETY: fd is a valid file descriptor from stream.as_raw_fd(),
    // size is a valid c_int pointer, and size_of::<c_int>() is correct.
    // setsockopt may fail but we check and log the return value.
    unsafe {
        let ret = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &size as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        if ret != 0 {
            debug!(
                "Failed to set SO_SNDBUF to {}: {}",
                buffer_size,
                std::io::Error::last_os_error()
            );
        }

        let ret = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &size as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        if ret != 0 {
            debug!(
                "Failed to set SO_RCVBUF to {}: {}",
                buffer_size,
                std::io::Error::last_os_error()
            );
        }
    }

    Ok(())
}

#[cfg(not(unix))]
fn configure_socket_buffers(_stream: &TcpStream, _buffer_size: usize) -> std::io::Result<()> {
    Ok(())
}

/// Set TCP congestion control algorithm (e.g. "cubic", "bbr", "reno")
#[cfg(target_os = "linux")]
fn set_tcp_congestion(stream: &TcpStream, algo: &str) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    let fd = stream.as_raw_fd();
    // SAFETY: fd is a valid file descriptor from stream.as_raw_fd(),
    // algo.as_ptr() points to valid bytes, and algo.len() is the correct length.
    // setsockopt returns -1 on error which we check below.
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_CONGESTION,
            algo.as_ptr() as *const libc::c_void,
            algo.len() as libc::socklen_t,
        )
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn set_tcp_congestion(_stream: &TcpStream, _algo: &str) -> std::io::Result<()> {
    Ok(())
}

/// Set kernel TCP pacing rate via SO_MAX_PACING_RATE.
/// The kernel's FQ scheduler uses EDT for precise per-packet timing,
/// eliminating burst behavior inherent in userspace sleep/wake cycles.
/// Returns true if kernel pacing was successfully set.
#[cfg(target_os = "linux")]
#[inline]
fn pacing_rate_bytes_per_sec(bitrate_bps: u64) -> libc::c_ulong {
    if bitrate_bps == 0 {
        return 0;
    }
    // Use ceil division so very low bitrates (e.g. 1 bps) map to 1 B/s
    // instead of 0, which would disable pacing.
    let bytes_per_sec = bitrate_bps.saturating_add(7) / 8;
    libc::c_ulong::try_from(bytes_per_sec).unwrap_or(libc::c_ulong::MAX)
}

#[cfg(target_os = "linux")]
fn try_set_pacing_rate(stream: &TcpStream, bitrate_bps: u64) -> bool {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let rate = pacing_rate_bytes_per_sec(bitrate_bps);
    // SAFETY: fd is a valid file descriptor from stream.as_raw_fd(),
    // rate is a valid c_ulong pointer, and size_of::<c_ulong>() is correct.
    // setsockopt returns -1 on error which we check below.
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_MAX_PACING_RATE,
            &rate as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_ulong>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        warn!(
            "SO_MAX_PACING_RATE failed, using userspace pacing: {}",
            std::io::Error::last_os_error()
        );
        return false;
    }
    true
}

#[cfg(not(target_os = "linux"))]
fn try_set_pacing_rate(_stream: &TcpStream, _bitrate_bps: u64) -> bool {
    false
}

/// Validate that a congestion control algorithm is available on this kernel.
/// Creates a temporary socket to test the setsockopt call.
#[cfg(target_os = "linux")]
pub fn validate_congestion(algo: &str) -> Result<(), String> {
    // SAFETY: socket() returns a valid fd or -1 on error (checked below).
    // setsockopt uses the fd with valid pointer/length from algo slice.
    // close() is called unconditionally to avoid fd leak.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(format!(
            "failed to create test socket: {}",
            std::io::Error::last_os_error()
        ));
    }
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_CONGESTION,
            algo.as_ptr() as *const libc::c_void,
            algo.len() as libc::socklen_t,
        )
    };
    let result = if ret != 0 {
        let mut msg = "not available on this kernel".to_string();
        // Try to list available algorithms for a helpful error
        if let Ok(available) =
            std::fs::read_to_string("/proc/sys/net/ipv4/tcp_available_congestion_control")
        {
            msg = format!("not available (available: {})", available.trim());
        }
        Err(msg)
    } else {
        Ok(())
    };
    unsafe { libc::close(fd) };
    result
}

#[cfg(not(target_os = "linux"))]
pub fn validate_congestion(_algo: &str) -> Result<(), String> {
    Ok(())
}

pub fn configure_stream(stream: &TcpStream, config: &TcpConfig) -> std::io::Result<()> {
    stream.set_nodelay(config.nodelay)?;

    if let Some(window) = config.window_size {
        configure_socket_buffers(stream, window)?;
    }

    if let Some(ref algo) = config.congestion {
        set_tcp_congestion(stream, algo)?;
    }

    Ok(())
}

/// Send data as fast as possible for the given duration
/// Returns the final TCP_INFO snapshot (for RTT, retransmits, cwnd)
pub async fn send_data(
    mut stream: TcpStream,
    stats: Arc<StreamStats>,
    duration: Duration,
    config: TcpConfig,
    mut cancel: watch::Receiver<bool>,
    bitrate: Option<u64>,
    mut pause: watch::Receiver<bool>,
) -> anyhow::Result<Option<crate::protocol::TcpInfoSnapshot>> {
    configure_stream(&stream, &config)?;
    // Force abortive close on Linux only, where `tcpi_bytes_acked` lets us clamp
    // the overcount of discarded send-buffer bytes. On platforms without that
    // counter (macOS, fallback), graceful shutdown is used at end of loop to
    // preserve accuracy. See issue #54.
    #[cfg(target_os = "linux")]
    let _ = socket2::SockRef::from(&stream).set_linger(Some(Duration::ZERO));

    let kernel_pacing = match bitrate {
        Some(bps) if bps > 0 => {
            let set = try_set_pacing_rate(&stream, bps);
            if set {
                debug!(
                    "Stream {}: kernel TCP pacing set to {} bps",
                    stats.stream_id, bps
                );
            }
            set
        }
        _ => false,
    };

    // Cap buffer size for rate-limited sends to prevent large first-write burst
    let buf_size = match bitrate {
        Some(bps) if bps > 0 => {
            let bytes_per_sec = bps / 8;
            // Target ~10 writes/sec minimum, capped at config buffer size
            config.buffer_size.min((bytes_per_sec / 10).max(1) as usize)
        }
        _ => config.buffer_size,
    };
    let mut buffer = vec![0u8; buf_size];
    if config.random_payload {
        rand::Rng::fill(&mut rand::rng(), buffer.as_mut_slice());
    }
    let start = tokio::time::Instant::now();
    let deadline = start + duration;
    let is_infinite = duration == Duration::ZERO;
    let mut pace_start = start;
    let mut pace_bytes_offset: u64 = 0;
    let mut suppressed_teardown_errors: u32 = 0;

    loop {
        if *cancel.borrow() {
            debug!("Send cancelled for stream {}", stats.stream_id);
            break;
        }

        if *pause.borrow() {
            if crate::pause::wait_while_paused(&mut pause, &mut cancel).await {
                break;
            }
            // Reset pacing baseline after resume to prevent catch-up burst
            pace_start = tokio::time::Instant::now();
            pace_bytes_offset = stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed);
            continue;
        }

        // Duration::ZERO means infinite - only check deadline if finite
        if !is_infinite && tokio::time::Instant::now() >= deadline {
            break;
        }

        match stream.write(&buffer).await {
            Ok(n) => {
                stats.add_bytes_sent(n as u64);
                // Pace sends to target bitrate using byte-budget approach
                // Skip userspace pacing when kernel pacing is active
                if !kernel_pacing
                    && let Some(bps) = bitrate
                    && bps > 0
                {
                    let bytes_per_sec = bps as f64 / 8.0;
                    let elapsed = pace_start.elapsed().as_secs_f64();
                    let expected = elapsed * bytes_per_sec;
                    let total = (stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed)
                        - pace_bytes_offset) as f64;
                    if total > expected {
                        let overshoot = Duration::from_secs_f64((total - expected) / bytes_per_sec);
                        // Interruptible sleep: wake on cancel/pause to avoid blocking
                        tokio::select! {
                            biased;
                            _ = cancel.changed() => {
                                debug!("Send cancelled during pacing sleep for stream {}", stats.stream_id);
                                break;
                            }
                            _ = pause.changed() => {} // will check pause at top of next loop
                            _ = tokio::time::sleep(overshoot) => {}
                        }
                    }
                }
            }
            Err(e) => {
                let now = tokio::time::Instant::now();
                let deadline_reached = !is_infinite && now >= deadline;
                let near_deadline = !is_infinite && now + SEND_TEARDOWN_GRACE >= deadline;
                let peer_closed = is_peer_closed_error(&e);
                // Treat connection reset/pipe errors as clean teardown only when we are
                // already cancelling or very close to the configured end time.
                if *cancel.borrow() || deadline_reached || (peer_closed && near_deadline) {
                    if peer_closed {
                        suppressed_teardown_errors += 1;
                    }
                    break;
                }
                if peer_closed {
                    warn!(
                        "Unexpected peer-close on stream {} after {:.3}s (cancel={}, deadline_reached={}, near_deadline={}): {}",
                        stats.stream_id,
                        now.saturating_duration_since(start).as_secs_f64(),
                        *cancel.borrow(),
                        deadline_reached,
                        near_deadline,
                        e
                    );
                }
                error!("Send error on stream {}: {}", stats.stream_id, e);
                // Clamp bytes_sent before the RST-on-drop discards unACK'd data,
                // preserving the accuracy invariant even on the error path.
                if let Some(info) = get_stream_tcp_info(&stream) {
                    clamp_bytes_sent_to_acked(&stats, &info);
                }
                return Err(e.into());
            }
        }
    }

    // Capture final TCP_INFO for retransmit count and RTT
    let tcp_info = get_stream_tcp_info(&stream);
    if let Some(ref info) = tcp_info {
        stats.add_retransmits(info.retransmits);
        clamp_bytes_sent_to_acked(&stats, info);
    }

    // On Linux, SO_LINGER=0 was set earlier; drop alone triggers RST (skipping drain).
    // On other platforms, call shutdown() for graceful FIN — slower but preserves
    // accounting accuracy since we don't have bytes_acked to clamp overcount.
    #[cfg(not(target_os = "linux"))]
    if let Err(e) = stream.shutdown().await {
        if *cancel.borrow() || is_peer_closed_error(&e) {
            debug!(
                "Stream {} shutdown ended during teardown: {}",
                stats.stream_id, e
            );
        } else {
            warn!(
                "Stream {} shutdown error after send loop: {}",
                stats.stream_id, e
            );
        }
    }

    debug!(
        "Stream {} send complete: {} bytes",
        stats.stream_id,
        stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed)
    );
    if suppressed_teardown_errors > 0 {
        debug!(
            "Stream {} suppressed {} expected teardown send errors",
            stats.stream_id, suppressed_teardown_errors
        );
    }

    Ok(tcp_info)
}

/// Receive data and count bytes
/// Returns the final TCP_INFO snapshot (for RTT, cwnd - note: retransmits only meaningful for sender)
pub async fn receive_data(
    mut stream: TcpStream,
    stats: Arc<StreamStats>,
    mut cancel: watch::Receiver<bool>,
    config: TcpConfig,
) -> anyhow::Result<Option<crate::protocol::TcpInfoSnapshot>> {
    configure_stream(&stream, &config)?;

    let mut buffer = vec![0u8; config.buffer_size];
    let mut suppressed_teardown_errors: u32 = 0;
    let mut cancelled = false;

    loop {
        tokio::select! {
            result = stream.read(&mut buffer) => {
                match result {
                    Ok(0) => {
                        debug!("Stream {} EOF", stats.stream_id);
                        break;
                    }
                    Ok(n) => {
                        stats.add_bytes_received(n as u64);
                    }
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::WouldBlock {
                            continue;
                        }
                        // Receive-side reset/pipe before cancel is unexpected and should
                        // still be surfaced as an error.
                        if *cancel.borrow() {
                            cancelled = true;
                            if is_peer_closed_error(&e) {
                                suppressed_teardown_errors += 1;
                            }
                            break;
                        }
                        warn!("Receive error on stream {}: {}", stats.stream_id, e);
                        return Err(e.into());
                    }
                }
            }
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    cancelled = true;
                    debug!("Receive cancelled for stream {}", stats.stream_id);
                    break;
                }
            }
        }
    }

    if cancelled {
        drain_after_cancel(&mut stream, stats.stream_id).await;
    }

    // Capture final TCP_INFO
    let tcp_info = get_stream_tcp_info(&stream);
    if let Some(ref info) = tcp_info {
        stats.add_retransmits(info.retransmits);
    }

    debug!(
        "Stream {} receive complete: {} bytes",
        stats.stream_id,
        stats
            .bytes_received
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    if suppressed_teardown_errors > 0 {
        debug!(
            "Stream {} suppressed {} expected teardown receive errors",
            stats.stream_id, suppressed_teardown_errors
        );
    }

    Ok(tcp_info)
}

/// Get TCP stats from the socket
pub fn get_stream_tcp_info(stream: &TcpStream) -> Option<crate::protocol::TcpInfoSnapshot> {
    get_tcp_info(stream).ok()
}

/// Send data on a split socket write half (for bidir mode).
/// Returns the write half for reuniting, plus the sender-side TCP_INFO snapshot
/// captured at clamp time — caller should prefer this over re-reading TCP_INFO
/// post-reunite, since a second read would see a later (larger) `bytes_acked`
/// that could exceed the clamped `bytes_sent`.
pub async fn send_data_half(
    mut write_half: OwnedWriteHalf,
    stats: Arc<StreamStats>,
    duration: Duration,
    config: TcpConfig,
    mut cancel: watch::Receiver<bool>,
    bitrate: Option<u64>,
    mut pause: watch::Receiver<bool>,
) -> anyhow::Result<(OwnedWriteHalf, Option<crate::protocol::TcpInfoSnapshot>)> {
    // Linux only: force abortive close. See send_data() for rationale.
    #[cfg(target_os = "linux")]
    let _ = socket2::SockRef::from(write_half.as_ref()).set_linger(Some(Duration::ZERO));

    let kernel_pacing = match bitrate {
        Some(bps) if bps > 0 => {
            let set = try_set_pacing_rate(write_half.as_ref(), bps);
            if set {
                debug!(
                    "Stream {}: kernel TCP pacing set to {} bps",
                    stats.stream_id, bps
                );
            }
            set
        }
        _ => false,
    };

    // Cap buffer size for rate-limited sends to prevent large first-write burst
    let buf_size = match bitrate {
        Some(bps) if bps > 0 => {
            let bytes_per_sec = bps / 8;
            config.buffer_size.min((bytes_per_sec / 10).max(1) as usize)
        }
        _ => config.buffer_size,
    };
    let mut buffer = vec![0u8; buf_size];
    if config.random_payload {
        rand::Rng::fill(&mut rand::rng(), buffer.as_mut_slice());
    }
    let start = tokio::time::Instant::now();
    let deadline = start + duration;
    let is_infinite = duration == Duration::ZERO;
    let mut pace_start = start;
    let mut pace_bytes_offset: u64 = 0;
    let mut suppressed_teardown_errors: u32 = 0;

    loop {
        if *cancel.borrow() {
            debug!("Send cancelled for stream {}", stats.stream_id);
            break;
        }

        if *pause.borrow() {
            if crate::pause::wait_while_paused(&mut pause, &mut cancel).await {
                break;
            }
            pace_start = tokio::time::Instant::now();
            pace_bytes_offset = stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed);
            continue;
        }

        // Duration::ZERO means infinite - only check deadline if finite
        if !is_infinite && tokio::time::Instant::now() >= deadline {
            break;
        }

        match write_half.write(&buffer).await {
            Ok(n) => {
                stats.add_bytes_sent(n as u64);
                // Pace sends to target bitrate using byte-budget approach
                // Skip userspace pacing when kernel pacing is active
                if !kernel_pacing
                    && let Some(bps) = bitrate
                    && bps > 0
                {
                    let bytes_per_sec = bps as f64 / 8.0;
                    let elapsed = pace_start.elapsed().as_secs_f64();
                    let expected = elapsed * bytes_per_sec;
                    let total = (stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed)
                        - pace_bytes_offset) as f64;
                    if total > expected {
                        let overshoot = Duration::from_secs_f64((total - expected) / bytes_per_sec);
                        tokio::select! {
                            biased;
                            _ = cancel.changed() => {
                                debug!("Send cancelled during pacing sleep for stream {}", stats.stream_id);
                                break;
                            }
                            _ = pause.changed() => {}
                            _ = tokio::time::sleep(overshoot) => {}
                        }
                    }
                }
            }
            Err(e) => {
                let now = tokio::time::Instant::now();
                let deadline_reached = !is_infinite && now >= deadline;
                let near_deadline = !is_infinite && now + SEND_TEARDOWN_GRACE >= deadline;
                let peer_closed = is_peer_closed_error(&e);
                if *cancel.borrow() || deadline_reached || (peer_closed && near_deadline) {
                    if peer_closed {
                        suppressed_teardown_errors += 1;
                    }
                    break;
                }
                if peer_closed {
                    warn!(
                        "Unexpected peer-close on stream {} after {:.3}s (cancel={}, deadline_reached={}, near_deadline={}): {}",
                        stats.stream_id,
                        now.saturating_duration_since(start).as_secs_f64(),
                        *cancel.borrow(),
                        deadline_reached,
                        near_deadline,
                        e
                    );
                }
                error!("Send error on stream {}: {}", stats.stream_id, e);
                // Clamp before RST-on-drop discards unACK'd data.
                if let Ok(info) = get_tcp_info(write_half.as_ref()) {
                    clamp_bytes_sent_to_acked(&stats, &info);
                }
                return Err(e.into());
            }
        }
    }

    // Clamp bytes_sent to bytes_acked before abortive close (see send_data for rationale).
    // Capture the snapshot so the caller can store it directly — a second post-reunite
    // read would see a later, larger bytes_acked that could exceed the clamped bytes_sent.
    let tcp_info = get_tcp_info(write_half.as_ref()).ok();
    if let Some(ref info) = tcp_info {
        clamp_bytes_sent_to_acked(&stats, info);
    }

    // On Linux, SO_LINGER=0 was set earlier; drop alone triggers RST.
    // On other platforms, do graceful shutdown to preserve accounting accuracy.
    #[cfg(not(target_os = "linux"))]
    let _ = write_half.shutdown().await;

    debug!(
        "Stream {} send complete: {} bytes",
        stats.stream_id,
        stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed)
    );
    if suppressed_teardown_errors > 0 {
        debug!(
            "Stream {} suppressed {} expected teardown send-half errors",
            stats.stream_id, suppressed_teardown_errors
        );
    }

    Ok((write_half, tcp_info))
}

/// Receive data on a split socket read half (for bidir mode)
/// Returns the read half for reuniting to get TCP_INFO
pub async fn receive_data_half(
    mut read_half: OwnedReadHalf,
    stats: Arc<StreamStats>,
    mut cancel: watch::Receiver<bool>,
    config: TcpConfig,
) -> anyhow::Result<OwnedReadHalf> {
    let mut buffer = vec![0u8; config.buffer_size];
    let mut suppressed_teardown_errors: u32 = 0;
    let mut cancelled = false;

    loop {
        tokio::select! {
            result = read_half.read(&mut buffer) => {
                match result {
                    Ok(0) => {
                        debug!("Stream {} EOF", stats.stream_id);
                        break;
                    }
                    Ok(n) => {
                        stats.add_bytes_received(n as u64);
                    }
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::WouldBlock {
                            continue;
                        }
                        if *cancel.borrow() {
                            cancelled = true;
                            if is_peer_closed_error(&e) {
                                suppressed_teardown_errors += 1;
                            }
                            break;
                        }
                        warn!("Receive error on stream {}: {}", stats.stream_id, e);
                        return Err(e.into());
                    }
                }
            }
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    cancelled = true;
                    debug!("Receive cancelled for stream {}", stats.stream_id);
                    break;
                }
            }
        }
    }

    if cancelled {
        drain_after_cancel(&mut read_half, stats.stream_id).await;
    }

    debug!(
        "Stream {} receive complete: {} bytes",
        stats.stream_id,
        stats
            .bytes_received
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    if suppressed_teardown_errors > 0 {
        debug!(
            "Stream {} suppressed {} expected teardown receive-half errors",
            stats.stream_id, suppressed_teardown_errors
        );
    }

    Ok(read_half)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = TcpConfig::default();
        assert_eq!(config.buffer_size, DEFAULT_BUFFER_SIZE);
        assert!(!config.nodelay);
    }

    #[test]
    fn test_high_speed_config() {
        let config = TcpConfig::high_speed();
        assert_eq!(config.buffer_size, HIGH_SPEED_BUFFER);
        assert!(config.nodelay);
    }

    #[test]
    fn test_clamp_bytes_sent_to_acked_reduces_overcount() {
        // Sender had written 1000 bytes but peer only ACK'd 800.
        // Clamp should bring bytes_sent down to 800 (what actually landed).
        let stats = StreamStats::new(0);
        stats.add_bytes_sent(1000);
        let info = crate::protocol::TcpInfoSnapshot {
            retransmits: 0,
            rtt_us: 1000,
            rtt_var_us: 100,
            cwnd: 10,
            bytes_acked: Some(800),
        };
        clamp_bytes_sent_to_acked(&stats, &info);
        assert_eq!(
            stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed),
            800
        );
    }

    #[test]
    fn test_clamp_bytes_sent_to_acked_no_change_when_acked_exceeds_sent() {
        // bytes_acked > bytes_sent shouldn't happen (would be a kernel bug), but
        // if it does, we must not inflate the counter beyond what was actually sent.
        let stats = StreamStats::new(0);
        stats.add_bytes_sent(500);
        let info = crate::protocol::TcpInfoSnapshot {
            retransmits: 0,
            rtt_us: 1000,
            rtt_var_us: 100,
            cwnd: 10,
            bytes_acked: Some(999),
        };
        clamp_bytes_sent_to_acked(&stats, &info);
        assert_eq!(
            stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed),
            500
        );
    }

    #[test]
    fn test_clamp_bytes_sent_to_acked_none_is_noop() {
        // On platforms without bytes_acked support (macOS, old Linux kernels),
        // the clamp must be a no-op — we can't clamp to an unknown value.
        let stats = StreamStats::new(0);
        stats.add_bytes_sent(1000);
        let info = crate::protocol::TcpInfoSnapshot {
            retransmits: 0,
            rtt_us: 1000,
            rtt_var_us: 100,
            cwnd: 10,
            bytes_acked: None,
        };
        clamp_bytes_sent_to_acked(&stats, &info);
        assert_eq!(
            stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed),
            1000
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_validate_congestion_cubic() {
        // cubic is available on all Linux kernels
        assert!(validate_congestion("cubic").is_ok());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_pacing_rate_bytes_per_sec_conversion() {
        assert_eq!(pacing_rate_bytes_per_sec(0), 0);
        assert_eq!(pacing_rate_bytes_per_sec(1), 1);
        assert_eq!(pacing_rate_bytes_per_sec(7), 1);
        assert_eq!(pacing_rate_bytes_per_sec(8), 1);
        assert_eq!(pacing_rate_bytes_per_sec(9), 2);
        let expected_max_input =
            libc::c_ulong::try_from(u64::MAX.saturating_add(7) / 8).unwrap_or(libc::c_ulong::MAX);
        assert_eq!(pacing_rate_bytes_per_sec(u64::MAX), expected_max_input);

        // On 32-bit where c_ulong < u64, verify explicit clamp behavior.
        if libc::c_ulong::BITS < 64 {
            // On 64-bit, c_ulong::MAX == u64::MAX so try_from is trivially ok;
            // on 32-bit the cast widens. Clippy flags the 64-bit case; allow it.
            #[allow(clippy::useless_conversion)]
            let max = u64::try_from(libc::c_ulong::MAX).unwrap_or(u64::MAX);
            let overflow_bytes = max.saturating_add(1);
            let bitrate = overflow_bytes.saturating_mul(8);
            assert_eq!(pacing_rate_bytes_per_sec(bitrate), libc::c_ulong::MAX);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_set_pacing_rate() {
        use std::os::unix::io::AsRawFd;
        // Use a raw socket to avoid needing a tokio runtime
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stream = std::net::TcpStream::connect(addr).unwrap();
        let fd = stream.as_raw_fd();
        let rate: libc::c_ulong =
            libc::c_ulong::try_from(100_000_000u64 / 8).unwrap_or(libc::c_ulong::MAX); // 100 Mbps in bytes/sec
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_MAX_PACING_RATE,
                &rate as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_ulong>() as libc::socklen_t,
            )
        };
        assert_eq!(
            ret,
            0,
            "SO_MAX_PACING_RATE failed: {}",
            std::io::Error::last_os_error()
        );

        let mut actual: libc::c_ulong = 0;
        let mut len = std::mem::size_of::<libc::c_ulong>() as libc::socklen_t;
        let ret = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_MAX_PACING_RATE,
                &mut actual as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        assert_eq!(
            ret,
            0,
            "SO_MAX_PACING_RATE getsockopt failed: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(len as usize, std::mem::size_of::<libc::c_ulong>());
        assert!(actual > 0, "SO_MAX_PACING_RATE should be non-zero");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_validate_congestion_invalid() {
        let result = validate_congestion("nonexistent_algo_xyz");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("not available"), "unexpected error: {}", msg);
    }
}
