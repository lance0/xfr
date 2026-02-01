use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "1.0";
pub const DEFAULT_PORT: u16 = 5201;

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
    },
    TestStart {
        id: String,
        protocol: Protocol,
        streams: u8,
        duration_secs: u32,
        direction: Direction,
        #[serde(skip_serializing_if = "Option::is_none")]
        bitrate: Option<u64>,
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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Tcp,
    Udp,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Tcp => write!(f, "TCP"),
            Protocol::Udp => write!(f, "UDP"),
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

impl ControlMessage {
    pub fn client_hello() -> Self {
        ControlMessage::Hello {
            version: PROTOCOL_VERSION.to_string(),
            client: Some(format!("xfr/{}", env!("CARGO_PKG_VERSION"))),
            server: None,
            capabilities: None,
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
                "multistream".to_string(),
            ]),
        }
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
        assert!(json.contains("\"version\":\"1.0\""));
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
