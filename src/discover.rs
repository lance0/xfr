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
            if tokio::time::Instant::now() >= deadline {
                break;
            }

            let event = tokio::time::timeout(Duration::from_millis(100), async {
                receiver.recv()
            })
            .await;

            match event {
                Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                    debug!("Found service: {:?}", info);

                    for addr in info.get_addresses() {
                        let server = DiscoveredServer {
                            ip: *addr,
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
                Ok(Ok(_)) => {
                    // Other events
                }
                Ok(Err(_)) | Err(_) => {
                    // Timeout or error
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
        )?;

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
