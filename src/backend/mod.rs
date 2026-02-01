//! I/O backend abstraction for data transfer.
//!
//! Provides a trait-based abstraction over different I/O implementations:
//! - `tokio` (default): Standard tokio async I/O using epoll/kqueue
//! - `uring` (optional): Linux io_uring for reduced syscall overhead
//!
//! The appropriate backend is auto-detected at runtime based on platform
//! and kernel version.

mod tokio_backend;

#[cfg(all(target_os = "linux", feature = "io-uring"))]
mod uring;

pub use tokio_backend::TokioBackend;

#[cfg(all(target_os = "linux", feature = "io-uring"))]
pub use uring::UringBackend;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::{TcpStream, UdpSocket};

use crate::protocol::UdpStats;
use crate::stats::StreamStats;
use crate::tcp::TcpConfig;
use crate::udp::UdpSendStats;

/// Backend selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendType {
    /// Auto-detect best available backend
    #[default]
    Auto,
    /// Standard tokio async I/O
    Tokio,
    /// Linux io_uring (requires feature and Linux 5.10+)
    #[cfg(all(target_os = "linux", feature = "io-uring"))]
    Uring,
}

impl std::str::FromStr for BackendType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "tokio" | "epoll" | "kqueue" => Ok(Self::Tokio),
            #[cfg(all(target_os = "linux", feature = "io-uring"))]
            "uring" | "io_uring" | "io-uring" => Ok(Self::Uring),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for BackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Tokio => write!(f, "tokio"),
            #[cfg(all(target_os = "linux", feature = "io-uring"))]
            Self::Uring => write!(f, "io_uring"),
        }
    }
}

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

/// Create the appropriate backend based on type and platform
pub fn create_backend(backend_type: BackendType) -> Box<dyn DataBackend> {
    match backend_type {
        BackendType::Auto => {
            #[cfg(all(target_os = "linux", feature = "io-uring"))]
            if is_uring_available() {
                tracing::info!("Using io_uring backend");
                return Box::new(UringBackend::new());
            }

            tracing::debug!("Using tokio backend");
            Box::new(TokioBackend::new())
        }
        BackendType::Tokio => Box::new(TokioBackend::new()),
        #[cfg(all(target_os = "linux", feature = "io-uring"))]
        BackendType::Uring => Box::new(UringBackend::new()),
    }
}

/// Check if io_uring is available on this system
#[cfg(all(target_os = "linux", feature = "io-uring"))]
fn is_uring_available() -> bool {
    // Check kernel version >= 5.10
    if let Ok(release) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        let parts: Vec<&str> = release.trim().split('.').collect();
        if parts.len() >= 2
            && let (Ok(major), Ok(minor)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>())
            && (major > 5 || (major == 5 && minor >= 10))
        {
            // Try to create a test ring to verify io_uring works
            return true; // Simplified - actual check would probe io_uring
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_type_from_str() {
        assert_eq!("auto".parse::<BackendType>(), Ok(BackendType::Auto));
        assert_eq!("tokio".parse::<BackendType>(), Ok(BackendType::Tokio));
        assert_eq!("epoll".parse::<BackendType>(), Ok(BackendType::Tokio));
        assert_eq!("invalid".parse::<BackendType>(), Err(()));
    }

    #[test]
    fn test_create_backend() {
        let backend = create_backend(BackendType::Tokio);
        assert_eq!(backend.name(), "tokio");
    }
}
