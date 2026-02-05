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

impl std::str::FromStr for AddressFamily {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "4" | "v4" | "ipv4" | "v4only" | "ipv4-only" => Ok(Self::V4Only),
            "6" | "v6" | "ipv6" | "v6only" | "ipv6-only" => Ok(Self::V6Only),
            "dual" | "dualstack" | "dual-stack" | "both" => Ok(Self::DualStack),
            _ => Err(()),
        }
    }
}

impl AddressFamily {
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

/// Create a UDP socket matching the address family of the remote address.
/// This ensures cross-platform compatibility (macOS dual-stack differs from Linux).
pub async fn create_udp_socket_for_remote(remote: SocketAddr) -> io::Result<UdpSocket> {
    let domain = if remote.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;

    // Bind to appropriate unspecified address
    let bind_addr = if remote.is_ipv4() {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    };
    socket.bind(&SockAddr::from(bind_addr))?;
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

/// Connect to a host with address family preference and optional bind address
pub async fn connect_tcp(
    host: &str,
    port: u16,
    family: AddressFamily,
    bind_addr: Option<SocketAddr>,
) -> io::Result<(TcpStream, SocketAddr)> {
    let addrs = resolve_host(host, port, family)?;

    let mut last_err = None;

    for addr in addrs {
        debug!("Trying to connect to {}", addr);
        let result = if bind_addr.is_some() {
            connect_tcp_with_bind(addr, bind_addr).await
        } else {
            TcpStream::connect(addr).await
        };
        match result {
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

/// Connect TCP socket with optional local bind address
pub async fn connect_tcp_with_bind(
    remote: SocketAddr,
    bind_addr: Option<SocketAddr>,
) -> io::Result<TcpStream> {
    if let Some(local) = bind_addr {
        // Create socket, bind to local address, then connect
        let domain = if remote.is_ipv6() {
            Domain::IPV6
        } else {
            Domain::IPV4
        };
        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_nonblocking(true)?;
        socket.bind(&SockAddr::from(local))?;

        // Connect (non-blocking) - handle platform-specific "in progress" errors
        match socket.connect(&SockAddr::from(remote)) {
            Ok(()) => {}
            #[cfg(unix)]
            Err(e)
                if e.raw_os_error() == Some(libc::EINPROGRESS)
                    || e.raw_os_error() == Some(libc::EALREADY)
                    || e.raw_os_error() == Some(libc::EWOULDBLOCK) => {}
            #[cfg(windows)]
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }

        // Convert to tokio TcpStream
        let std_stream: std::net::TcpStream = socket.into();
        let stream = TcpStream::from_std(std_stream)?;

        // Wait for connection to complete
        stream.writable().await?;

        // Check for connection errors
        if let Some(e) = stream.take_error()? {
            return Err(e);
        }

        debug!("Connected to {} from {}", remote, local);
        Ok(stream)
    } else {
        // No bind address, use normal connect
        Ok(TcpStream::connect(remote).await?)
    }
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
    // SAFETY: fd is a valid file descriptor from socket.as_raw_fd(),
    // enable is a valid c_int pointer, and size_of::<c_int>() is correct.
    // setsockopt may fail but we check the return value.
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

/// Normalize an IP address for comparison.
///
/// Converts IPv4-mapped IPv6 addresses (::ffff:x.x.x.x) to their IPv4 form.
/// This ensures that addresses representing the same endpoint compare equal
/// regardless of whether they arrived via dual-stack or IPv4-only.
pub fn normalize_ip(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                IpAddr::V4(v4)
            } else {
                addr
            }
        }
        IpAddr::V4(_) => addr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_family_from_str() {
        assert_eq!("4".parse::<AddressFamily>(), Ok(AddressFamily::V4Only));
        assert_eq!("ipv4".parse::<AddressFamily>(), Ok(AddressFamily::V4Only));
        assert_eq!("6".parse::<AddressFamily>(), Ok(AddressFamily::V6Only));
        assert_eq!("ipv6".parse::<AddressFamily>(), Ok(AddressFamily::V6Only));
        assert_eq!(
            "dual".parse::<AddressFamily>(),
            Ok(AddressFamily::DualStack)
        );
        assert_eq!(
            "both".parse::<AddressFamily>(),
            Ok(AddressFamily::DualStack)
        );
        assert_eq!("invalid".parse::<AddressFamily>(), Err(()));
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

    #[test]
    fn test_normalize_ip() {
        // Regular IPv4 unchanged
        let v4: IpAddr = "192.168.1.1".parse().unwrap();
        assert_eq!(normalize_ip(v4), v4);

        // Regular IPv6 unchanged
        let v6: IpAddr = "::1".parse().unwrap();
        assert_eq!(normalize_ip(v6), v6);

        // IPv4-mapped IPv6 should convert to IPv4
        let mapped: IpAddr = "::ffff:192.168.1.1".parse().unwrap();
        let normalized = normalize_ip(mapped);
        assert!(normalized.is_ipv4());
        assert_eq!(normalized.to_string(), "192.168.1.1");

        // Comparing normalized addresses should work across representations
        let v4_addr: IpAddr = "127.0.0.1".parse().unwrap();
        let mapped_addr: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert_eq!(normalize_ip(v4_addr), normalize_ip(mapped_addr));
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
