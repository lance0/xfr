//! TCP data transfer implementation
//!
//! Handles bulk TCP data transfer for bandwidth testing, with support for
//! configurable buffer sizes and TCP tuning options.

use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::watch;
use tracing::{debug, error, warn};

use crate::stats::StreamStats;
use crate::tcp_info::get_tcp_info;

const DEFAULT_BUFFER_SIZE: usize = 128 * 1024; // 128 KB
const HIGH_SPEED_BUFFER: usize = 4 * 1024 * 1024; // 4 MB for 10G+

#[derive(Clone)]
pub struct TcpConfig {
    pub buffer_size: usize,
    pub nodelay: bool,
    pub window_size: Option<usize>,
    pub congestion: Option<String>,
}

impl Default for TcpConfig {
    fn default() -> Self {
        Self {
            buffer_size: DEFAULT_BUFFER_SIZE,
            nodelay: false,
            window_size: None,
            congestion: None,
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

    // Cap buffer size for rate-limited sends to prevent large first-write burst
    let buf_size = match bitrate {
        Some(bps) if bps > 0 => {
            let bytes_per_sec = bps / 8;
            // Target ~10 writes/sec minimum, capped at config buffer size
            config.buffer_size.min((bytes_per_sec / 10).max(1) as usize)
        }
        _ => config.buffer_size,
    };
    let buffer = vec![0u8; buf_size];
    let start = tokio::time::Instant::now();
    let deadline = start + duration;
    let is_infinite = duration == Duration::ZERO;
    let mut pace_start = start;
    let mut pace_bytes_offset: u64 = 0;

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
                if let Some(bps) = bitrate
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
                error!("Send error on stream {}: {}", stats.stream_id, e);
                return Err(e.into());
            }
        }
    }

    // Capture final TCP_INFO for retransmit count and RTT
    let tcp_info = get_stream_tcp_info(&stream);
    if let Some(ref info) = tcp_info {
        stats.add_retransmits(info.retransmits);
    }

    stream.shutdown().await?;
    debug!(
        "Stream {} send complete: {} bytes",
        stats.stream_id,
        stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed)
    );

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
                        warn!("Receive error on stream {}: {}", stats.stream_id, e);
                        return Err(e.into());
                    }
                }
            }
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    debug!("Receive cancelled for stream {}", stats.stream_id);
                    break;
                }
            }
        }
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

    Ok(tcp_info)
}

/// Get TCP stats from the socket
pub fn get_stream_tcp_info(stream: &TcpStream) -> Option<crate::protocol::TcpInfoSnapshot> {
    get_tcp_info(stream).ok()
}

/// Send data on a split socket write half (for bidir mode)
/// Returns the write half for reuniting to get TCP_INFO
pub async fn send_data_half(
    mut write_half: OwnedWriteHalf,
    stats: Arc<StreamStats>,
    duration: Duration,
    config: TcpConfig,
    mut cancel: watch::Receiver<bool>,
    bitrate: Option<u64>,
    mut pause: watch::Receiver<bool>,
) -> anyhow::Result<OwnedWriteHalf> {
    // Cap buffer size for rate-limited sends to prevent large first-write burst
    let buf_size = match bitrate {
        Some(bps) if bps > 0 => {
            let bytes_per_sec = bps / 8;
            config.buffer_size.min((bytes_per_sec / 10).max(1) as usize)
        }
        _ => config.buffer_size,
    };
    let buffer = vec![0u8; buf_size];
    let start = tokio::time::Instant::now();
    let deadline = start + duration;
    let is_infinite = duration == Duration::ZERO;
    let mut pace_start = start;
    let mut pace_bytes_offset: u64 = 0;

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
                if let Some(bps) = bitrate
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
                error!("Send error on stream {}: {}", stats.stream_id, e);
                return Err(e.into());
            }
        }
    }

    let _ = write_half.shutdown().await;
    debug!(
        "Stream {} send complete: {} bytes",
        stats.stream_id,
        stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed)
    );

    Ok(write_half)
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
                        warn!("Receive error on stream {}: {}", stats.stream_id, e);
                        return Err(e.into());
                    }
                }
            }
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    debug!("Receive cancelled for stream {}", stats.stream_id);
                    break;
                }
            }
        }
    }

    debug!(
        "Stream {} receive complete: {} bytes",
        stats.stream_id,
        stats
            .bytes_received
            .load(std::sync::atomic::Ordering::Relaxed)
    );

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
    #[cfg(target_os = "linux")]
    fn test_validate_congestion_cubic() {
        // cubic is available on all Linux kernels
        assert!(validate_congestion("cubic").is_ok());
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
