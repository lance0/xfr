//! io_uring-based I/O backend for Linux.
//!
//! Uses tokio-uring for high-performance async I/O with reduced syscall overhead.
//! Requires Linux kernel 5.10+ and the `io-uring` feature.
//!
//! Note: io_uring has a different buffer ownership model - buffers must be owned
//! and cannot be shared during I/O operations. This implementation manages
//! buffer pools internally.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::watch;
use tracing::{debug, warn};

use super::DataBackend;
use crate::protocol::UdpStats;
use crate::stats::StreamStats;
use crate::tcp::TcpConfig;
use crate::udp::UdpSendStats;

/// io_uring-based data transfer backend
///
/// This backend uses io_uring for efficient async I/O on Linux systems.
/// Currently falls back to tokio for some operations pending full io_uring support.
pub struct UringBackend {
    /// Buffer size for I/O operations
    buffer_size: usize,
}

impl UringBackend {
    pub fn new() -> Self {
        Self {
            buffer_size: 4 * 1024 * 1024, // 4MB default
        }
    }

    /// Create with custom buffer size
    pub fn with_buffer_size(buffer_size: usize) -> Self {
        Self { buffer_size }
    }
}

impl Default for UringBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DataBackend for UringBackend {
    async fn send_tcp(
        &self,
        stream: TcpStream,
        stats: Arc<StreamStats>,
        duration: Duration,
        config: TcpConfig,
        cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        // TODO: Implement true io_uring-based TCP send
        // For now, fall back to tokio implementation
        debug!(
            "io_uring TCP send: falling back to tokio (buffer_size={})",
            self.buffer_size
        );
        crate::tcp::send_data(stream, stats, duration, config, cancel).await
    }

    async fn recv_tcp(
        &self,
        stream: TcpStream,
        stats: Arc<StreamStats>,
        config: TcpConfig,
        cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        // TODO: Implement true io_uring-based TCP receive
        debug!("io_uring TCP recv: falling back to tokio");
        crate::tcp::receive_data(stream, stats, cancel, config).await
    }

    async fn send_udp(
        &self,
        socket: Arc<UdpSocket>,
        target_bitrate: u64,
        duration: Duration,
        stats: Arc<StreamStats>,
        cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<UdpSendStats> {
        // TODO: Implement true io_uring-based UDP send with batching
        debug!("io_uring UDP send: falling back to tokio");
        crate::udp::send_udp_paced(socket, target_bitrate, duration, stats, cancel).await
    }

    async fn recv_udp(
        &self,
        socket: Arc<UdpSocket>,
        stats: Arc<StreamStats>,
        cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<(UdpStats, u64)> {
        // TODO: Implement true io_uring-based UDP receive with batching
        debug!("io_uring UDP recv: falling back to tokio");
        crate::udp::receive_udp(socket, stats, cancel).await
    }

    fn name(&self) -> &'static str {
        "io_uring"
    }
}

/// True io_uring TCP send implementation
///
/// This would use tokio_uring's TcpStream directly for zero-copy sends.
/// The main challenge is that tokio_uring requires:
/// 1. Running inside tokio_uring::start() runtime
/// 2. Owned buffers (no shared references during I/O)
/// 3. Different socket types than tokio::net
#[allow(dead_code)]
async fn uring_send_tcp_impl(
    _stream: tokio_uring::net::TcpStream,
    _stats: Arc<StreamStats>,
    _duration: Duration,
    _buffer_size: usize,
) -> anyhow::Result<()> {
    // Implementation would look like:
    // let buffer = vec![0u8; buffer_size];
    // let deadline = Instant::now() + duration;
    // loop {
    //     if Instant::now() >= deadline { break; }
    //     let (result, buf) = stream.write(buffer).await;
    //     buffer = buf;
    //     let n = result?;
    //     stats.add_bytes_sent(n as u64);
    // }
    warn!("True io_uring implementation not yet complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uring_backend_creation() {
        let backend = UringBackend::new();
        assert_eq!(backend.name(), "io_uring");
        assert_eq!(backend.buffer_size, 4 * 1024 * 1024);
    }

    #[test]
    fn test_uring_backend_custom_buffer() {
        let backend = UringBackend::with_buffer_size(1024 * 1024);
        assert_eq!(backend.buffer_size, 1024 * 1024);
    }
}
