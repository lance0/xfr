//! xfr - Modern network bandwidth testing with TUI
//!
//! A fast, beautiful iperf replacement built in Rust.

pub mod acl;
pub mod auth;
pub mod client;
pub mod config;
pub mod diff;
pub mod discover;
pub mod net;
pub mod output;
pub mod prefs;
pub mod protocol;
pub mod quic;
pub mod rate_limit;
pub mod serve;
pub mod stats;
pub mod tcp;
pub mod tcp_info;
pub mod tui;
pub mod udp;

pub use client::{Client, ClientConfig};
pub use protocol::{ControlMessage, Direction, Protocol, TestResult};
pub use serve::{Server, ServerConfig};
