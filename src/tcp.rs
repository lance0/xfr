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

pub struct TcpConfig {
    pub buffer_size: usize,
    pub nodelay: bool,
    pub window_size: Option<usize>,
}

impl Default for TcpConfig {
    fn default() -> Self {
        Self {
            buffer_size: DEFAULT_BUFFER_SIZE,
            nodelay: false,
            window_size: None,
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
        // 2. No bitrate limit is set (unlimited speed)
        let use_high_speed = window_size
            .map(|w| w > HIGH_SPEED_WINDOW_THRESHOLD)
            .unwrap_or(false)
            || bitrate_limit.is_none();

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

pub fn configure_stream(stream: &TcpStream, config: &TcpConfig) -> std::io::Result<()> {
    stream.set_nodelay(config.nodelay)?;

    if let Some(window) = config.window_size {
        configure_socket_buffers(stream, window)?;
    }

    Ok(())
}

/// Send data as fast as possible for the given duration
pub async fn send_data(
    mut stream: TcpStream,
    stats: Arc<StreamStats>,
    duration: Duration,
    config: TcpConfig,
    cancel: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    configure_stream(&stream, &config)?;

    let buffer = vec![0u8; config.buffer_size];
    let deadline = tokio::time::Instant::now() + duration;

    loop {
        if *cancel.borrow() {
            debug!("Send cancelled for stream {}", stats.stream_id);
            break;
        }

        if tokio::time::Instant::now() >= deadline {
            break;
        }

        match stream.write(&buffer).await {
            Ok(n) => {
                stats.add_bytes_sent(n as u64);
            }
            Err(e) => {
                error!("Send error on stream {}: {}", stats.stream_id, e);
                return Err(e.into());
            }
        }
    }

    // Capture final TCP_INFO for retransmit count
    if let Some(info) = get_stream_tcp_info(&stream) {
        stats.add_retransmits(info.retransmits);
    }

    stream.shutdown().await?;
    debug!(
        "Stream {} send complete: {} bytes",
        stats.stream_id,
        stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed)
    );

    Ok(())
}

/// Receive data and count bytes
pub async fn receive_data(
    mut stream: TcpStream,
    stats: Arc<StreamStats>,
    mut cancel: watch::Receiver<bool>,
    config: TcpConfig,
) -> anyhow::Result<()> {
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

    // Capture final TCP_INFO for retransmit count
    if let Some(info) = get_stream_tcp_info(&stream) {
        stats.add_retransmits(info.retransmits);
    }

    debug!(
        "Stream {} receive complete: {} bytes",
        stats.stream_id,
        stats
            .bytes_received
            .load(std::sync::atomic::Ordering::Relaxed)
    );

    Ok(())
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
    cancel: watch::Receiver<bool>,
) -> anyhow::Result<OwnedWriteHalf> {
    let buffer = vec![0u8; config.buffer_size];
    let deadline = tokio::time::Instant::now() + duration;

    loop {
        if *cancel.borrow() {
            debug!("Send cancelled for stream {}", stats.stream_id);
            break;
        }

        if tokio::time::Instant::now() >= deadline {
            break;
        }

        match write_half.write(&buffer).await {
            Ok(n) => {
                stats.add_bytes_sent(n as u64);
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
}
