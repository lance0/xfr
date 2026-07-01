//! JSON output

use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::protocol::TestResult;

fn serialize_pretty<T: Serialize + ?Sized>(value: &T) -> anyhow::Result<String> {
    serde_json::to_string_pretty(value)
        .map_err(|e| anyhow::anyhow!("failed to serialize value to JSON: {e}"))
}

pub fn output_json(result: &TestResult) -> anyhow::Result<String> {
    serialize_pretty(result)
        .map_err(|e| anyhow::anyhow!("failed to serialize test result to JSON: {e}"))
}

pub fn save_json(result: &TestResult, path: &Path) -> anyhow::Result<()> {
    let json = output_json(result)?;
    fs::write(path, json)?;
    Ok(())
}

/// JSON streaming interval output (one JSON object per line)
#[derive(Serialize)]
pub struct StreamInterval {
    pub timestamp: String,
    pub elapsed_secs: f64,
    pub bytes: u64,
    pub throughput_mbps: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtt_us: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwnd: Option<u32>,
}

/// Output interval as JSON line (for --json-stream)
#[allow(clippy::too_many_arguments)]
pub fn output_interval_json(
    timestamp: &str,
    elapsed_secs: f64,
    throughput_mbps: f64,
    bytes: u64,
    retransmits: Option<u64>,
    jitter_ms: Option<f64>,
    lost: Option<u64>,
    rtt_us: Option<u32>,
    cwnd: Option<u32>,
) -> anyhow::Result<String> {
    let interval = StreamInterval {
        timestamp: timestamp.to_string(),
        elapsed_secs,
        bytes,
        throughput_mbps,
        retransmits,
        jitter_ms,
        lost,
        rtt_us,
        cwnd,
    };
    serde_json::to_string(&interval)
        .map_err(|e| anyhow::anyhow!("failed to serialize interval to JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use crate::output::json::{output_interval_json, output_json, save_json, serialize_pretty};
    use crate::protocol::{StreamResult, TcpInfoSnapshot, TestResult};

    fn make_result() -> TestResult {
        TestResult {
            id: "test".to_string(),
            bytes_total: 1_000_000,
            duration_ms: 1_000,
            throughput_mbps: 8.0,
            streams: vec![StreamResult {
                id: 0,
                bytes: 1_000_000,
                throughput_mbps: 8.0,
                retransmits: None,
                jitter_ms: None,
                lost: None,
            }],
            tcp_info: Some(TcpInfoSnapshot {
                retransmits: 0,
                rtt_us: 100,
                rtt_var_us: 20,
                cwnd: 64 * 1024,
                bytes_acked: None,
            }),
            udp_stats: None,
            bytes_sent: None,
            bytes_received: None,
            throughput_send_mbps: None,
            throughput_recv_mbps: None,
            mtu_probe: None,
        }
    }

    #[test]
    fn test_output_json_serializes_result() {
        let json = output_json(&make_result()).expect("serialization should succeed");
        assert!(
            json.contains("\"id\": \"test\""),
            "expected id field, got: {}",
            json
        );
        assert!(json.contains("\"throughput_mbps\": 8"), "got: {}", json);
        assert!(json.contains("\"bytes_total\": 1000000"), "got: {}", json);
    }

    // Sentinel type whose Serialize impl always fails.
    struct AlwaysFails;

    impl serde::Serialize for AlwaysFails {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom(
                "intentional serialization failure",
            ))
        }
    }

    #[test]
    fn test_serialize_pretty_propagates_errors() {
        let result = serialize_pretty(&AlwaysFails);
        assert!(result.is_err(), "expected serialization error to propagate");
        let message = result.unwrap_err().to_string();
        assert!(
            message.contains("intentional serialization failure"),
            "error message should mention the cause: {}",
            message
        );
    }

    #[test]
    fn test_save_json_writes_valid_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("result.json");
        save_json(&make_result(), &path).expect("save_json should succeed");

        let contents = std::fs::read_to_string(&path).expect("read file");
        assert!(contents.contains("\"id\": \"test\""));
        let parsed: serde_json::Value =
            serde_json::from_str(&contents).expect("saved JSON should parse");
        assert_eq!(parsed["bytes_total"], 1_000_000);
    }

    #[test]
    fn test_output_interval_json_serializes_fields() {
        let line = output_interval_json(
            "2026-01-01T00:00:00Z",
            1.0,
            8.0,
            1_000_000,
            Some(5),
            Some(0.5),
            Some(1),
            Some(100),
            Some(64 * 1024),
        )
        .expect("interval serialization should succeed");

        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(parsed["timestamp"], "2026-01-01T00:00:00Z");
        assert_eq!(parsed["throughput_mbps"], 8.0);
        assert_eq!(parsed["bytes"], 1_000_000);
        assert_eq!(parsed["retransmits"], 5);
    }
}
