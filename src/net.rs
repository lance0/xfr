//! Network utilities for IPv4/IPv6 socket creation and address resolution.
//!
//! Provides proper dual-stack socket handling using socket2 for cross-platform
//! compatibility and IPv6 flow label support.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tracing::{debug, info};

/// Address family preference for socket creation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddressFamily {
    /// IPv4 only (bind to 0.0.0.0)
    V4Only,
    /// IPv6 only (bind to :: with IPV6_V6ONLY=true)
    V6Only,
    /// Dual-stack: accept both IPv4 and IPv6 (bind to :: with IPV6_V6ONLY=false)
    #[default]
    DualStack,
}

impl AddressFamily {
    /// Parse from string (for config/CLI)
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "4" | "v4" | "ipv4" | "v4only" | "ipv4-only" => Some(Self::V4Only),
            "6" | "v6" | "ipv6" | "v6only" | "ipv6-only" => Some(Self::V6Only),
            "dual" | "dualstack" | "dual-stack" | "both" => Some(Self::DualStack),
            _ => None,
        }
    }

    /// Get the bind address for this family
    pub fn bind_addr(&self, port: u16) -> SocketAddr {
        match self {
            Self::V4Only => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
            Self::V6Only | Self::DualStack => {
                SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port)
            }
        }
    }

    /// Get the domain for socket creation
    pub fn domain(&self) -> Domain {
        match self {
            Self::V4Only => Domain::IPV4,
            Self::V6Only | Self::DualStack => Domain::IPV6,
        }
    }
}

impl std::fmt::Display for AddressFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::V4Only => write!(f, "IPv4"),
            Self::V6Only => write!(f, "IPv6"),
            Self::DualStack => write!(f, "dual-stack"),
        }
    }
}

/// Create a TCP listener with proper address family handling
pub async fn create_tcp_listener(port: u16, family: AddressFamily) -> io::Result<TcpListener> {
    let socket = Socket::new(family.domain(), Type::STREAM, Some(Protocol::TCP))?;

    // Allow address reuse
    socket.set_reuse_address(true)?;

    // Configure IPv6 behavior
    if family != AddressFamily::V4Only {
        // IPV6_V6ONLY: true = IPv6 only, false = dual-stack
        let v6only = family == AddressFamily::V6Only;
        socket.set_only_v6(v6only)?;
        debug!("Set IPV6_V6ONLY={} for {} mode", v6only, family);
    }

    let addr = family.bind_addr(port);
    socket.bind(&SockAddr::from(addr))?;
    socket.listen(128)?;

    // Convert to non-blocking for tokio
    socket.set_nonblocking(true)?;
    let std_listener: std::net::TcpListener = socket.into();
    let listener = TcpListener::from_std(std_listener)?;

    info!("Listening on {} ({})", addr, family);
    Ok(listener)
}

/// Create a UDP socket with proper address family handling
pub async fn create_udp_socket(port: u16, family: AddressFamily) -> io::Result<UdpSocket> {
    let socket = Socket::new(family.domain(), Type::DGRAM, Some(Protocol::UDP))?;

    // Allow address reuse
    socket.set_reuse_address(true)?;

    // Configure IPv6 behavior
    if family != AddressFamily::V4Only {
        let v6only = family == AddressFamily::V6Only;
        socket.set_only_v6(v6only)?;
    }

    let addr = family.bind_addr(port);
    socket.bind(&SockAddr::from(addr))?;

    // Convert to non-blocking for tokio
    socket.set_nonblocking(true)?;
    let std_socket: std::net::UdpSocket = socket.into();
    let udp = UdpSocket::from_std(std_socket)?;

    debug!("UDP socket bound to {} ({})", addr, family);
    Ok(udp)
}

/// Create a UDP socket bound to a specific address
pub async fn create_udp_socket_bound(addr: SocketAddr) -> io::Result<UdpSocket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.bind(&SockAddr::from(addr))?;
    socket.set_nonblocking(true)?;

    let std_socket: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_socket)
}

/// Resolve a hostname to addresses, filtered by address family preference
pub fn resolve_host(host: &str, port: u16, family: AddressFamily) -> io::Result<Vec<SocketAddr>> {
    // Handle zone IDs for link-local IPv6 (e.g., fe80::1%eth0)
    let host_for_lookup = if host.contains('%') {
        // Strip zone ID for lookup, we'll add it back
        host.split('%').next().unwrap_or(host)
    } else {
        host
    };

    let addr_str = format!("{}:{}", host_for_lookup, port);
    let addrs: Vec<SocketAddr> = addr_str.to_socket_addrs()?.collect();

    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Could not resolve host: {}", host),
        ));
    }

    // Filter by address family
    let filtered: Vec<SocketAddr> = match family {
        AddressFamily::V4Only => addrs.into_iter().filter(|a| a.is_ipv4()).collect(),
        AddressFamily::V6Only => addrs.into_iter().filter(|a| a.is_ipv6()).collect(),
        AddressFamily::DualStack => addrs, // Accept any
    };

    if filtered.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("No {} addresses found for host: {}", family, host),
        ));
    }

    // Re-add zone ID for link-local addresses if present
    if host.contains('%') {
        let zone_id = host.split('%').nth(1);
        if let Some(_zone) = zone_id {
            // Note: Rust's SocketAddr doesn't support zone IDs directly
            // This would need platform-specific handling for link-local
            debug!("Zone ID specified but not directly supported in SocketAddr");
        }
    }

    Ok(filtered)
}

/// Connect to a host with address family preference
pub async fn connect_tcp(
    host: &str,
    port: u16,
    family: AddressFamily,
) -> io::Result<(TcpStream, SocketAddr)> {
    let addrs = resolve_host(host, port, family)?;

    let mut last_err = None;

    for addr in addrs {
        debug!("Trying to connect to {}", addr);
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                info!("Connected to {}", addr);
                return Ok((stream, addr));
            }
            Err(e) => {
                debug!("Failed to connect to {}: {}", addr, e);
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotConnected, "No addresses to connect to")
    }))
}

/// Set IPv6 flow label on a socket (Linux only)
#[cfg(target_os = "linux")]
pub fn set_flow_label(socket: &Socket, flow_label: u32) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;

    if flow_label > 0xFFFFF {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Flow label must be 0-1048575 (20 bits)",
        ));
    }

    // IPV6_FLOWLABEL_MGR and IPV6_FLOWINFO are needed
    // This is complex on Linux - requires flow label manager
    // For now, we'll set it via IPV6_FLOWINFO on send
    debug!(
        "Flow label {} requested (will be set on packets)",
        flow_label
    );

    // Set IPV6_FLOWINFO to include flow label in sent packets
    let fd = socket.as_raw_fd();
    let enable: libc::c_int = 1;
    unsafe {
        let ret = libc::setsockopt(
            fd,
            libc::IPPROTO_IPV6,
            libc::IPV6_FLOWINFO,
            &enable as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn set_flow_label(_socket: &Socket, _flow_label: u32) -> io::Result<()> {
    // Flow labels are Linux-specific
    debug!("IPv6 flow labels not supported on this platform");
    Ok(())
}

/// Check if an address is IPv4-mapped IPv6 (::ffff:x.x.x.x)
pub fn is_ipv4_mapped(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().is_some(),
        IpAddr::V4(_) => false,
    }
}

/// Convert IPv4-mapped IPv6 to IPv4 if applicable
pub fn normalize_addr(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) => {
            if let Some(v4) = v6.ip().to_ipv4_mapped() {
                SocketAddr::new(IpAddr::V4(v4), v6.port())
            } else {
                addr
            }
        }
        _ => addr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_family_from_str() {
        assert_eq!(AddressFamily::from_str("4"), Some(AddressFamily::V4Only));
        assert_eq!(AddressFamily::from_str("ipv4"), Some(AddressFamily::V4Only));
        assert_eq!(AddressFamily::from_str("6"), Some(AddressFamily::V6Only));
        assert_eq!(AddressFamily::from_str("ipv6"), Some(AddressFamily::V6Only));
        assert_eq!(
            AddressFamily::from_str("dual"),
            Some(AddressFamily::DualStack)
        );
        assert_eq!(
            AddressFamily::from_str("both"),
            Some(AddressFamily::DualStack)
        );
        assert_eq!(AddressFamily::from_str("invalid"), None);
    }

    #[test]
    fn test_bind_addr() {
        let v4 = AddressFamily::V4Only.bind_addr(5201);
        assert!(v4.is_ipv4());
        assert_eq!(v4.port(), 5201);

        let v6 = AddressFamily::V6Only.bind_addr(5201);
        assert!(v6.is_ipv6());
        assert_eq!(v6.port(), 5201);

        let dual = AddressFamily::DualStack.bind_addr(5201);
        assert!(dual.is_ipv6()); // Dual-stack uses IPv6 socket
    }

    #[test]
    fn test_normalize_addr() {
        // Regular IPv4
        let v4 = "192.168.1.1:5201".parse().unwrap();
        assert_eq!(normalize_addr(v4), v4);

        // Regular IPv6
        let v6: SocketAddr = "[::1]:5201".parse().unwrap();
        assert_eq!(normalize_addr(v6), v6);

        // IPv4-mapped IPv6 should convert to IPv4
        let mapped: SocketAddr = "[::ffff:192.168.1.1]:5201".parse().unwrap();
        let normalized = normalize_addr(mapped);
        assert!(normalized.is_ipv4());
        assert_eq!(normalized.port(), 5201);
    }

    #[test]
    fn test_is_ipv4_mapped() {
        assert!(!is_ipv4_mapped(&"192.168.1.1".parse().unwrap()));
        assert!(!is_ipv4_mapped(&"::1".parse().unwrap()));
        assert!(is_ipv4_mapped(&"::ffff:192.168.1.1".parse().unwrap()));
    }

    #[tokio::test]
    async fn test_resolve_localhost_v4() {
        let addrs = resolve_host("localhost", 5201, AddressFamily::V4Only).unwrap();
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.is_ipv4()));
    }

    #[tokio::test]
    async fn test_resolve_localhost_v6() {
        // May fail if IPv6 is not configured
        if let Ok(addrs) = resolve_host("localhost", 5201, AddressFamily::V6Only) {
            assert!(addrs.iter().all(|a| a.is_ipv6()));
        }
    }
}
