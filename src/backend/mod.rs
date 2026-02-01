//! I/O backend abstraction for data transfer.
//!
//! Currently uses tokio for async I/O (epoll/kqueue).

mod tokio_backend;

pub use tokio_backend::TokioBackend;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::{TcpStream, UdpSocket};

use crate::protocol::UdpStats;
use crate::stats::StreamStats;
use crate::tcp::TcpConfig;
use crate::udp::UdpSendStats;

/// Trait for data transfer backends
#[async_trait]
pub trait DataBackend: Send + Sync {
    /// Send TCP data for the given duration
    async fn send_tcp(
        &self,
        stream: TcpStream,
        stats: Arc<StreamStats>,
        duration: Duration,
        config: TcpConfig,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()>;

    /// Receive TCP data until EOF or cancel
    async fn recv_tcp(
        &self,
        stream: TcpStream,
        stats: Arc<StreamStats>,
        config: TcpConfig,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()>;

    /// Send UDP data with pacing
    async fn send_udp(
        &self,
        socket: Arc<UdpSocket>,
        target_bitrate: u64,
        duration: Duration,
        stats: Arc<StreamStats>,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<UdpSendStats>;

    /// Receive UDP data and return (stats, packets_received)
    async fn recv_udp(
        &self,
        socket: Arc<UdpSocket>,
        stats: Arc<StreamStats>,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<(UdpStats, u64)>;

    /// Get backend name for logging
    fn name(&self) -> &'static str;
}

/// Create the default backend
pub fn create_backend() -> Box<dyn DataBackend> {
    Box::new(TokioBackend::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_backend() {
        let backend = create_backend();
        assert_eq!(backend.name(), "tokio");
    }
}
