//! LAN discovery via mDNS
//!
//! Finds xfr servers on the local network using mDNS service discovery.

use std::net::IpAddr;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct DiscoveredServer {
    pub ip: IpAddr,
    pub port: u16,
    pub hostname: Option<String>,
    pub version: Option<String>,
}

/// RFC 1035 §3.1: a DNS label is at most 63 bytes. Linux `HOST_NAME_MAX` is
/// 64, so a legal nodename can be one byte too long for mDNS. mdns-sd 0.21
/// then skips the oversize record at encode time (`WriteError::NameTooLong`)
/// while `register()` still returns `Ok` — #194.
///
/// Compiled for `discovery` and for unit tests so `--no-default-features`
/// clippy does not treat the sanitizer as dead code.
#[cfg(any(feature = "discovery", test))]
const DNS_LABEL_MAX: usize = 63;

#[cfg(any(feature = "discovery", test))]
fn dns_label(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return "xfr-server".to_string();
    }
    if name.len() <= DNS_LABEL_MAX {
        return name.to_string();
    }

    // '-' + 4 hex from FNV-1a keeps two 64-byte names that share a 63-byte
    // prefix from advertising the same instance.
    let suffix = format!("-{:04x}", fnv1a_16(name.as_bytes()));
    let mut end = DNS_LABEL_MAX - suffix.len();
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + suffix.len());
    out.push_str(&name[..end]);
    out.push_str(&suffix);
    out
}

#[cfg(any(feature = "discovery", test))]
fn fnv1a_16(bytes: &[u8]) -> u16 {
    const OFFSET: u32 = 2_166_136_261;
    const PRIME: u32 = 16_777_619;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash as u16
}

#[cfg(feature = "discovery")]
mod mdns_impl {
    use super::*;
    use mdns_sd::{ServiceDaemon, ServiceEvent};
    use tracing::{debug, info, warn};

    const SERVICE_TYPE: &str = "_xfr._tcp.local.";

    pub async fn discover(timeout: Duration) -> anyhow::Result<Vec<DiscoveredServer>> {
        let mdns = ServiceDaemon::new()?;
        let receiver = mdns.browse(SERVICE_TYPE)?;

        let mut servers = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;

        info!("Searching for xfr servers...");

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }

            // Use recv_timeout to avoid blocking indefinitely
            // Cap at 100ms to allow periodic deadline checks
            let wait_time = remaining.min(Duration::from_millis(100));
            let event = receiver.recv_timeout(wait_time);

            match event {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    debug!("Service resolved: {:?}", info);

                    for addr in info.get_addresses() {
                        let server = DiscoveredServer {
                            ip: addr.to_ip_addr(),
                            port: info.get_port(),
                            hostname: Some(info.get_hostname().to_string()),
                            version: info
                                .get_properties()
                                .get("version")
                                .map(|v| v.val_str().to_string()),
                        };

                        // Avoid duplicates
                        if !servers.iter().any(|s: &DiscoveredServer| s.ip == server.ip) {
                            info!(
                                "Found server: {}:{} ({})",
                                server.ip,
                                server.port,
                                server.hostname.as_deref().unwrap_or("unknown")
                            );
                            servers.push(server);
                        }
                    }
                }
                Ok(ServiceEvent::ServiceFound(service_type, name)) => {
                    debug!("Service found: {} {}", service_type, name);
                }
                Ok(ServiceEvent::SearchStarted(_)) => {
                    debug!("mDNS search started");
                }
                Ok(event) => {
                    debug!("mDNS event: {:?}", event);
                }
                Err(_) => {
                    // Timeout or disconnected - continue to check deadline
                }
            }
        }

        mdns.shutdown()?;
        Ok(servers)
    }

    pub fn register_server(port: u16) -> anyhow::Result<ServiceDaemon> {
        use mdns_sd::ServiceInfo;

        let mdns = ServiceDaemon::new()?;

        let raw_hostname = hostname::get()
            .map(|h: std::ffi::OsString| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "xfr-server".to_string());
        let hostname = dns_label(&raw_hostname);
        if hostname != raw_hostname {
            warn!(
                "system hostname {:?} exceeds the 63-byte DNS label limit; advertising {:?}",
                raw_hostname, hostname
            );
        }

        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &hostname,
            &format!("{}.local.", hostname),
            "",
            port,
            [("version", env!("CARGO_PKG_VERSION"))].as_ref(),
        )?
        .enable_addr_auto();

        mdns.register(service)?;
        info!("Registered mDNS service: {}", hostname);

        Ok(mdns)
    }
}

#[cfg(not(feature = "discovery"))]
mod fallback {
    use super::*;

    pub async fn discover(_timeout: Duration) -> anyhow::Result<Vec<DiscoveredServer>> {
        Err(anyhow::anyhow!(
            "Discovery feature not enabled. Rebuild with --features discovery"
        ))
    }

    // Stub kept for API parity with the discovery-enabled build.
    // Serve-mode callers are cfg-gated on the feature, so this is dead
    // in a --no-default-features build.
    #[allow(dead_code)]
    pub fn register_server(_port: u16) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(feature = "discovery")]
pub use mdns_impl::{discover, register_server};

#[cfg(not(feature = "discovery"))]
pub use fallback::discover;

impl std::fmt::Display for DiscoveredServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.ip, self.port)?;
        if let Some(ref hostname) = self.hostname {
            write!(f, " ({})", hostname)?;
        }
        if let Some(ref version) = self.version {
            write!(f, " xfr/{}", version)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DNS_LABEL_MAX, dns_label};

    #[test]
    fn short_hostname_is_unchanged() {
        assert_eq!(dns_label("xfr-short"), "xfr-short");
    }

    #[test]
    fn sixty_three_byte_hostname_is_kept() {
        let raw = "y".repeat(DNS_LABEL_MAX);
        assert_eq!(dns_label(&raw), raw);
    }

    #[test]
    fn sixty_four_byte_linux_hostname_fits_a_dns_label() {
        let raw = "x".repeat(64);
        let label = dns_label(&raw);
        assert!(label.len() <= DNS_LABEL_MAX);
        assert_ne!(label, raw);
        assert!(label.starts_with('x'));
    }

    #[test]
    fn long_hostnames_that_share_a_prefix_stay_distinct() {
        let a = format!("{}a", "x".repeat(63));
        let b = format!("{}b", "x".repeat(63));
        assert_eq!(a.len(), 64);
        assert_eq!(b.len(), 64);
        let la = dns_label(&a);
        let lb = dns_label(&b);
        assert_ne!(la, lb);
        assert!(la.len() <= DNS_LABEL_MAX);
        assert!(lb.len() <= DNS_LABEL_MAX);
    }

    #[test]
    fn truncation_does_not_split_utf8() {
        // 62 ASCII bytes + 2-byte 'é' = 64 → drop the accented char, keep a suffix.
        let raw = format!("{}é", "a".repeat(62));
        assert_eq!(raw.len(), 64);
        let label = dns_label(&raw);
        assert!(label.is_char_boundary(label.len()));
        assert!(label.len() <= DNS_LABEL_MAX);
        assert!(label.starts_with("a"));
        assert!(!label.contains('é'));
    }

    #[test]
    fn empty_or_whitespace_falls_back() {
        assert_eq!(dns_label(""), "xfr-server");
        assert_eq!(dns_label("   "), "xfr-server");
    }

    #[cfg(feature = "discovery")]
    #[test]
    fn sanitized_64_byte_name_has_encodable_labels() {
        use mdns_sd::ServiceInfo;

        let hostname = dns_label(&"x".repeat(64));
        let info = ServiceInfo::new(
            "_xfr._tcp.local.",
            &hostname,
            &format!("{}.local.", hostname),
            "",
            5201,
            [("version", "test")].as_ref(),
        )
        .unwrap();

        for label in info.get_hostname().trim_end_matches('.').split('.') {
            assert!(
                label.len() <= DNS_LABEL_MAX,
                "host label {label:?} is {} bytes",
                label.len()
            );
        }
        let instance = info.get_fullname().split('.').next().unwrap();
        assert!(
            instance.len() <= DNS_LABEL_MAX,
            "instance label {instance:?} is {} bytes",
            instance.len()
        );
    }

    /// Live multicast register + browse. Default `cargo test` skips this
    /// (`#[ignore]`): GitHub Actions and most container sandboxes have no
    /// multicast route, and we have not verified localhost mDNS there.
    /// Opt in locally (or later in CI, after a green run) with:
    /// `XFR_MDNS_LIVE=1 cargo test --lib discover::tests -- --ignored --nocapture`
    #[cfg(feature = "discovery")]
    #[test]
    #[ignore = "live mDNS multicast; set XFR_MDNS_LIVE=1"]
    fn sanitized_64_byte_hostname_is_discoverable() {
        use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
        use std::time::{Duration, Instant};

        match std::env::var("XFR_MDNS_LIVE") {
            Ok(v) if matches!(v.trim(), "1" | "true" | "yes") => {}
            _ => {
                eprintln!("skipping live mDNS test: set XFR_MDNS_LIVE=1 and pass --ignored");
                return;
            }
        }

        let raw = format!(
            "xfr194{pid}{pad}",
            pid = std::process::id(),
            pad = "x".repeat(64)
        );
        let hostname = dns_label(&raw[..64]);
        assert!(hostname.len() <= DNS_LABEL_MAX);

        let mdns = ServiceDaemon::new().expect("mdns daemon");
        let service = ServiceInfo::new(
            "_xfr._tcp.local.",
            &hostname,
            &format!("{}.local.", hostname),
            "",
            18941,
            [("version", "test-194")].as_ref(),
        )
        .unwrap()
        .enable_addr_auto();
        mdns.register(service).expect("register");

        let receiver = mdns.browse("_xfr._tcp.local.").expect("browse");
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut found = false;
        while Instant::now() < deadline {
            if let Ok(ServiceEvent::ServiceResolved(info)) =
                receiver.recv_timeout(Duration::from_millis(100))
                && info.get_fullname().starts_with(&hostname)
            {
                found = true;
                break;
            }
        }
        let _ = mdns.shutdown();
        assert!(
            found,
            "sanitized 64-byte hostname {hostname:?} was not discovered"
        );
    }
}
