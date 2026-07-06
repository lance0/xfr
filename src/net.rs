//! Network utilities for IPv4/IPv6 socket creation, MPTCP, and address resolution.
//!
//! Provides proper dual-stack socket handling using socket2 for cross-platform
//! compatibility, MPTCP auto-negotiation (Linux 5.6+), and IPv6 flow label support.

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
/// Determine whether to set IPV6_V6ONLY on an IPv6 socket, and set it if
/// needed.
///
/// For **concrete IPv6 addresses** (e.g. `::1`, `fd00:...`): V6ONLY=true —
/// the socket only accepts IPv6 connections, matching the operator's
/// explicit address choice.
///
/// For **unspecified IPv6** (`::`): defer to `AddressFamily` —
/// `DualStack` leaves V6ONLY=false (accepts IPv4-mapped), `V6Only` sets it
/// true.  This is the only case where the platform default would otherwise
/// diverge (Linux=false, macOS/Windows=true).
///
/// For **IPv4**: V6ONLY is irrelevant (no-op).
fn set_v6only_for_addr(socket: &Socket, addr: SocketAddr, family: AddressFamily) -> io::Result<()> {
    if addr.is_ipv4() {
        return Ok(());
    }
    // Concrete IPv6 address → always V6ONLY.
    // Unspecified IPv6 (::) → respect the AddressFamily mode.
    let v6only = if addr.ip().is_unspecified() {
        family == AddressFamily::V6Only
    } else {
        true
    };
    socket.set_only_v6(v6only)?;
    debug!("Set IPV6_V6ONLY={} for {}", v6only, addr);
    Ok(())
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

/// Get the socket protocol for TCP or MPTCP.
/// Returns an error on non-Linux platforms when `mptcp: true`, safe for library use.
fn tcp_protocol(mptcp: bool) -> io::Result<Protocol> {
    if mptcp {
        #[cfg(target_os = "linux")]
        {
            Ok(Protocol::MPTCP)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "MPTCP is only supported on Linux (kernel 5.6+ with CONFIG_MPTCP=y)",
            ))
        }
    } else {
        Ok(Protocol::TCP)
    }
}

/// Validate that MPTCP is available on this platform.
pub fn validate_mptcp() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "MPTCP is only supported on Linux (kernel 5.6+ with CONFIG_MPTCP=y)",
        ))
    }
}

/// Wrap MPTCP socket creation errors with a clear message.
fn wrap_mptcp_error(e: io::Error, mptcp: bool) -> io::Error {
    if mptcp && is_mptcp_unavailable_error(&e) {
        return io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "MPTCP not available (kernel 5.6+ with CONFIG_MPTCP=y required): {}",
                e
            ),
        );
    }
    e
}

/// Returns true if an error means MPTCP is unavailable and TCP fallback is appropriate.
#[cfg(unix)]
fn is_mptcp_unavailable_error(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::Unsupported
        || matches!(
            e.raw_os_error(),
            Some(libc::EPROTONOSUPPORT) | Some(libc::EINVAL) | Some(libc::ENOPROTOOPT)
        )
}

#[cfg(not(unix))]
fn is_mptcp_unavailable_error(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::Unsupported
}

/// Create a TCP listener with proper address family handling
pub async fn create_tcp_listener(
    port: u16,
    family: AddressFamily,
    mptcp: bool,
) -> io::Result<TcpListener> {
    let socket = Socket::new(family.domain(), Type::STREAM, Some(tcp_protocol(mptcp)?))
        .map_err(|e| wrap_mptcp_error(e, mptcp))?;

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

    let proto_label = if mptcp { "MPTCP" } else { "TCP" };
    info!("{} listening on {} ({})", proto_label, addr, family);
    Ok(listener)
}

/// Create a TCP listener bound to a specific address
///
/// Used when `--bind` is specified to bind to a concrete IP address.
/// Tries MPTCP first on Linux, falling back to regular TCP.
///
/// `family` controls IPV6_V6ONLY behavior when binding to `::` (unspecified
/// IPv6): `DualStack` leaves V6ONLY=false (accepts IPv4-mapped), `V6Only`
/// sets it true.  Concrete IPv6 addresses always get V6ONLY=true regardless.
pub async fn create_tcp_listener_on_addr(
    addr: SocketAddr,
    family: AddressFamily,
) -> io::Result<TcpListener> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };

    // Try MPTCP first on Linux
    #[cfg(target_os = "linux")]
    {
        match Socket::new(domain, Type::STREAM, Some(Protocol::MPTCP)) {
            Ok(socket) => {
                socket.set_reuse_address(true)?;
                set_v6only_for_addr(&socket, addr, family)?;
                socket.bind(&SockAddr::from(addr))?;
                socket.listen(128)?;
                socket.set_nonblocking(true)?;
                let std_listener: std::net::TcpListener = socket.into();
                let listener = TcpListener::from_std(std_listener)?;
                info!("MPTCP listening on {}", addr);
                return Ok(listener);
            }
            Err(e) if is_mptcp_unavailable_error(&e) => {
                debug!("MPTCP not available, falling back to TCP: {}", e);
            }
            Err(e) => return Err(e),
        }
    }

    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    set_v6only_for_addr(&socket, addr, family)?;
    socket.bind(&SockAddr::from(addr))?;
    socket.listen(128)?;
    socket.set_nonblocking(true)?;
    let std_listener: std::net::TcpListener = socket.into();
    let listener = TcpListener::from_std(std_listener)?;
    info!("TCP listening on {}", addr);
    Ok(listener)
}

/// Create a TCP listener that tries MPTCP first, falling back to regular TCP silently.
///
/// On Linux, MPTCP listeners accept both MPTCP and regular TCP connections transparently
/// (the kernel handles the fallback). This means a server can always use MPTCP sockets
/// without requiring clients to use MPTCP — regular TCP clients connect normally.
///
/// If the kernel doesn't support MPTCP (missing CONFIG_MPTCP or kernel < 5.6), this
/// falls back to a regular TCP listener with a debug-level log message.
pub async fn create_tcp_listener_auto_mptcp(
    port: u16,
    family: AddressFamily,
) -> io::Result<TcpListener> {
    #[cfg(target_os = "linux")]
    {
        match create_tcp_listener(port, family, true).await {
            Ok(listener) => return Ok(listener),
            Err(e) if is_mptcp_unavailable_error(&e) => {
                debug!("MPTCP not available, falling back to TCP: {}", e);
            }
            Err(e) => return Err(e), // Real error (bind/listen failure), don't mask it
        }
    }
    create_tcp_listener(port, family, false).await
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

/// Re-family a bind address to match the remote address's IP version.
/// Preserves the port but swaps UNSPECIFIED IPv4 ↔ IPv6 as needed.
/// If the bind IP is not unspecified (i.e., user specified a concrete IP), it is left unchanged.
pub fn match_bind_family(bind: SocketAddr, remote: SocketAddr) -> SocketAddr {
    if bind.ip().is_unspecified() && bind.is_ipv4() != remote.is_ipv4() {
        // Swap unspecified address to match remote family
        let ip = if remote.is_ipv6() {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        };
        SocketAddr::new(ip, bind.port())
    } else {
        bind
    }
}

/// Create a UDP socket bound to a specific address.
///
/// `family` controls IPV6_V6ONLY when binding to `::` (unspecified IPv6),
/// matching the TCP listener behavior.  Concrete IPv6 addresses always get
/// V6ONLY=true.
pub async fn create_udp_socket_bound(
    addr: SocketAddr,
    family: AddressFamily,
) -> io::Result<UdpSocket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    set_v6only_for_addr(&socket, addr, family)?;
    socket.bind(&SockAddr::from(addr))?;
    socket.set_nonblocking(true)?;

    let std_socket: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_socket)
}

/// Create a UDP socket matching the address family of the remote address.
///
/// For IPv6 remotes, binds to `::` with V6ONLY=false (dual-stack) so the
/// socket can also reach IPv4-mapped destinations.  This ensures
/// cross-platform compatibility (macOS dual-stack differs from Linux).
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
        // Dual-stack: allow IPv4-mapped connections on this IPv6 socket
        socket.set_only_v6(false)?;
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    };
    socket.bind(&SockAddr::from(bind_addr))?;
    socket.set_nonblocking(true)?;

    let std_socket: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_socket)
}

/// Create the shared UDP socket for the single-port data plane (issue
/// #63) when no QUIC endpoint owns the main port: SO_REUSEADDR +
/// SO_REUSEPORT set before bind so per-stream connected sockets can join
/// the same address later. `v6only` follows the server's address-family
/// mode (`None` for IPv4 sockets).
///
/// Unix-only: SO_REUSEPORT doesn't exist on Windows, and the capability
/// is never advertised there (the self-test below fails closed).
#[cfg(unix)]
pub async fn create_shared_udp_socket(
    addr: SocketAddr,
    v6only: Option<bool>,
) -> io::Result<UdpSocket> {
    let socket = new_reuseport_udp_socket(addr, v6only)?;
    socket.bind(&SockAddr::from(addr))?;
    socket.set_nonblocking(true)?;
    let std_socket: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_socket)
}

/// Create a per-stream data socket for single-port UDP (issue #63):
/// bound to the same local address as the shared socket (SO_REUSEPORT)
/// and `connect()`ed to the client's source address. Once connected, the
/// kernel's UDP lookup scores it above the shared wildcard socket, so
/// line-rate data flows here and never touches the shared-socket tee.
#[cfg(unix)]
pub async fn create_connected_udp_same_port(
    local: SocketAddr,
    v6only: Option<bool>,
    peer: SocketAddr,
) -> io::Result<UdpSocket> {
    let socket = new_reuseport_udp_socket(local, v6only)?;
    socket.bind(&SockAddr::from(local))?;
    socket.set_nonblocking(true)?;
    let std_socket: std::net::UdpSocket = socket.into();
    let udp = UdpSocket::from_std(std_socket)?;
    udp.connect(peer).await?;
    Ok(udp)
}

#[cfg(unix)]
fn new_reuseport_udp_socket(addr: SocketAddr, v6only: Option<bool>) -> io::Result<Socket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    // Plain SO_REUSEPORT everywhere — never SO_REUSEPORT_LB (FreeBSD),
    // whose load-balancing group semantics would defeat the
    // connected-socket-wins routing this feature depends on.
    socket.set_reuse_port(true)?;
    if let Some(v6only) = v6only {
        socket.set_only_v6(v6only)?;
    }
    Ok(socket)
}

#[cfg(not(unix))]
pub async fn create_shared_udp_socket(
    _addr: SocketAddr,
    _v6only: Option<bool>,
) -> io::Result<UdpSocket> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "single-port UDP requires SO_REUSEPORT, which this platform lacks",
    ))
}

#[cfg(not(unix))]
pub async fn create_connected_udp_same_port(
    _local: SocketAddr,
    _v6only: Option<bool>,
    _peer: SocketAddr,
) -> io::Result<UdpSocket> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "single-port UDP requires SO_REUSEPORT, which this platform lacks",
    ))
}

/// Startup self-test gating the `single_port_udp_v1` capability (issue
/// #63): replicate the production socket topology on an ephemeral port —
/// shared WILDCARD-bound socket first (same bind shape and v6only as
/// the real lane), same-port connected socket second — and verify a
/// datagram from the connected peer lands on the connected socket, not
/// the shared one. Dual-stack additionally proves the IPv4-mapped path
/// (`::ffff:x` sources), which is what every plain-IPv4 client hits.
/// Kernel UDP lookup is supposed to score connected sockets above
/// wildcards, but SO_REUSEPORT group semantics vary across kernels and
/// platforms, so we prove it on the running system instead of assuming.
/// Sub-millisecond when it passes; runs once at server start. Any
/// failure means the server simply doesn't advertise the capability and
/// clients fall back to legacy per-stream ports.
pub async fn single_port_udp_self_test(family: AddressFamily) -> bool {
    match single_port_udp_self_test_inner(family).await {
        Ok(()) => true,
        Err(e) => {
            debug!(
                "single-port UDP self-test failed ({}); not advertising {}: {}",
                family,
                crate::protocol::SINGLE_PORT_UDP_CAPABILITY,
                e
            );
            false
        }
    }
}

async fn single_port_udp_self_test_inner(family: AddressFamily) -> io::Result<()> {
    // Bind the shared socket exactly the way production binds the main
    // UDP lane: WILDCARD for the family, same v6only setting. A concrete
    // loopback bind would exercise a different kernel lookup path
    // (exact-address-bound sockets score differently from wildcard-bound
    // ones), and the whole point of this gate is to prove the production
    // topology, not a lookalike.
    let v6only = match family {
        AddressFamily::V4Only => None,
        AddressFamily::V6Only => Some(true),
        AddressFamily::DualStack => Some(false),
    };
    let wildcard = family.bind_addr(0);
    let shared = create_shared_udp_socket(wildcard, v6only).await?;
    let port = shared.local_addr()?.port();
    // Production passes the shared socket's own local address when
    // binding each connected socket; mirror that.
    let local = SocketAddr::new(wildcard.ip(), port);

    let native_loopback: IpAddr = match family {
        AddressFamily::V4Only => IpAddr::V4(Ipv4Addr::LOCALHOST),
        AddressFamily::V6Only | AddressFamily::DualStack => IpAddr::V6(Ipv6Addr::LOCALHOST),
    };
    check_connected_precedence(&shared, local, v6only, native_loopback, port, false).await?;

    // A dual-stack lane also serves IPv4 clients, whose source addresses
    // arrive as ::ffff:x.x.x.x and whose connected sockets are
    // connect()ed to that mapped form. That is the path every ordinary
    // IPv4 client takes, so prove it separately — native-IPv6 routing
    // passing says nothing about mapped-address routing.
    if family == AddressFamily::DualStack {
        check_connected_precedence(
            &shared,
            local,
            v6only,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            true,
        )
        .await?;
    }
    Ok(())
}

/// One self-test round: a peer on `peer_loopback` gets a same-port
/// connected socket (connect target in the mapped form when `map_peer`,
/// mirroring how a dual-stack lane observes IPv4 sources), then we
/// verify (1) the peer's datagram lands on the connected socket and
/// (2) an unrelated source on the same family still reaches the shared
/// socket — guarding against reuseport semantics where the last-bound
/// group member captures all unicast, which would starve QUIC and
/// new-client hellos in production.
async fn check_connected_precedence(
    shared: &UdpSocket,
    local: SocketAddr,
    v6only: Option<bool>,
    peer_loopback: IpAddr,
    port: u16,
    map_peer: bool,
) -> io::Result<()> {
    use std::time::Duration;

    let peer = UdpSocket::bind(SocketAddr::new(peer_loopback, 0)).await?;
    let peer_addr = peer.local_addr()?;
    let connect_to = match (map_peer, peer_addr.ip()) {
        (true, IpAddr::V4(v4)) => {
            SocketAddr::new(IpAddr::V6(v4.to_ipv6_mapped()), peer_addr.port())
        }
        _ => peer_addr,
    };
    let connected = create_connected_udp_same_port(local, v6only, connect_to).await?;
    // The shared socket is wildcard-bound; the peer reaches it via its
    // own family's loopback at the shared port.
    let dest = SocketAddr::new(peer_loopback, port);

    const PROBE: &[u8] = b"xfr-single-port-self-test";
    peer.send_to(PROBE, dest).await?;

    let mut connected_buf = [0u8; 64];
    let mut shared_buf = [0u8; 64];
    tokio::select! {
        result = connected.recv(&mut connected_buf) => {
            let n = result?;
            if &connected_buf[..n] != PROBE {
                return Err(io::Error::other("connected socket received unexpected data"));
            }
        }
        _ = shared.recv_from(&mut shared_buf) => return Err(io::Error::other(
            "kernel delivered the datagram to the shared wildcard socket \
             instead of the connected socket",
        )),
        _ = tokio::time::sleep(Duration::from_millis(500)) => return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "datagram reached neither socket",
        )),
    }

    let stranger = UdpSocket::bind(SocketAddr::new(peer_loopback, 0)).await?;
    stranger.send_to(PROBE, dest).await?;
    tokio::select! {
        result = shared.recv_from(&mut shared_buf) => {
            let (n, _) = result?;
            if &shared_buf[..n] == PROBE {
                Ok(())
            } else {
                Err(io::Error::other("shared socket received unexpected data"))
            }
        }
        _ = connected.recv(&mut connected_buf) => Err(io::Error::other(
            "kernel delivered an unrelated source's datagram to the \
             connected socket",
        )),
        _ = tokio::time::sleep(Duration::from_millis(500)) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "unrelated-source datagram reached neither socket",
        )),
    }
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

    // Wrap literal IPv6 addresses in brackets so `to_socket_addrs` parses
    // "[::1]:5201" correctly instead of treating the colons as port separators.
    let is_ipv6_literal = host_for_lookup
        .parse::<IpAddr>()
        .map(|ip| ip.is_ipv6())
        .unwrap_or(false);
    let addr_str = if is_ipv6_literal {
        format!("[{}]:{}", host_for_lookup, port)
    } else {
        format!("{}:{}", host_for_lookup, port)
    };
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
    mptcp: bool,
) -> io::Result<(TcpStream, SocketAddr)> {
    let addrs = resolve_host(host, port, family)?;

    let mut last_err = None;

    for addr in addrs {
        debug!("Trying to connect to {}", addr);
        // When bind IP is unspecified (0.0.0.0 / ::), match the remote family
        // so dual-stack clients can connect across IPv4/IPv6 targets.
        let local_bind = bind_addr.map(|local| match_bind_family(local, addr));
        let result = connect_tcp_with_bind(addr, local_bind, mptcp).await;
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
    mptcp: bool,
) -> io::Result<TcpStream> {
    if bind_addr.is_some() || mptcp {
        // Use socket2 path for bind address or MPTCP (tokio's TcpStream::connect
        // doesn't support setting the socket protocol)
        let domain = if remote.is_ipv6() {
            Domain::IPV6
        } else {
            Domain::IPV4
        };
        let socket = Socket::new(domain, Type::STREAM, Some(tcp_protocol(mptcp)?))
            .map_err(|e| wrap_mptcp_error(e, mptcp))?;
        socket.set_nonblocking(true)?;

        if let Some(local) = bind_addr {
            if local.port() != 0 {
                socket.set_reuse_address(true)?;
            }
            // Set V6ONLY consistently for IPv6 bind addresses.  For
            // wildcard `::` use dual-stack (V6ONLY=false) so the socket
            // can reach IPv4 remotes too; concrete IPv6 → V6ONLY=true.
            if local.is_ipv6() {
                let v6only = !local.ip().is_unspecified();
                socket.set_only_v6(v6only)?;
                debug!("Set IPV6_V6ONLY={} for bind {}", v6only, local);
            }
            socket.bind(&SockAddr::from(local))?;
        }

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
            Err(e) => return Err(wrap_mptcp_error(e, mptcp)),
        }

        // Convert to tokio TcpStream
        let std_stream: std::net::TcpStream = socket.into();
        let stream = TcpStream::from_std(std_stream)?;

        // Wait for connection to complete
        stream.writable().await?;

        // Check for connection errors
        if let Some(e) = stream.take_error()? {
            return Err(wrap_mptcp_error(e, mptcp));
        }

        if let Some(local) = bind_addr {
            debug!("Connected to {} from {}", remote, local);
        } else {
            debug!("Connected to {} (MPTCP)", remote);
        }
        Ok(stream)
    } else {
        // No bind address and no MPTCP, use normal connect
        Ok(TcpStream::connect(remote).await?)
    }
}

/// Set DSCP/TOS marking on a raw fd.
///
/// For **IPv4** sockets: sets `IP_TOS`.
///
/// For **IPv6** sockets: sets `IPV6_TCLASS`.  On dual-stack (non-V6ONLY)
/// IPv6 sockets, also sets `IP_TOS` so IPv4-mapped traffic gets the
/// requested DSCP value (the kernel applies `IP_TOS` to IPv4 packets and
/// `IPV6_TCLASS` to IPv6 packets independently — setting both is safe).
#[cfg(unix)]
fn set_tos_on_fd(fd: std::os::unix::io::RawFd, tos: u8, ipv6: bool) -> io::Result<()> {
    let tos_val = tos as libc::c_int;

    if ipv6 {
        // IPV6_TCLASS applies to IPv6 traffic
        // SAFETY: fd is a valid file descriptor, tos_val is a valid c_int on the stack.
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::IPPROTO_IPV6,
                libc::IPV6_TCLASS,
                &tos_val as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }

        // Also set IP_TOS for IPv4-mapped traffic on dual-stack sockets.
        // On V6ONLY sockets this is a no-op (no IPv4 traffic), but the
        // setsockopt itself succeeds and is harmless.
        // SAFETY: same fd, same tos_val.
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_TOS,
                &tos_val as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if ret != 0 {
            let err = io::Error::last_os_error();
            if ip_tos_on_ipv6_unsupported(&err) {
                // Some platforms do not expose IP_TOS on IPv6 sockets.
                // Native IPv6 traffic is still covered by IPV6_TCLASS above.
                debug!(
                    "IP_TOS on IPv6 socket unsupported on this platform: {}",
                    err
                );
            } else {
                return Err(err);
            }
        }
    } else {
        // IPv4 socket: IP_TOS only
        // SAFETY: fd is a valid file descriptor, tos_val is a valid c_int on the stack.
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_TOS,
                &tos_val as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

#[cfg(unix)]
fn ip_tos_on_ipv6_unsupported(err: &io::Error) -> bool {
    match err.raw_os_error() {
        Some(libc::ENOPROTOOPT) | Some(libc::EOPNOTSUPP) => true,
        // macOS reports EINVAL for IP_TOS on an IPv6 socket. Keep EINVAL
        // fatal on Linux, where it would indicate an unexpected setsockopt
        // failure rather than a known unsupported dual-stack behavior.
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        Some(libc::EINVAL) => true,
        _ => false,
    }
}

/// Set IP_TOS / IPV6_TCLASS on a TCP stream for DSCP/QoS marking.
#[cfg(unix)]
pub fn set_tos_on_tcp(stream: &TcpStream, tos: u8) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let ipv6 = stream.local_addr().map(|a| a.is_ipv6()).unwrap_or(false);
    set_tos_on_fd(stream.as_raw_fd(), tos, ipv6)
}

#[cfg(not(unix))]
pub fn set_tos_on_tcp(_stream: &TcpStream, _tos: u8) -> io::Result<()> {
    tracing::warn!("--dscp is not supported on this platform");
    Ok(())
}

/// Set IP_TOS / IPV6_TCLASS on a UDP socket for DSCP/QoS marking.
#[cfg(unix)]
pub fn set_tos_on_udp(socket: &UdpSocket, tos: u8) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let ipv6 = socket.local_addr().map(|a| a.is_ipv6()).unwrap_or(false);
    set_tos_on_fd(socket.as_raw_fd(), tos, ipv6)
}

#[cfg(not(unix))]
pub fn set_tos_on_udp(_socket: &UdpSocket, _tos: u8) -> io::Result<()> {
    tracing::warn!("--dscp is not supported on this platform");
    Ok(())
}

/// Set the IP don't-fragment behavior on a UDP socket so oversized
/// MTU-probe packets are dropped (surfacing EMSGSIZE locally, or dying
/// at the constraining hop) instead of being fragmented — fragmentation
/// would hide exactly the path limit `--probe-mtu` measures (issue #64).
///
/// Linux exposes this as `IP_MTU_DISCOVER = IP_PMTUDISC_DO` (and the
/// IPv6 twin); macOS/BSD use the boolean `IP_DONTFRAG`/`IPV6_DONTFRAG`.
#[cfg(target_os = "linux")]
pub fn set_dont_fragment(socket: &UdpSocket, ipv6: bool) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let (level, optname, value) = if ipv6 {
        (
            libc::IPPROTO_IPV6,
            libc::IPV6_MTU_DISCOVER,
            libc::IPV6_PMTUDISC_DO,
        )
    } else {
        (
            libc::IPPROTO_IP,
            libc::IP_MTU_DISCOVER,
            libc::IP_PMTUDISC_DO,
        )
    };
    let value = value as libc::c_int;
    // SAFETY: fd is a valid descriptor, value is a c_int on the stack.
    let ret = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            level,
            optname,
            &value as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
pub fn set_dont_fragment(socket: &UdpSocket, ipv6: bool) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let (level, optname) = if ipv6 {
        (libc::IPPROTO_IPV6, libc::IPV6_DONTFRAG)
    } else {
        (libc::IPPROTO_IP, libc::IP_DONTFRAG)
    };
    let value: libc::c_int = 1;
    // SAFETY: fd is a valid descriptor, value is a c_int on the stack.
    let ret = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            level,
            optname,
            &value as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// Only platforms with a verified DONTFRAG constant get the real call;
// guessing option numbers risks setting an unrelated IP option.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
pub fn set_dont_fragment(_socket: &UdpSocket, _ipv6: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "don't-fragment control is not supported on this platform",
    ))
}

/// Apply `-w`/`--window` to a UDP socket via `SO_SNDBUF`/`SO_RCVBUF`.
///
/// Sets both buffers to the same size for symmetry with the existing TCP
/// path (`tcp::configure_socket_buffers`). On UDP this is more impactful
/// than on TCP — bumping the receiver's buffer is the primary way to
/// avoid kernel tail-drops on high-rate flows where the recv loop can't
/// drain fast enough (issue #70 follow-up).
///
/// `size` is validated up front: it must fit in `c_int` and be positive,
/// otherwise the kernel would reject or wrap the request silently.
///
/// `setsockopt` failures bubble up rather than getting swallowed at debug
/// level: when a user passes `-w 16M` for #70-style troubleshooting, a
/// silent rejection (typically exceeding `net.core.rmem_max` without
/// `CAP_NET_ADMIN`) is exactly the case they need to learn about.
/// Both `SO_SNDBUF` and `SO_RCVBUF` are attempted independently — a
/// failure on the send side does not skip the receive side, since the
/// receive buffer is the part that actually mitigates tail-drops on the
/// receiver. If either or both fail, the returned error names which
/// option(s) failed; the caller is expected to surface it as a warning.
#[cfg(unix)]
pub fn set_udp_buffer_size(socket: &UdpSocket, size: usize) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = socket.as_raw_fd();
    set_udp_buffer_size_with(size, |opt, size_c| set_one_buffer(fd, opt, size_c))
}

/// Validate-and-orchestrate core for [`set_udp_buffer_size`], with the
/// per-option `setsockopt` call factored out as a closure. Production
/// callers go through [`set_udp_buffer_size`] which plugs in the real
/// `set_one_buffer`. Tests pass a fake closure to exercise partial-
/// failure paths (one option succeeds, the other fails) without
/// depending on kernel sysctls.
#[cfg(unix)]
fn set_udp_buffer_size_with<F>(size: usize, mut set: F) -> io::Result<()>
where
    F: FnMut(libc::c_int, libc::c_int) -> Option<io::Error>,
{
    let size_c = validate_udp_buffer_size(size)?;
    // Try both buffers independently. Returning early on the first failure
    // would defeat the #70 mitigation on receiver sockets, where the
    // server's wmem_max ceiling may reject SO_SNDBUF even though the
    // RCVBUF bump is exactly the thing we want to land.
    let snd_err = set(libc::SO_SNDBUF, size_c);
    let rcv_err = set(libc::SO_RCVBUF, size_c);
    combine_udp_buffer_errors(snd_err, rcv_err)
}

#[cfg(unix)]
fn validate_udp_buffer_size(size: usize) -> io::Result<libc::c_int> {
    let size_c = libc::c_int::try_from(size).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "window size {} exceeds platform maximum ({} bytes)",
                size,
                libc::c_int::MAX
            ),
        )
    })?;
    if size_c <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "window size must be greater than zero",
        ));
    }
    Ok(size_c)
}

#[cfg(unix)]
fn combine_udp_buffer_errors(snd: Option<io::Error>, rcv: Option<io::Error>) -> io::Result<()> {
    match (snd, rcv) {
        (None, None) => Ok(()),
        (Some(e), None) => Err(io::Error::new(e.kind(), format!("SO_SNDBUF: {}", e))),
        (None, Some(e)) => Err(io::Error::new(e.kind(), format!("SO_RCVBUF: {}", e))),
        (Some(snd), Some(rcv)) => Err(io::Error::other(format!(
            "SO_SNDBUF: {}; SO_RCVBUF: {}",
            snd, rcv
        ))),
    }
}

#[cfg(unix)]
fn set_one_buffer(
    fd: std::os::unix::io::RawFd,
    optname: libc::c_int,
    size_c: libc::c_int,
) -> Option<io::Error> {
    // SAFETY: fd is a valid descriptor (caller guarantees), size_c is a
    // valid c_int by reference, and the size argument matches its layout.
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            optname,
            &size_c as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret == 0 {
        None
    } else {
        Some(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
pub fn set_udp_buffer_size(_socket: &UdpSocket, _size: usize) -> io::Result<()> {
    tracing::warn!("UDP --window is not supported on this platform");
    Ok(())
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

    #[test]
    fn test_match_bind_family() {
        // IPv4 bind + IPv6 remote → swap to IPv6 unspecified
        let bind: SocketAddr = "0.0.0.0:5300".parse().unwrap();
        let remote: SocketAddr = "[::1]:5201".parse().unwrap();
        let result = match_bind_family(bind, remote);
        assert!(result.is_ipv6());
        assert_eq!(result.port(), 5300);
        assert!(result.ip().is_unspecified());

        // IPv6 bind + IPv4 remote → swap to IPv4 unspecified
        let bind: SocketAddr = "[::]:5300".parse().unwrap();
        let remote: SocketAddr = "192.168.1.1:5201".parse().unwrap();
        let result = match_bind_family(bind, remote);
        assert!(result.is_ipv4());
        assert_eq!(result.port(), 5300);
        assert!(result.ip().is_unspecified());

        // Matching families → unchanged
        let bind: SocketAddr = "0.0.0.0:5300".parse().unwrap();
        let remote: SocketAddr = "192.168.1.1:5201".parse().unwrap();
        assert_eq!(match_bind_family(bind, remote), bind);

        // Concrete (non-unspecified) IP → unchanged even if family mismatches
        let bind: SocketAddr = "10.0.0.1:5300".parse().unwrap();
        let remote: SocketAddr = "[::1]:5201".parse().unwrap();
        assert_eq!(match_bind_family(bind, remote), bind);
    }

    #[test]
    fn test_resolve_ipv6_literal_uses_brackets() {
        let addrs = resolve_host("::1", 5201, AddressFamily::V6Only).unwrap();
        assert!(!addrs.is_empty(), "::1 should resolve");
        assert!(
            addrs.iter().all(|a| a.is_ipv6() && a.port() == 5201),
            "literal IPv6 address must be parsed with [addr]:port syntax"
        );
    }

    #[test]
    fn test_resolve_ipv4_literal_keeps_standard_formatting() {
        let addrs = resolve_host("127.0.0.1", 5201, AddressFamily::V4Only).unwrap();
        assert!(!addrs.is_empty(), "127.0.0.1 should resolve");
        assert!(
            addrs.iter().all(|a| a.is_ipv4() && a.port() == 5201),
            "literal IPv4 address must be parsed as addr:port"
        );
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

    #[tokio::test]
    async fn test_connect_tcp_refamilies_unspecified_bind() {
        let listener = match tokio::net::TcpListener::bind("[::1]:0").await {
            Ok(listener) => listener,
            Err(_) => return, // IPv6 not configured in this environment
        };
        let port = listener.local_addr().unwrap().port();

        let accept_task = tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let bind: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let result = connect_tcp("::1", port, AddressFamily::V6Only, Some(bind), false).await;

        assert!(
            result.is_ok(),
            "connect_tcp should re-family unspecified bind addr for IPv6 target"
        );

        let _ = accept_task.await;
    }

    /// The self-test must pass for real on Linux (CI and dev machines) —
    /// it is what turns single-port UDP on, so a regression here silently
    /// downgrades every client to legacy ephemeral ports.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn single_port_udp_self_test_passes_on_linux() {
        assert!(single_port_udp_self_test(AddressFamily::V4Only).await);
        assert!(single_port_udp_self_test(AddressFamily::DualStack).await);
    }

    /// On every platform the self-test must come back with a verdict
    /// rather than hang or panic — failure just means legacy fallback.
    #[tokio::test]
    async fn single_port_udp_self_test_returns_verdict() {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            single_port_udp_self_test(AddressFamily::V4Only),
        )
        .await
        .expect("self-test must complete promptly");
    }

    #[test]
    fn test_validate_mptcp() {
        #[cfg(target_os = "linux")]
        assert!(validate_mptcp().is_ok());

        #[cfg(not(target_os = "linux"))]
        assert!(validate_mptcp().is_err());
    }

    #[test]
    fn test_is_mptcp_unavailable_error_by_kind() {
        let unsupported = io::Error::new(io::ErrorKind::Unsupported, "mptcp unavailable");
        assert!(is_mptcp_unavailable_error(&unsupported));

        let other = io::Error::new(io::ErrorKind::AddrInUse, "address in use");
        assert!(!is_mptcp_unavailable_error(&other));
    }

    #[cfg(unix)]
    #[test]
    fn test_wrap_mptcp_error_marks_unsupported() {
        let wrapped = wrap_mptcp_error(io::Error::from_raw_os_error(libc::EPROTONOSUPPORT), true);
        assert_eq!(wrapped.kind(), io::ErrorKind::Unsupported);
        assert!(is_mptcp_unavailable_error(&wrapped));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_mptcp_socket_creation() {
        // Attempt to create an MPTCP socket — may fail if kernel lacks support
        let result = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::MPTCP));
        match result {
            Ok(_) => {} // MPTCP available
            Err(e) => {
                // Expected on kernels without CONFIG_MPTCP
                assert!(
                    e.raw_os_error() == Some(libc::EPROTONOSUPPORT)
                        || e.raw_os_error() == Some(libc::EINVAL)
                        || e.raw_os_error() == Some(libc::ENOPROTOOPT),
                    "Unexpected error: {}",
                    e
                );
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn set_udp_buffer_size_rejects_zero_and_overflow() {
        // Validation must reject zero and `usize` values that don't fit in
        // c_int up front, so the kernel never sees a wrapped or negative
        // size. Otherwise setsockopt could silently misapply a huge request
        // as a tiny one (or vice versa) and leave only a debug-level trace
        // of the fallout.
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let err = set_udp_buffer_size(&socket, 0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let oversized = (libc::c_int::MAX as usize).saturating_add(1);
        let err = set_udp_buffer_size(&socket, oversized).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn set_udp_buffer_size_actually_grows_socket_buffers() {
        // Verify that a reasonable buffer-size request lands on the kernel,
        // not just succeeds in our own validation. Linux doubles the
        // requested value when reporting back via getsockopt (per
        // socket(7)) and clamps to `net.core.rmem_max`/`wmem_max`, so we
        // assert the readback exceeds the default rather than equals our
        // request — the request must have *increased* the buffer.
        use std::os::unix::io::AsRawFd;

        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let fd = socket.as_raw_fd();

        let read_buf = |which: libc::c_int| -> libc::c_int {
            let mut value: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            // SAFETY: fd is valid for the lifetime of socket; value/len
            // outlive the call; size matches c_int layout.
            let ret = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    which,
                    &mut value as *mut _ as *mut libc::c_void,
                    &mut len,
                )
            };
            assert_eq!(ret, 0, "getsockopt failed");
            value
        };

        let baseline_rcv = read_buf(libc::SO_RCVBUF);
        let baseline_snd = read_buf(libc::SO_SNDBUF);

        // Request roughly twice the default. Some sandboxed CI hosts ship
        // very low `rmem_max` ceilings (e.g. 256 KB), so an unconditional
        // 4 MB request would be rejected by the kernel and our error-
        // bubbling helper would surface that as a hard failure. Doubling
        // the existing default is well within any reasonable ceiling and
        // still proves the helper actually moves the kernel-side value.
        let target = (baseline_rcv.max(baseline_snd) as usize).saturating_mul(2);
        set_udp_buffer_size(&socket, target).unwrap();

        let new_rcv = read_buf(libc::SO_RCVBUF);
        let new_snd = read_buf(libc::SO_SNDBUF);

        assert!(
            new_rcv > baseline_rcv,
            "SO_RCVBUF didn't grow: {} → {}",
            baseline_rcv,
            new_rcv
        );
        assert!(
            new_snd > baseline_snd,
            "SO_SNDBUF didn't grow: {} → {}",
            baseline_snd,
            new_snd
        );
    }

    #[cfg(unix)]
    #[test]
    fn set_one_buffer_surfaces_setsockopt_failure() {
        // Pin the contract that setsockopt errors propagate up to the
        // caller. We use an invalid fd (-1) so the kernel deterministically
        // returns EBADF regardless of sysctl tunables — an environment-
        // independent way to exercise the error path that would otherwise
        // depend on `net.core.rmem_max` being clamped low enough to reject
        // the request, which isn't true on most CI/dev hosts.
        let err = set_one_buffer(-1, libc::SO_SNDBUF, 4096).expect("expected error from -1 fd");
        assert_eq!(err.raw_os_error(), Some(libc::EBADF));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn set_udp_buffer_size_succeeds_within_kernel_ceilings() {
        // Smoke: a valid request on a real socket succeeds for both
        // buffers and returns Ok(()). 64 KB sits comfortably inside any
        // realistic rmem_max/wmem_max ceiling, including sandboxed CI
        // hosts.
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        set_udp_buffer_size(&socket, 64 * 1024).expect("both setsockopts should succeed");
    }

    #[cfg(unix)]
    #[test]
    fn set_one_buffer_returns_kernel_error_for_each_option() {
        // Confirms the helper runs `setsockopt` once per call and
        // surfaces whatever error the kernel returned, regardless of
        // which option (`SO_SNDBUF` or `SO_RCVBUF`) was passed in.
        // Independence between the two calls in the public API is
        // verified separately via `set_udp_buffer_size_with` and the
        // fake-setter tests below.
        let snd = set_one_buffer(-1, libc::SO_SNDBUF, 4096).expect("snd should fail on -1");
        let rcv = set_one_buffer(-1, libc::SO_RCVBUF, 4096).expect("rcv should fail on -1");
        assert_eq!(snd.raw_os_error(), Some(libc::EBADF));
        assert_eq!(rcv.raw_os_error(), Some(libc::EBADF));
    }

    // The four tests below exercise the public-API contract via the
    // injectable `set_udp_buffer_size_with` core. The fake setter records
    // every (option, size) tuple it was invoked with and returns whatever
    // error pattern the test selected, so we can assert the actual
    // behavior that matters for #70: SNDBUF failing must not skip RCVBUF,
    // and the returned error has to identify which option(s) failed so
    // the caller's `warn!` is actionable.
    #[cfg(unix)]
    #[test]
    fn fake_setter_sndbuf_failure_still_attempts_rcvbuf() {
        // The exact regression #70 cared about: the helper must keep
        // going even after SO_SNDBUF returns an error, so the receive-
        // buffer bump (which is what actually mitigates kernel
        // tail-drops on the receiver) lands.
        let calls: std::rc::Rc<std::cell::RefCell<Vec<libc::c_int>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let calls_for_closure = calls.clone();
        let setter = move |opt: libc::c_int, _sz: libc::c_int| -> Option<io::Error> {
            calls_for_closure.borrow_mut().push(opt);
            if opt == libc::SO_SNDBUF {
                Some(io::Error::from_raw_os_error(libc::ENOBUFS))
            } else {
                None
            }
        };

        let result = set_udp_buffer_size_with(64 * 1024, setter);
        let err = result.expect_err("SNDBUF failure must surface as Err");
        let recorded = calls.borrow();
        assert_eq!(
            *recorded,
            vec![libc::SO_SNDBUF, libc::SO_RCVBUF],
            "RCVBUF must still be attempted after SNDBUF failure",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("SO_SNDBUF"),
            "error must name SO_SNDBUF: {msg}"
        );
        assert!(
            !msg.contains("SO_RCVBUF"),
            "error must NOT name SO_RCVBUF when only SNDBUF failed: {msg}",
        );
    }

    #[cfg(unix)]
    #[test]
    fn fake_setter_rcvbuf_failure_reports_rcvbuf_only() {
        let setter = move |opt: libc::c_int, _sz: libc::c_int| -> Option<io::Error> {
            if opt == libc::SO_RCVBUF {
                Some(io::Error::from_raw_os_error(libc::ENOBUFS))
            } else {
                None
            }
        };

        let err = set_udp_buffer_size_with(64 * 1024, setter)
            .expect_err("RCVBUF failure must surface as Err");
        let msg = err.to_string();
        assert!(
            msg.contains("SO_RCVBUF"),
            "error must name SO_RCVBUF: {msg}"
        );
        assert!(
            !msg.contains("SO_SNDBUF"),
            "error must NOT name SO_SNDBUF when only RCVBUF failed: {msg}",
        );
    }

    #[cfg(unix)]
    #[test]
    fn fake_setter_both_failures_combine_into_one_error() {
        let setter =
            |_opt: libc::c_int, _sz: libc::c_int| Some(io::Error::from_raw_os_error(libc::EPERM));

        let err = set_udp_buffer_size_with(64 * 1024, setter)
            .expect_err("dual failure must surface as Err");
        let msg = err.to_string();
        assert!(
            msg.contains("SO_SNDBUF") && msg.contains("SO_RCVBUF"),
            "combined error must name both options: {msg}",
        );
    }

    #[cfg(unix)]
    #[test]
    fn fake_setter_both_success_returns_ok_and_attempts_both() {
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let calls_for_closure = calls.clone();
        let setter = move |opt: libc::c_int, sz: libc::c_int| -> Option<io::Error> {
            calls_for_closure.borrow_mut().push((opt, sz));
            None
        };

        let result = set_udp_buffer_size_with(64 * 1024, setter);
        assert!(result.is_ok(), "no failure must produce Ok");
        let recorded = calls.borrow();
        assert_eq!(
            recorded.len(),
            2,
            "exactly two setsockopt calls expected, got {}",
            recorded.len()
        );
        assert_eq!(recorded[0].0, libc::SO_SNDBUF);
        assert_eq!(recorded[1].0, libc::SO_RCVBUF);
        assert_eq!(recorded[0].1, 64 * 1024);
        assert_eq!(recorded[1].1, 64 * 1024);
    }

    /// Regression for LAN-169 #29: `create_tcp_listener_on_addr` with
    /// `AddressFamily::DualStack` on `::` (unspecified IPv6) must accept
    /// IPv4 connections — the platform default (Linux=false, macOS=true)
    /// must not leak through.
    #[tokio::test]
    async fn test_dualstack_listener_accepts_ipv4() {
        let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);
        let listener = match create_tcp_listener_on_addr(addr, AddressFamily::DualStack).await {
            Ok(l) => l,
            Err(_) => return, // IPv6 unsupported on this host
        };
        let local = listener.local_addr().unwrap();

        // An IPv4 connect to the dual-stack listener must succeed.
        let v4_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), local.port());
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            TcpStream::connect(v4_addr),
        )
        .await;
        assert!(
            matches!(result, Ok(Ok(_))),
            "IPv4 connect to dual-stack [::] listener should succeed, got {result:?}"
        );
    }

    /// Regression for LAN-169 #29: `create_tcp_listener_on_addr` with
    /// `AddressFamily::V6Only` on `::` must reject IPv4 connections.
    #[tokio::test]
    async fn test_v6only_listener_rejects_ipv4() {
        let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);
        let listener = match create_tcp_listener_on_addr(addr, AddressFamily::V6Only).await {
            Ok(l) => l,
            Err(_) => return, // IPv6 unsupported on this host
        };
        let local = listener.local_addr().unwrap();

        let v4_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), local.port());
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            TcpStream::connect(v4_addr),
        )
        .await;
        assert!(
            !matches!(result, Ok(Ok(_))),
            "IPv4 connect to V6Only [::] listener should fail, got {result:?}"
        );
    }

    /// Regression for LAN-169 #29: `create_udp_socket_bound` with
    /// `AddressFamily::DualStack` on `::` must accept IPv4 packets.
    #[tokio::test]
    async fn test_dualstack_udp_accepts_ipv4() {
        let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);
        let socket = match create_udp_socket_bound(addr, AddressFamily::DualStack).await {
            Ok(s) => s,
            Err(_) => return,
        };
        let local = socket.local_addr().unwrap();

        // Send a packet from an IPv4 socket to the dual-stack listener.
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), local.port());
        sender.send_to(b"ping", target).await.unwrap();

        let mut buf = [0u8; 4];
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(500), socket.recv(&mut buf))
                .await;
        match result {
            Ok(Ok(n)) => assert_eq!(n, 4, "should receive exactly 4 bytes"),
            other => panic!("dual-stack UDP socket should receive IPv4 packets, got {other:?}"),
        }
        assert_eq!(&buf, b"ping");
    }

    /// Regression for LAN-169 #30: `set_tos_on_fd` on an IPv6 socket must set
    /// IPV6_TCLASS and should also set IP_TOS where the platform allows it.
    #[cfg(unix)]
    #[test]
    fn test_dscp_sets_both_on_dualstack_ipv6() {
        use std::os::unix::io::AsRawFd;

        let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        socket.set_only_v6(false).unwrap(); // dual-stack
        let fd = socket.as_raw_fd();

        let tos: u8 = 0x46; // EF DSCP
        set_tos_on_fd(fd, tos, true).unwrap();

        // Read back IPV6_TCLASS to verify it was set.
        let mut val: libc::c_int = 0;
        let mut len: libc::socklen_t = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let ret = unsafe {
            libc::getsockopt(
                fd,
                libc::IPPROTO_IPV6,
                libc::IPV6_TCLASS,
                &mut val as *mut libc::c_int as *mut libc::c_void,
                &mut len,
            )
        };
        assert_eq!(ret, 0, "getsockopt IPV6_TCLASS should succeed");
        assert_eq!(
            val as u8, tos,
            "IPV6_TCLASS should match the requested DSCP"
        );

        // Read back IP_TOS to verify the IPv4-mapped DSCP path when the
        // platform exposes IP_TOS on IPv6 sockets.
        let mut val: libc::c_int = 0;
        let mut len: libc::socklen_t = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let ret = unsafe {
            libc::getsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_TOS,
                &mut val as *mut libc::c_int as *mut libc::c_void,
                &mut len,
            )
        };
        if ret == 0 {
            assert_eq!(
                val as u8, tos,
                "IP_TOS should match the requested DSCP on dual-stack IPv6 socket"
            );
        }
        // If ret != 0, the platform doesn't expose IP_TOS on IPv6 sockets;
        // IPV6_TCLASS still covers native IPv6.
    }
}
