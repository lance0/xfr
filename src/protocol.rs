//! Control protocol for xfr client-server communication
//!
//! The control channel uses newline-delimited JSON messages over TCP/QUIC:
//!
//! ```text
//! {"type":"Hello","version":"1.1",...}\n
//! {"type":"ServerHello",...}\n
//! {"type":"TestStart",...}\n
//! ```
//!
//! Each message is a single JSON object followed by a newline character.
//!
//! # Protocol Versioning
//!
//! Protocol version follows semver conventions:
//! - Major version changes break compatibility
//! - Minor version changes are backwards compatible
//!
//! Use [`versions_compatible`] to check compatibility.
//!
//! # Message Flow
//!
//! ```text
//! Client                          Server
//!   │                               │
//!   │──────── Hello ───────────────>│  (control connection)
//!   │<─────── Hello (+ auth?) ──────│
//!   │──────── AuthResponse? ───────>│
//!   │<─────── AuthSuccess? ─────────│
//!   │──────── TestStart ───────────>│
//!   │<─────── TestAck ──────────────│
//!   │                               │
//!   │──────── DataHello ───────────>│  (data connection 1, same port)
//!   │──────── DataHello ───────────>│  (data connection 2, same port)
//!   │      [Data Transfer]          │
//!   │                               │
//!   │<─────── Interval ─────────────│ (periodic, on control)
//!   │<─────── Result ───────────────│
//!   │                               │
//! ```

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "1.1";
pub const DEFAULT_PORT: u16 = 5201;

/// Check if two protocol versions are compatible.
///
/// Compatibility rules:
/// - Major versions must match exactly
/// - Minor version differences are allowed (backwards compatible)
pub fn versions_compatible(version_a: &str, version_b: &str) -> bool {
    let parse_major = |v: &str| -> u32 {
        v.split('.')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    };
    parse_major(version_a) == parse_major(version_b)
}

/// Authentication challenge sent by server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthChallenge {
    pub method: String,
    pub nonce: String,
}

/// Authentication response from client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    pub response: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    Hello {
        version: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        client: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        server: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        capabilities: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        auth: Option<AuthChallenge>,
    },
    AuthResponse {
        response: String,
    },
    AuthSuccess,
    TestStart {
        id: String,
        protocol: Protocol,
        streams: u8,
        duration_secs: u32,
        direction: Direction,
        #[serde(skip_serializing_if = "Option::is_none")]
        bitrate: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        congestion: Option<String>,
    },
    TestAck {
        id: String,
        data_ports: Vec<u16>,
    },
    Interval {
        id: String,
        elapsed_ms: u64,
        streams: Vec<StreamInterval>,
        aggregate: AggregateInterval,
    },
    Result(TestResult),
    Cancel {
        id: String,
        reason: String,
    },
    Cancelled {
        id: String,
    },
    Error {
        message: String,
    },
    /// Data channel handshake for single-port TCP mode
    /// Sent by client on each data connection to identify stream
    DataHello {
        test_id: String,
        stream_index: u16,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Tcp,
    Udp,
    Quic,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Tcp => write!(f, "TCP"),
            Protocol::Udp => write!(f, "UDP"),
            Protocol::Quic => write!(f, "QUIC"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Upload,
    Download,
    Bidir,
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Direction::Upload => write!(f, "Upload"),
            Direction::Download => write!(f, "Download"),
            Direction::Bidir => write!(f, "Bidirectional"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInterval {
    pub id: u8,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateInterval {
    pub bytes: u64,
    pub throughput_mbps: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub id: String,
    pub bytes_total: u64,
    pub duration_ms: u64,
    pub throughput_mbps: f64,
    pub streams: Vec<StreamResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_info: Option<TcpInfoSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udp_stats: Option<UdpStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamResult {
    pub id: u8,
    pub bytes: u64,
    pub throughput_mbps: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpInfoSnapshot {
    pub retransmits: u64,
    pub rtt_us: u32,
    pub rtt_var_us: u32,
    pub cwnd: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpStats {
    pub packets_sent: u64,
    pub packets_received: u64,
    pub lost: u64,
    pub lost_percent: f64,
    pub jitter_ms: f64,
    pub out_of_order: u64,
}

/// Timestamp format options for interval output
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TimestampFormat {
    /// Seconds since test start (default): [5.2s]
    #[default]
    Relative,
    /// ISO 8601 format: 2026-01-31T21:45:30Z
    Iso8601,
    /// Unix epoch with milliseconds: 1738356330.123
    Unix,
}

impl TimestampFormat {
    /// Format a timestamp based on the format type
    ///
    /// # Arguments
    /// * `test_start` - When the test started (monotonic, for relative calculation)
    /// * `now` - Current monotonic time
    /// * `system_start` - Wall clock time when test started (for ISO8601/Unix)
    pub fn format(
        &self,
        test_start: std::time::Instant,
        now: std::time::Instant,
        system_start: std::time::SystemTime,
    ) -> String {
        match self {
            TimestampFormat::Relative => {
                let elapsed = now.duration_since(test_start);
                format!("{:.3}", elapsed.as_secs_f64())
            }
            TimestampFormat::Iso8601 => {
                let elapsed = now.duration_since(test_start);
                let wall_time = system_start + elapsed;
                let datetime = chrono::DateTime::<chrono::Utc>::from(wall_time);
                datetime.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
            }
            TimestampFormat::Unix => {
                let elapsed = now.duration_since(test_start);
                let wall_time = system_start + elapsed;
                let duration_since_epoch = wall_time
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                format!("{:.3}", duration_since_epoch.as_secs_f64())
            }
        }
    }

    /// Format for CLI help text
    pub fn variants() -> &'static [&'static str] {
        &["relative", "iso8601", "unix"]
    }
}

impl std::str::FromStr for TimestampFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "relative" => Ok(TimestampFormat::Relative),
            "iso8601" | "iso" => Ok(TimestampFormat::Iso8601),
            "unix" | "epoch" => Ok(TimestampFormat::Unix),
            _ => Err(format!(
                "Invalid timestamp format: {}. Valid options: {}",
                s,
                Self::variants().join(", ")
            )),
        }
    }
}

impl std::fmt::Display for TimestampFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimestampFormat::Relative => write!(f, "relative"),
            TimestampFormat::Iso8601 => write!(f, "iso8601"),
            TimestampFormat::Unix => write!(f, "unix"),
        }
    }
}

impl ControlMessage {
    pub fn client_hello() -> Self {
        ControlMessage::Hello {
            version: PROTOCOL_VERSION.to_string(),
            client: Some(format!("xfr/{}", env!("CARGO_PKG_VERSION"))),
            server: None,
            capabilities: Some(vec![
                "tcp".to_string(),
                "udp".to_string(),
                "quic".to_string(),
                "multistream".to_string(),
                "single_port_tcp".to_string(),
            ]),
            auth: None,
        }
    }

    pub fn server_hello() -> Self {
        ControlMessage::Hello {
            version: PROTOCOL_VERSION.to_string(),
            client: None,
            server: Some(format!("xfr/{}", env!("CARGO_PKG_VERSION"))),
            capabilities: Some(vec![
                "tcp".to_string(),
                "udp".to_string(),
                "quic".to_string(),
                "multistream".to_string(),
                "single_port_tcp".to_string(),
            ]),
            auth: None,
        }
    }

    pub fn server_hello_with_auth(nonce: String) -> Self {
        ControlMessage::Hello {
            version: PROTOCOL_VERSION.to_string(),
            client: None,
            server: Some(format!("xfr/{}", env!("CARGO_PKG_VERSION"))),
            capabilities: Some(vec![
                "tcp".to_string(),
                "udp".to_string(),
                "quic".to_string(),
                "multistream".to_string(),
                "single_port_tcp".to_string(),
            ]),
            auth: Some(AuthChallenge {
                method: "psk".to_string(),
                nonce,
            }),
        }
    }

    pub fn auth_response(response: String) -> Self {
        ControlMessage::AuthResponse { response }
    }

    pub fn auth_success() -> Self {
        ControlMessage::AuthSuccess
    }

    pub fn error(message: impl Into<String>) -> Self {
        ControlMessage::Error {
            message: message.into(),
        }
    }

    pub fn serialize(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn deserialize(s: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(s)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_hello() {
        let msg = ControlMessage::client_hello();
        let json = msg.serialize().unwrap();
        assert!(json.contains("\"type\":\"hello\""));
        assert!(json.contains("\"version\":\"1.1\""));
    }

    #[test]
    fn test_roundtrip_test_start() {
        let msg = ControlMessage::TestStart {
            id: "test123".to_string(),
            protocol: Protocol::Tcp,
            streams: 4,
            duration_secs: 10,
            direction: Direction::Upload,
            bitrate: None,
            congestion: None,
        };
        let json = msg.serialize().unwrap();
        let decoded = ControlMessage::deserialize(&json).unwrap();

        match decoded {
            ControlMessage::TestStart { id, streams, .. } => {
                assert_eq!(id, "test123");
                assert_eq!(streams, 4);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_protocol_display() {
        assert_eq!(Protocol::Tcp.to_string(), "TCP");
        assert_eq!(Protocol::Udp.to_string(), "UDP");
    }
}
