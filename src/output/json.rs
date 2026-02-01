//! JSON output

use std::fs;
use std::path::Path;

use crate::protocol::TestResult;

pub fn output_json(result: &TestResult) -> String {
    serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".to_string())
}

pub fn save_json(result: &TestResult, path: &Path) -> anyhow::Result<()> {
    let json = output_json(result);
    fs::write(path, json)?;
    Ok(())
}
