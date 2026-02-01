//! Diff mode - compare two test results
//!
//! Compares bandwidth test results and reports regressions.

use std::fs;
use std::path::Path;

use crate::protocol::TestResult;

#[derive(Debug)]
pub struct DiffConfig {
    pub threshold_percent: f64,
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            threshold_percent: 0.0,
        }
    }
}

#[derive(Debug)]
pub struct DiffResult {
    pub baseline: TestResult,
    pub current: TestResult,
    pub throughput_change_percent: f64,
    pub retransmit_change_percent: f64,
    pub rtt_change_percent: f64,
    pub is_regression: bool,
}

impl DiffResult {
    pub fn format_plain(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!(
            "Throughput: {:.1} Mbps → {:.1} Mbps ({:+.1}%)",
            self.baseline.throughput_mbps,
            self.current.throughput_mbps,
            self.throughput_change_percent
        ));

        if self.throughput_change_percent < -5.0 {
            output.push_str(" ✗");
        } else if self.throughput_change_percent < 0.0 {
            output.push_str(" ⚠");
        } else {
            output.push_str(" ✓");
        }
        output.push('\n');

        if let (Some(baseline_tcp), Some(current_tcp)) =
            (&self.baseline.tcp_info, &self.current.tcp_info)
        {
            output.push_str(&format!(
                "Retransmits: {} → {} ({:+.1}%)",
                baseline_tcp.retransmits, current_tcp.retransmits, self.retransmit_change_percent
            ));
            if self.retransmit_change_percent > 50.0 {
                output.push_str(" ✗");
            } else if self.retransmit_change_percent > 20.0 {
                output.push_str(" ⚠");
            }
            output.push('\n');

            let baseline_rtt_ms = baseline_tcp.rtt_us as f64 / 1000.0;
            let current_rtt_ms = current_tcp.rtt_us as f64 / 1000.0;
            output.push_str(&format!(
                "RTT: {:.2} ms → {:.2} ms ({:+.1}%)",
                baseline_rtt_ms, current_rtt_ms, self.rtt_change_percent
            ));
            if self.rtt_change_percent > 20.0 {
                output.push_str(" ⚠");
            }
            output.push('\n');
        }

        output.push('\n');
        if self.is_regression {
            output.push_str("Verdict: REGRESSION DETECTED\n");
        } else {
            output.push_str("Verdict: OK\n");
        }

        output
    }
}

pub fn load_result(path: &Path) -> anyhow::Result<TestResult> {
    let content = fs::read_to_string(path)?;
    let result: TestResult = serde_json::from_str(&content)?;
    Ok(result)
}

pub fn compare(baseline: TestResult, current: TestResult, config: &DiffConfig) -> DiffResult {
    let throughput_change_percent = if baseline.throughput_mbps > 0.0 {
        ((current.throughput_mbps - baseline.throughput_mbps) / baseline.throughput_mbps) * 100.0
    } else {
        0.0
    };

    let retransmit_change_percent = match (&baseline.tcp_info, &current.tcp_info) {
        (Some(b), Some(c)) if b.retransmits > 0 => {
            ((c.retransmits as f64 - b.retransmits as f64) / b.retransmits as f64) * 100.0
        }
        _ => 0.0,
    };

    let rtt_change_percent = match (&baseline.tcp_info, &current.tcp_info) {
        (Some(b), Some(c)) if b.rtt_us > 0 => {
            ((c.rtt_us as f64 - b.rtt_us as f64) / b.rtt_us as f64) * 100.0
        }
        _ => 0.0,
    };

    let is_regression = throughput_change_percent < -config.threshold_percent;

    DiffResult {
        baseline,
        current,
        throughput_change_percent,
        retransmit_change_percent,
        rtt_change_percent,
        is_regression,
    }
}

pub fn run_diff(
    baseline_path: &Path,
    current_path: &Path,
    config: &DiffConfig,
) -> anyhow::Result<DiffResult> {
    let baseline = load_result(baseline_path)?;
    let current = load_result(current_path)?;
    Ok(compare(baseline, current, config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TcpInfoSnapshot;

    fn make_result(throughput: f64, retransmits: u64, rtt_us: u32) -> TestResult {
        TestResult {
            id: "test".to_string(),
            bytes_total: 1_000_000_000,
            duration_ms: 10_000,
            throughput_mbps: throughput,
            streams: vec![],
            tcp_info: Some(TcpInfoSnapshot {
                retransmits,
                rtt_us,
                rtt_var_us: 100,
                cwnd: 65535,
            }),
            udp_stats: None,
        }
    }

    #[test]
    fn test_no_regression() {
        let baseline = make_result(1000.0, 10, 1000);
        let current = make_result(1050.0, 8, 900);

        let diff = compare(baseline, current, &DiffConfig::default());
        assert!(!diff.is_regression);
        assert!(diff.throughput_change_percent > 0.0);
    }

    #[test]
    fn test_regression() {
        let baseline = make_result(1000.0, 10, 1000);
        let current = make_result(800.0, 50, 1500);

        let diff = compare(
            baseline,
            current,
            &DiffConfig {
                threshold_percent: 5.0,
            },
        );
        assert!(diff.is_regression);
        assert!(diff.throughput_change_percent < -10.0);
    }
}
