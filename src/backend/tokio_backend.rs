//! Tokio-based I/O backend (default).
//!
//! Uses standard tokio async I/O with epoll (Linux) or kqueue (macOS).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::watch;

use super::DataBackend;
use crate::protocol::UdpStats;
use crate::stats::StreamStats;
use crate::tcp::{self, TcpConfig};
use crate::udp::{self, UdpSendStats};

/// Tokio-based data transfer backend
pub struct TokioBackend;

impl TokioBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TokioBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DataBackend for TokioBackend {
    async fn send_tcp(
        &self,
        stream: TcpStream,
        stats: Arc<StreamStats>,
        duration: Duration,
        config: TcpConfig,
        cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        tcp::send_data(stream, stats, duration, config, cancel).await
    }

    async fn recv_tcp(
        &self,
        stream: TcpStream,
        stats: Arc<StreamStats>,
        config: TcpConfig,
        cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        tcp::receive_data(stream, stats, cancel, config).await
    }

    async fn send_udp(
        &self,
        socket: Arc<UdpSocket>,
        target_bitrate: u64,
        duration: Duration,
        stats: Arc<StreamStats>,
        cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<UdpSendStats> {
        udp::send_udp_paced(socket, target_bitrate, duration, stats, cancel).await
    }

    async fn recv_udp(
        &self,
        socket: Arc<UdpSocket>,
        stats: Arc<StreamStats>,
        cancel: watch::Receiver<bool>,
    ) -> anyhow::Result<(UdpStats, u64)> {
        udp::receive_udp(socket, stats, cancel).await
    }

    fn name(&self) -> &'static str {
        "tokio"
    }
}
