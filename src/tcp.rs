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

impl TcpConfig {
    pub fn high_speed() -> Self {
        Self {
            buffer_size: HIGH_SPEED_BUFFER,
            nodelay: true,
            window_size: Some(HIGH_SPEED_BUFFER),
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
    cancel: watch::Receiver<bool>,
    config: TcpConfig,
) -> anyhow::Result<()> {
    configure_stream(&stream, &config)?;

    let mut buffer = vec![0u8; config.buffer_size];

    loop {
        if *cancel.borrow() {
            debug!("Receive cancelled for stream {}", stats.stream_id);
            break;
        }

        match stream.read(&mut buffer).await {
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
pub async fn send_data_half(
    mut write_half: OwnedWriteHalf,
    stats: Arc<StreamStats>,
    duration: Duration,
    config: TcpConfig,
    cancel: watch::Receiver<bool>,
) -> anyhow::Result<()> {
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

    write_half.shutdown().await?;
    debug!(
        "Stream {} send complete: {} bytes",
        stats.stream_id,
        stats.bytes_sent.load(std::sync::atomic::Ordering::Relaxed)
    );

    Ok(())
}

/// Receive data on a split socket read half (for bidir mode)
pub async fn receive_data_half(
    mut read_half: OwnedReadHalf,
    stats: Arc<StreamStats>,
    cancel: watch::Receiver<bool>,
    config: TcpConfig,
) -> anyhow::Result<()> {
    let mut buffer = vec![0u8; config.buffer_size];

    loop {
        if *cancel.borrow() {
            debug!("Receive cancelled for stream {}", stats.stream_id);
            break;
        }

        match read_half.read(&mut buffer).await {
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

    debug!(
        "Stream {} receive complete: {} bytes",
        stats.stream_id,
        stats
            .bytes_received
            .load(std::sync::atomic::Ordering::Relaxed)
    );

    Ok(())
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
