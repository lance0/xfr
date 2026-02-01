//! Audit logging for security events
//!
//! Provides structured logging for authentication, access control, and test events.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;

/// Audit event types
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEvent {
    ClientConnect {
        ip: IpAddr,
        tls: bool,
    },
    AuthSuccess {
        ip: IpAddr,
        method: String,
    },
    AuthFailure {
        ip: IpAddr,
        method: String,
        reason: String,
    },
    TestStart {
        ip: IpAddr,
        test_id: String,
        protocol: String,
        streams: u8,
        direction: String,
        duration_secs: u32,
    },
    TestComplete {
        ip: IpAddr,
        test_id: String,
        bytes: u64,
        duration_ms: u64,
        throughput_mbps: f64,
    },
    TestCancelled {
        ip: IpAddr,
        test_id: String,
        reason: String,
    },
    RateLimitHit {
        ip: IpAddr,
        current: u32,
        max: u32,
    },
    AclDenied {
        ip: IpAddr,
        rule: Option<String>,
    },
}

/// Audit log entry
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub ts: DateTime<Utc>,
    #[serde(flatten)]
    pub event: AuditEvent,
}

/// Audit log format
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuditFormat {
    #[default]
    Json,
    Text,
}

impl std::str::FromStr for AuditFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "json" => Ok(AuditFormat::Json),
            "text" => Ok(AuditFormat::Text),
            _ => Err(format!("Invalid audit format: {}. Valid: json, text", s)),
        }
    }
}

impl std::fmt::Display for AuditFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditFormat::Json => write!(f, "json"),
            AuditFormat::Text => write!(f, "text"),
        }
    }
}

/// Audit logger
pub struct AuditLogger {
    file: Mutex<File>,
    format: AuditFormat,
}

impl AuditLogger {
    /// Create a new audit logger
    pub fn new(path: &Path, format: AuditFormat) -> anyhow::Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new().create(true).append(true).open(path)?;

        Ok(Self {
            file: Mutex::new(file),
            format,
        })
    }

    /// Log an event
    pub fn log(&self, event: AuditEvent) {
        let entry = AuditEntry {
            ts: Utc::now(),
            event,
        };

        let line = match self.format {
            AuditFormat::Json => self.format_json(&entry),
            AuditFormat::Text => self.format_text(&entry),
        };

        if let Err(e) = writeln!(self.file.lock(), "{}", line) {
            tracing::error!("Failed to write audit log: {}", e);
        }
    }

    fn format_json(&self, entry: &AuditEntry) -> String {
        serde_json::to_string(entry).unwrap_or_else(|_| "{}".to_string())
    }

    fn format_text(&self, entry: &AuditEntry) -> String {
        let ts = entry.ts.format("%Y-%m-%dT%H:%M:%SZ");
        match &entry.event {
            AuditEvent::ClientConnect { ip, tls } => {
                format!("{} [CLIENT_CONNECT] ip={} tls={}", ts, ip, tls)
            }
            AuditEvent::AuthSuccess { ip, method } => {
                format!("{} [AUTH_SUCCESS] ip={} method={}", ts, ip, method)
            }
            AuditEvent::AuthFailure { ip, method, reason } => {
                format!(
                    "{} [AUTH_FAILURE] ip={} method={} reason={}",
                    ts, ip, method, reason
                )
            }
            AuditEvent::TestStart {
                ip,
                test_id,
                protocol,
                streams,
                direction,
                duration_secs,
            } => {
                format!(
                    "{} [TEST_START] ip={} test_id={} protocol={} streams={} direction={} duration={}s",
                    ts, ip, test_id, protocol, streams, direction, duration_secs
                )
            }
            AuditEvent::TestComplete {
                ip,
                test_id,
                bytes,
                duration_ms,
                throughput_mbps,
            } => {
                format!(
                    "{} [TEST_COMPLETE] ip={} test_id={} bytes={} duration={}ms throughput={:.2}Mbps",
                    ts, ip, test_id, bytes, duration_ms, throughput_mbps
                )
            }
            AuditEvent::TestCancelled {
                ip,
                test_id,
                reason,
            } => {
                format!(
                    "{} [TEST_CANCELLED] ip={} test_id={} reason={}",
                    ts, ip, test_id, reason
                )
            }
            AuditEvent::RateLimitHit { ip, current, max } => {
                format!(
                    "{} [RATE_LIMIT_HIT] ip={} current={} max={}",
                    ts, ip, current, max
                )
            }
            AuditEvent::AclDenied { ip, rule } => {
                format!(
                    "{} [ACL_DENIED] ip={} rule={}",
                    ts,
                    ip,
                    rule.as_deref().unwrap_or("default")
                )
            }
        }
    }
}

/// Audit logger configuration
#[derive(Debug, Clone, Default)]
pub struct AuditConfig {
    pub path: Option<String>,
    pub format: AuditFormat,
}

impl AuditConfig {
    /// Build an audit logger from this configuration
    pub fn build(&self) -> anyhow::Result<Option<Arc<AuditLogger>>> {
        match &self.path {
            Some(path) => {
                let logger = AuditLogger::new(Path::new(path), self.format)?;
                Ok(Some(Arc::new(logger)))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tempfile::tempdir;

    #[test]
    fn test_json_format() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let logger = AuditLogger::new(&path, AuditFormat::Json).unwrap();

        logger.log(AuditEvent::ClientConnect {
            ip: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            tls: true,
        });

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"ip\":\"192.168.1.1\""));
        assert!(content.contains("\"tls\":true"));
    }

    #[test]
    fn test_text_format() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let logger = AuditLogger::new(&path, AuditFormat::Text).unwrap();

        logger.log(AuditEvent::AuthFailure {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            method: "psk".to_string(),
            reason: "invalid response".to_string(),
        });

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[AUTH_FAILURE]"));
        assert!(content.contains("ip=10.0.0.1"));
        assert!(content.contains("method=psk"));
    }
}
