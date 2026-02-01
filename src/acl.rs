//! IP-based Access Control Lists (ACL)
//!
//! Provides allowlist/denylist functionality for IP addresses and subnets.

use std::net::IpAddr;
use std::path::Path;

use ipnetwork::IpNetwork;

/// Access control list for IP filtering
#[derive(Debug, Clone, Default)]
pub struct Acl {
    /// Allowed networks (if non-empty, only these are allowed)
    allow: Vec<IpNetwork>,
    /// Denied networks (checked first)
    deny: Vec<IpNetwork>,
}

impl Acl {
    /// Create a new empty ACL
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an ACL from allow and deny lists
    pub fn from_rules(allow: Vec<String>, deny: Vec<String>) -> anyhow::Result<Self> {
        let allow = allow
            .into_iter()
            .map(|s| s.parse::<IpNetwork>())
            .collect::<Result<Vec<_>, _>>()?;
        let deny = deny
            .into_iter()
            .map(|s| s.parse::<IpNetwork>())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { allow, deny })
    }

    /// Load ACL rules from a file
    ///
    /// File format:
    /// ```text
    /// # Comment
    /// allow 192.168.0.0/16
    /// allow 10.0.0.0/8
    /// deny 0.0.0.0/0
    /// ```
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut allow = Vec::new();
        let mut deny = Vec::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() != 2 {
                anyhow::bail!("Invalid ACL line: {}", line);
            }

            let network: IpNetwork = parts[1].parse()?;
            match parts[0].to_lowercase().as_str() {
                "allow" => allow.push(network),
                "deny" => deny.push(network),
                _ => anyhow::bail!("Unknown ACL directive: {}", parts[0]),
            }
        }

        Ok(Self { allow, deny })
    }

    /// Add an allow rule
    pub fn allow(&mut self, network: IpNetwork) {
        self.allow.push(network);
    }

    /// Add a deny rule
    pub fn deny(&mut self, network: IpNetwork) {
        self.deny.push(network);
    }

    /// Check if an IP address is allowed
    ///
    /// Rules:
    /// 1. If IP matches any deny rule, reject
    /// 2. If allow list is empty, accept (no allowlist configured)
    /// 3. If IP matches any allow rule, accept
    /// 4. Otherwise reject (default deny when allowlist exists)
    pub fn is_allowed(&self, ip: IpAddr) -> bool {
        // Check deny list first
        for network in &self.deny {
            if network.contains(ip) {
                return false;
            }
        }

        // If no allow rules, allow by default
        if self.allow.is_empty() {
            return true;
        }

        // Check allow list
        for network in &self.allow {
            if network.contains(ip) {
                return true;
            }
        }

        // Default deny when allow list exists
        false
    }

    /// Check if any rules are configured
    pub fn is_configured(&self) -> bool {
        !self.allow.is_empty() || !self.deny.is_empty()
    }

    /// Get the rule that matched (for logging)
    pub fn matched_rule(&self, ip: IpAddr) -> Option<String> {
        for network in &self.deny {
            if network.contains(ip) {
                return Some(format!("deny {}", network));
            }
        }
        for network in &self.allow {
            if network.contains(ip) {
                return Some(format!("allow {}", network));
            }
        }
        None
    }
}

/// ACL configuration from CLI/config
#[derive(Debug, Clone, Default)]
pub struct AclConfig {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub file: Option<String>,
}

impl AclConfig {
    /// Build an ACL from this configuration
    pub fn build(&self) -> anyhow::Result<Acl> {
        let mut acl = if let Some(file) = &self.file {
            Acl::from_file(Path::new(file))?
        } else {
            Acl::new()
        };

        // Add CLI rules on top of file rules
        for allow in &self.allow {
            acl.allow(allow.parse()?);
        }
        for deny in &self.deny {
            acl.deny(deny.parse()?);
        }

        Ok(acl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn test_empty_acl_allows_all() {
        let acl = Acl::new();
        assert!(acl.is_allowed(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(acl.is_allowed(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn test_deny_rule() {
        let mut acl = Acl::new();
        acl.deny("192.168.0.0/16".parse().unwrap());

        assert!(!acl.is_allowed(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(acl.is_allowed(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    }

    #[test]
    fn test_allow_rule_creates_default_deny() {
        let mut acl = Acl::new();
        acl.allow("192.168.0.0/16".parse().unwrap());

        assert!(acl.is_allowed(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(!acl.is_allowed(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    }

    #[test]
    fn test_deny_takes_precedence() {
        let mut acl = Acl::new();
        acl.allow("192.168.0.0/16".parse().unwrap());
        acl.deny("192.168.1.0/24".parse().unwrap());

        assert!(acl.is_allowed(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 1))));
        assert!(!acl.is_allowed(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
    }

    #[test]
    fn test_ipv6() {
        let mut acl = Acl::new();
        acl.allow("::1/128".parse().unwrap());

        assert!(acl.is_allowed(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!acl.is_allowed(IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1))));
    }
}
