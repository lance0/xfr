//! Events the server publishes about its tests.
//!
//! The server dashboard consumes them, but the server emits them with or
//! without the `tui` feature, so they live outside the `tui` module.

use std::net::IpAddr;
use std::time::Instant;

/// Events sent from server to TUI
#[derive(Debug, Clone)]
pub enum ServerEvent {
    /// A new test has started
    TestStarted(ActiveTestInfo),
    /// Test progress update
    TestUpdated {
        id: String,
        bytes: u64,
        throughput_mbps: f64,
    },
    /// Test completed
    TestCompleted { id: String, bytes: u64 },
    /// Test failed before normal completion
    TestFailed { id: String, bytes: u64 },
    /// Connection blocked by ACL or rate limit
    ConnectionBlocked,
    /// Authentication failure
    AuthFailure,
}

/// Active test info for display
#[derive(Debug, Clone)]
pub struct ActiveTestInfo {
    pub id: String,
    pub client_ip: IpAddr,
    pub protocol: String,
    pub direction: String,
    pub streams: u8,
    pub started: Instant,
    pub duration_secs: u32,
    pub bytes: u64,
    pub throughput_mbps: f64,
}
