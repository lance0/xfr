//! xfr - Modern network bandwidth testing with TUI
//!
//! A fast, beautiful iperf replacement built in Rust.
//!
//! # Library Usage
//!
//! xfr can be used as a library for embedding bandwidth tests in your application:
//!
//! ```no_run
//! use xfr::{Client, ClientConfig, Server, ServerConfig};
//! use std::net::SocketAddr;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Run a client test
//!     let config = ClientConfig {
//!         server: "192.168.1.1:5201".parse()?,
//!         duration_secs: 10,
//!         parallel_streams: 4,
//!         ..Default::default()
//!     };
//!
//!     let client = Client::new(config);
//!     let result = client.run().await?;
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

pub use client::{Client, ClientConfig};
pub use protocol::{ControlMessage, Direction, Protocol, TestResult};
pub use serve::{Server, ServerConfig};
