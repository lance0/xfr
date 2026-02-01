//! JSON output

use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::protocol::TestResult;

pub fn output_json(result: &TestResult) -> String {
    serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".to_string())
}

pub fn save_json(result: &TestResult, path: &Path) -> anyhow::Result<()> {
    let json = output_json(result);
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
}

/// Output interval as JSON line (for --json-stream)
pub fn output_interval_json(
    timestamp: &str,
    elapsed_secs: f64,
    throughput_mbps: f64,
    bytes: u64,
    retransmits: Option<u64>,
    jitter_ms: Option<f64>,
    lost: Option<u64>,
) -> String {
    let interval = StreamInterval {
        timestamp: timestamp.to_string(),
        elapsed_secs,
        bytes,
        throughput_mbps,
        retransmits,
        jitter_ms,
        lost,
    };
    serde_json::to_string(&interval).unwrap_or_else(|_| "{}".to_string())
}
