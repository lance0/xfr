//! LAN discovery via mDNS
//!
//! Finds xfr servers on the local network using mDNS service discovery.

use std::net::IpAddr;
use std::time::Duration;

/// One server instance found on the LAN.
#[derive(Debug, Clone)]
pub struct DiscoveredServer {
    /// Every address the instance answered with, preferred first: IPv4, then
    /// global or ULA IPv6, then link-local, then loopback.
    pub addresses: Vec<DiscoveredAddress>,
    pub port: u16,
    pub hostname: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DiscoveredAddress {
    pub ip: IpAddr,
    /// IPv6 zone (interface) for a link-local `ip`, as `xfr <ip>%<zone>` takes it.
    pub zone: Option<String>,
}

/// Turn one instance's answers, each link-local one paired with the zone it
/// was received on, into its usable addresses, preferred first.
///
/// mdns-sd 0.21 publishes every link-local address of a host on every
/// interface that has one, because they all share fe80::/64. An interface that
/// carried a single link-local address carried the sender's own address on
/// that link. When it carried several, the right one cannot be told apart, so
/// none is kept rather than printing addresses that will not connect.
#[cfg(any(feature = "discovery", test))]
fn usable_addresses(answers: &[DiscoveredAddress]) -> Vec<DiscoveredAddress> {
    let link_local =
        |a: &DiscoveredAddress| matches!(a.ip, IpAddr::V6(v6) if v6.is_unicast_link_local());
    let mut out: Vec<DiscoveredAddress> = answers
        .iter()
        .filter(|a| {
            !link_local(a)
                || a.zone.is_some()
                    && answers
                        .iter()
                        .filter(|b| link_local(b) && b.zone == a.zone && b.ip != a.ip)
                        .count()
                        == 0
        })
        .cloned()
        .collect();
    out.sort_by_key(|a| (preference(a.ip), a.clone()));
    out.dedup();
    out
}

#[cfg(any(feature = "discovery", test))]
fn preference(ip: IpAddr) -> u8 {
    match ip {
        ip if ip.is_loopback() => 3,
        IpAddr::V4(v4) if v4.is_link_local() => 2,
        IpAddr::V4(_) => 0,
        IpAddr::V6(v6) if v6.is_unicast_link_local() => 2,
        IpAddr::V6(_) => 1,
    }
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

    /// The interface a link-local IPv6 answer arrived on: without it the
    /// printed address cannot be connected to. Names need `if_nametoindex`,
    /// which xfr only uses on unix, so other platforms get the index.
    fn link_local_zone(addr: &mdns_sd::ScopedIp) -> Option<String> {
        match addr {
            mdns_sd::ScopedIp::V6(v6)
                if v6.addr().is_unicast_link_local() && v6.scope_id().index != 0 =>
            {
                let id = v6.scope_id();
                Some(if cfg!(unix) && !id.name.is_empty() {
                    id.name.clone()
                } else {
                    id.index.to_string()
                })
            }
            _ => None,
        }
    }

    pub async fn discover(timeout: Duration) -> anyhow::Result<Vec<DiscoveredServer>> {
        let mdns = ServiceDaemon::new()?;
        let receiver = mdns.browse(SERVICE_TYPE)?;

        // One entry per host and port: servers on one host share an instance
        // name, so a name can resolve to several ports. Answers accumulate
        // across resolutions so the link-local filter sees every address an
        // interface carried.
        let mut found: Vec<(Vec<DiscoveredAddress>, DiscoveredServer)> = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;

        debug!("Searching for xfr servers...");

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }

            // Use recv_async with timeout to keep the async executor unblocked.
            // Cap at 100ms to allow periodic deadline checks.
            let wait_time = remaining.min(Duration::from_millis(100));
            let event = tokio::time::timeout(wait_time, receiver.recv_async()).await;

            match event {
                Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                    debug!("Service resolved: {:?}", info);

                    let server = DiscoveredServer {
                        addresses: Vec::new(),
                        port: info.get_port(),
                        hostname: Some(info.get_hostname().to_string()),
                        version: info
                            .get_properties()
                            .get("version")
                            .map(|v| v.val_str().to_string()),
                    };
                    let answers = info.get_addresses().iter().map(|addr| DiscoveredAddress {
                        ip: addr.to_ip_addr(),
                        zone: link_local_zone(addr),
                    });
                    match found
                        .iter_mut()
                        .find(|(_, s)| s.hostname == server.hostname && s.port == server.port)
                    {
                        Some((seen, latest)) => {
                            seen.extend(answers);
                            *latest = server;
                        }
                        None => found.push((answers.collect(), server)),
                    }
                }
                Ok(Ok(ServiceEvent::ServiceFound(service_type, name))) => {
                    debug!("Service found: {} {}", service_type, name);
                }
                Ok(Ok(ServiceEvent::SearchStarted(_))) => {
                    debug!("mDNS search started");
                }
                Ok(Ok(event)) => {
                    debug!("mDNS event: {:?}", event);
                }
                Ok(Err(_)) => {
                    // Channel disconnected
                    break;
                }
                Err(_) => {
                    // Timeout elapsed - continue to check deadline
                }
            }
        }

        if let Err(e) = mdns.shutdown() {
            debug!("mDNS shutdown error: {e}");
        }
        let mut servers: Vec<DiscoveredServer> = found
            .into_iter()
            .filter_map(|(answers, mut server)| {
                server.addresses = usable_addresses(&answers);
                debug!("Found server: {}", server);
                (!server.addresses.is_empty()).then_some(server)
            })
            .collect();
        servers.sort_by(|a, b| (&a.hostname, a.port).cmp(&(&b.hostname, b.port)));
        Ok(servers)
    }

    pub fn register_server(port: u16) -> anyhow::Result<ServiceDaemon> {
        use mdns_sd::ServiceInfo;

        let mdns = ServiceDaemon::new()?;

        let raw_hostname = hostname::get()
            .map(|h: std::ffi::OsString| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "xfr-server".to_string());
        let hostname = dns_label(&raw_hostname);
        if raw_hostname.trim().len() > DNS_LABEL_MAX {
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

impl std::fmt::Display for DiscoveredAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Bracket IPv6 so the printed ip:port pastes back as `xfr <target>`.
        match (self.ip, &self.zone) {
            (IpAddr::V6(ip), Some(zone)) => write!(f, "[{}%{}]", ip, zone),
            (IpAddr::V6(ip), None) => write!(f, "[{}]", ip),
            (ip, _) => write!(f, "{}", ip),
        }
    }
}

/// The preferred address as `host:port`, then hostname and version.
impl std::fmt::Display for DiscoveredServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(addr) = self.addresses.first() {
            write!(f, "{}", addr)?;
        }
        write!(f, ":{}", self.port)?;
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
    use super::{DNS_LABEL_MAX, DiscoveredAddress, DiscoveredServer, dns_label, usable_addresses};

    fn addr(ip: &str, zone: Option<&str>) -> DiscoveredAddress {
        DiscoveredAddress {
            ip: ip.parse().unwrap(),
            zone: zone.map(str::to_string),
        }
    }

    #[test]
    fn display_keeps_link_local_zone() {
        let server = DiscoveredServer {
            addresses: vec![addr("fe80::2", Some("eth0"))],
            port: 5201,
            hostname: None,
            version: None,
        };
        assert_eq!(server.to_string(), "[fe80::2%eth0]:5201");
    }

    #[test]
    fn display_shows_preferred_address_with_hostname_and_version() {
        let server = DiscoveredServer {
            addresses: usable_addresses(&[
                addr("fe80::2", Some("eth0")),
                addr("2001:db8::2", None),
                addr("192.168.1.20", None),
            ]),
            port: 5201,
            hostname: Some("box.local.".to_string()),
            version: Some("0.13.0".to_string()),
        };
        assert_eq!(
            server.to_string(),
            "192.168.1.20:5201 (box.local.) xfr/0.13.0"
        );
    }

    #[test]
    fn addresses_are_ordered_ipv4_global_v6_link_local_loopback() {
        let got = usable_addresses(&[
            addr("127.0.0.1", None),
            addr("fe80::2", Some("eth0")),
            addr("fd00::2", None),
            addr("::1", None),
            addr("2001:db8::2", None),
            addr("10.0.0.2", None),
            addr("192.168.1.20", None),
        ]);
        let got: Vec<String> = got.iter().map(ToString::to_string).collect();
        assert_eq!(
            got,
            [
                "10.0.0.2",
                "192.168.1.20",
                "[2001:db8::2]",
                "[fd00::2]",
                "[fe80::2%eth0]",
                "127.0.0.1",
                "[::1]",
            ]
        );
    }

    /// mdns-sd publishes every link-local address of the sender on every
    /// interface. Only an interface that carried exactly one link-local
    /// address says which one belongs to it.
    #[test]
    fn link_local_keeps_only_unambiguous_zones() {
        let got = usable_addresses(&[
            // eth0 carried both of the sender's link-local addresses.
            addr("fe80::a", Some("eth0")),
            addr("fe80::b", Some("eth0")),
            // wlan0 carried one, twice (two resolutions of the same answer).
            addr("fe80::c", Some("wlan0")),
            addr("fe80::c", Some("wlan0")),
            // No receiving interface known.
            addr("fe80::d", None),
            addr("192.168.1.20", None),
        ]);
        assert_eq!(
            got,
            [addr("192.168.1.20", None), addr("fe80::c", Some("wlan0"))]
        );
    }

    #[test]
    fn every_link_local_on_every_interface_yields_no_link_local() {
        // The repro: a host with N link-local addresses seen on N interfaces
        // printed N x N rows, almost all with the wrong zone.
        let zones = ["eth0", "docker0", "veth1"];
        let ips = ["fe80::1", "fe80::2", "fe80::3"];
        let answers: Vec<_> = zones
            .iter()
            .flat_map(|z| ips.iter().map(move |ip| addr(ip, Some(z))))
            .chain([addr("192.168.1.20", None), addr("192.168.1.20", None)])
            .collect();
        assert_eq!(usable_addresses(&answers), [addr("192.168.1.20", None)]);
    }

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
        // 57 ASCII bytes + four 2-byte 'é' = 65. The 58-byte cut lands inside
        // the second 'é', so the sanitizer has to walk back to 57; slicing at
        // 58 directly would panic.
        let raw = format!("{}{}", "a".repeat(57), "é".repeat(4));
        assert_eq!(raw.len(), 65);
        let label = dns_label(&raw);
        assert!(label.len() <= DNS_LABEL_MAX);
        assert!(label.starts_with(&"a".repeat(57)));
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
