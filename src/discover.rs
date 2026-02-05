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

#[cfg(feature = "discovery")]
mod mdns_impl {
    use super::*;
    use mdns_sd::{ServiceDaemon, ServiceEvent};
    use tracing::{debug, info};

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

        let hostname = hostname::get()
            .map(|h: std::ffi::OsString| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "xfr-server".to_string());

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
