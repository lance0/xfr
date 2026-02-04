//! xfr - Modern network bandwidth testing with TUI
//!
//! A fast, beautiful iperf replacement built in Rust.
//!
//! # Library Usage
//!
//! xfr can be used as a library for embedding bandwidth tests in your application:
//!
//! ```ignore
//! use xfr::{Client, ClientConfig};
//! use std::time::Duration;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let config = ClientConfig {
//!         host: "192.168.1.1".to_string(),
//!         streams: 4,
//!         duration: Duration::from_secs(10),
//!         ..Default::default()
//!     };
//!
//!     let client = Client::new(config);
//!     let result = client.run(None).await?;
//!
//!     println!("Throughput: {:.2} Mbps", result.throughput_mbps);
//!     Ok(())
//! }
//! ```
//!
//! # Modules
//!
//! - [`client`] - Client-side test orchestration
//! - [`serve`] - Server-side connection handling
//! - [`protocol`] - Control protocol messages
//! - [`tcp`], [`udp`], [`quic`] - Transport implementations
//! - [`stats`] - Statistics collection
//! - [`tui`] - Terminal UI rendering

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
pub mod update;

pub use client::{Client, ClientConfig};
pub use protocol::{ControlMessage, Direction, Protocol, TestResult};
pub use serve::{Server, ServerConfig};
