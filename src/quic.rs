//! QUIC transport implementation using quinn
//!
//! QUIC provides:
//! - Built-in TLS 1.3 encryption
//! - Multiplexed streams over a single connection
//! - Stream 0 for control, streams 1-N for data
//!
//! # Connection Flow
//!
//! ```text
//! Client                                 Server
//!   │                                      │
//!   │── QUIC handshake (TLS 1.3) ─────────>│
//!   │<─────────────────────────────────────│
//!   │                                      │
//!   │== Stream 0 (bidirectional) =========│  Control channel
//!   │   Hello, TestStart, Interval, etc.  │
//!   │                                      │
//!   │== Stream 1 (unidirectional) ========│  Data stream 1
//!   │== Stream 2 (unidirectional) ========│  Data stream 2
//!   │== ...                               │
//!   │                                      │
//! ```
//!
//! # Security Model
//!
//! QUIC provides transport encryption via TLS 1.3, but xfr uses self-signed
//! certificates by default (server identity is not verified). For authenticated
//! connections, combine with PSK authentication (`--psk`) to prevent MITM attacks.
//!
//! The self-signed approach is chosen because:
//! 1. xfr is typically used on trusted networks (LAN, VPN)
//! 2. PSK authentication provides mutual authentication when needed
//! 3. CA infrastructure is impractical for ephemeral test servers

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use quinn::{
    AsyncUdpSocket, ClientConfig, Connection, Endpoint, EndpointConfig, RecvStream, SendStream,
    ServerConfig, TransportConfig, UdpPoller, VarInt,
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use socket2::{Protocol, Socket, Type};
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info};

use crate::net::AddressFamily;
use crate::stats::StreamStats;
use crate::udp::SharedSocketLane;

/// A single-port UDP hello diverted off the shared server socket:
/// raw datagram bytes plus the client's source address.
pub type XfrHelloDatagram = (Vec<u8>, SocketAddr);

/// Default buffer size for QUIC send/receive operations (128 KB)
const DEFAULT_BUFFER_SIZE: usize = 128 * 1024;
/// Maximum consecutive divert-only batches in `XfrLaneTee::poll_recv`
/// before rescheduling quinn's receive task, preventing starvation under heavy
/// xfr-hello load (LAN-170 #32).
const MAX_DIVERT_ITERATIONS: u32 = 16;

/// Generate a self-signed certificate for QUIC
///
/// xfr uses its own PSK authentication mechanism, so we don't need
/// proper certificate validation. This generates a temporary cert
/// for the QUIC handshake.
pub fn generate_self_signed_cert()
-> anyhow::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert = rcgen::generate_simple_self_signed(vec!["xfr".to_string()])?;
    let key = PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
    let cert_der = CertificateDer::from(cert.cert);
    Ok((vec![cert_der], key))
}

/// Create a QUIC server endpoint
///
/// Uses socket2 to create the UDP socket with proper dual-stack handling.
/// On Windows, `std::net::UdpSocket::bind("[::]:port")` does NOT accept IPv4
/// connections by default — we must explicitly set `IPV6_V6ONLY=false`.
///
/// When `xfr_hello_tx` is `Some` (single-port UDP enabled, issue #63),
/// the socket also joins an SO_REUSEPORT group so per-stream connected
/// data sockets can bind the same port later, and quinn receives a tee
/// wrapper that diverts xfr hello datagrams to the dispatcher while
/// passing QUIC through untouched. The endpoint then disables QUIC-bit
/// greasing — see [`XfrLaneTee`] for why that is load-bearing.
pub fn create_server_endpoint(
    addr: SocketAddr,
    family: AddressFamily,
    cert: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    xfr_hello_tx: Option<mpsc::Sender<XfrHelloDatagram>>,
) -> anyhow::Result<Endpoint> {
    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert, key)?;

    crypto.alpn_protocols = vec![b"xfr".to_vec()];

    let mut transport = TransportConfig::default();
    transport.max_concurrent_bidi_streams(VarInt::from_u32(129)); // control + 128 data
    transport.keep_alive_interval(Some(Duration::from_secs(5)));

    let quic_crypto = QuicServerConfig::try_from(crypto)?;
    let mut server_config = ServerConfig::with_crypto(Arc::new(quic_crypto));
    server_config.transport_config(Arc::new(transport));

    // Create the UDP socket ourselves so we can set IPV6_V6ONLY properly.
    // Quinn's Endpoint::server() uses std::net::UdpSocket::bind() which
    // doesn't configure dual-stack, breaking IPv4 on Windows (#39).
    let socket = Socket::new(family.domain(), Type::DGRAM, Some(Protocol::UDP))?;
    if family != AddressFamily::V4Only {
        let v6only = family == AddressFamily::V6Only;
        socket.set_only_v6(v6only)?;
        debug!("QUIC socket IPV6_V6ONLY={} for {} mode", v6only, family);
    }
    if xfr_hello_tx.is_some() {
        // Both options must be set BEFORE bind for later same-port
        // connected sockets to be admitted to the group.
        socket.set_reuse_address(true)?;
        #[cfg(unix)]
        socket.set_reuse_port(true)?;
    }
    socket.bind(&socket2::SockAddr::from(addr))?;
    socket.set_nonblocking(true)?;
    let std_socket: std::net::UdpSocket = socket.into();

    let runtime =
        quinn::default_runtime().ok_or_else(|| anyhow::anyhow!("no async runtime found"))?;
    let mut endpoint_config = EndpointConfig::default();
    // Wrap the runtime's own quinn-udp-backed socket object so the
    // GSO/GRO/ECN metadata paths are preserved — the tee only inspects
    // and reroutes, it never reimplements IO.
    let inner = runtime.wrap_udp_socket(std_socket)?;
    let socket: Arc<dyn AsyncUdpSocket> = match xfr_hello_tx {
        Some(hello_tx) => {
            // The shared-socket classifier routes short-header QUIC by its
            // fixed bit (0x40). Quinn greases that bit by default (RFC
            // 9287); greasing is permission-based, so a server that never
            // advertises the grease transport parameter stops compliant
            // peers from clearing the bit toward us — which is what makes
            // the classifier sound.
            endpoint_config.grease_quic_bit(false);
            Arc::new(XfrLaneTee { inner, hello_tx })
        }
        None => inner,
    };
    let endpoint =
        Endpoint::new_with_abstract_socket(endpoint_config, Some(server_config), socket, runtime)?;
    info!("QUIC server listening on {} ({})", addr, family);

    Ok(endpoint)
}

/// Tee on the shared server UDP socket (issue #63): inbound datagrams
/// matching the xfr single-port hello lane divert to the dispatcher
/// channel; everything else flows through to quinn untouched. Delegates
/// every `AsyncUdpSocket` operation to the real quinn-udp-backed socket,
/// so send-side GSO/ECN and recv-side GRO behavior are unchanged.
///
/// This is a low-rate hello/straggler lane ONLY. Per-stream data rides
/// connected sockets that the kernel scores above this wildcard, so
/// after the handshake the tee never sees test traffic.
#[derive(Debug)]
struct XfrLaneTee {
    inner: Arc<dyn AsyncUdpSocket>,
    hello_tx: mpsc::Sender<XfrHelloDatagram>,
}

impl AsyncUdpSocket for XfrLaneTee {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        self.inner.clone().create_io_poller()
    }

    fn try_send(&self, transmit: &quinn::udp::Transmit) -> io::Result<()> {
        self.inner.try_send(transmit)
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [io::IoSliceMut<'_>],
        meta: &mut [quinn::udp::RecvMeta],
    ) -> Poll<io::Result<usize>> {
        // Bound consecutive divert-only batches to prevent starving quinn
        // under heavy xfr-hello load. After this many iterations with zero
        // QUIC packets kept, yield this poll and ask the executor to
        // reschedule us.
        let mut divert_iterations: u32 = 0;

        loop {
            let n = match self.inner.poll_recv(cx, bufs, meta) {
                Poll::Ready(Ok(n)) => n,
                other => return other,
            };
            // Divert xfr-lane entries and compact the QUIC ones down so
            // quinn sees a contiguous prefix of buffers it owns.
            let mut kept = 0;
            for i in 0..n {
                let len = meta[i].len.min(bufs[i].len());
                match crate::udp::classify_shared_datagram(&bufs[i][..len]) {
                    SharedSocketLane::Quic => {
                        if kept != i {
                            let (front, back) = bufs.split_at_mut(i);
                            if front[kept].len() < len {
                                // Can't happen with quinn's equal-size recv
                                // buffers; drop rather than panic in poll.
                                debug!("shared-socket tee: buffer too small to compact");
                                continue;
                            }
                            front[kept][..len].copy_from_slice(&back[0][..len]);
                            meta[kept] = meta[i];
                        }
                        kept += 1;
                    }
                    SharedSocketLane::XfrHello => {
                        // Low-rate lane with client-side retry: dropping on
                        // a full channel is safe, blocking quinn's recv
                        // path is not.
                        let _ = self
                            .hello_tx
                            .try_send((bufs[i][..len].to_vec(), meta[i].addr));
                    }
                    SharedSocketLane::XfrStray => {
                        debug!(
                            "shared-socket tee: dropping {}-byte non-QUIC datagram from {}",
                            meta[i].len, meta[i].addr
                        );
                    }
                }
            }
            if kept > 0 || n == 0 {
                return Poll::Ready(Ok(kept));
            }
            // Every datagram in the batch was diverted; poll the inner
            // socket again instead of handing quinn an empty result.
            divert_iterations += 1;
            if divert_iterations >= MAX_DIVERT_ITERATIONS {
                debug!(
                    "shared-socket tee: {} consecutive divert-only batches, yielding receive task",
                    divert_iterations
                );
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        self.inner.max_transmit_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}

/// Create a QUIC client endpoint with optional local bind address
pub fn create_client_endpoint(
    remote_addr: SocketAddr,
    local_bind: Option<SocketAddr>,
) -> anyhow::Result<Endpoint> {
    // Client accepts any certificate (xfr uses its own auth mechanism)
    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    // Must match server ALPN
    crypto.alpn_protocols = vec![b"xfr".to_vec()];

    let mut transport = TransportConfig::default();
    transport.max_concurrent_bidi_streams(VarInt::from_u32(129));

    let quic_crypto = QuicClientConfig::try_from(crypto)?;
    let mut client_config = ClientConfig::new(Arc::new(quic_crypto));
    client_config.transport_config(Arc::new(transport));

    // Use provided bind address, or match the remote address family
    let bind_addr: SocketAddr = match local_bind {
        Some(addr) => crate::net::match_bind_family(addr, remote_addr),
        None => {
            if remote_addr.is_ipv6() {
                "[::]:0".parse().unwrap()
            } else {
                "0.0.0.0:0".parse().unwrap()
            }
        }
    };

    let mut endpoint = Endpoint::client(bind_addr)?;
    endpoint.set_default_client_config(client_config);

    Ok(endpoint)
}

/// Skip certificate verification (xfr uses PSK auth instead)
#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

/// Send data over a QUIC stream
pub async fn send_quic_data(
    mut send: SendStream,
    stats: Arc<StreamStats>,
    duration: Duration,
    mut cancel: watch::Receiver<bool>,
    mut pause: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let buffer = vec![0u8; DEFAULT_BUFFER_SIZE];
    let mut deadline = tokio::time::Instant::now() + duration;
    let is_infinite = duration == Duration::ZERO;
    let mut paused_total = Duration::ZERO;

    loop {
        if *cancel.borrow() {
            debug!("QUIC send cancelled");
            break;
        }

        if crate::pause::is_paused(&pause) {
            let (cancelled, paused) =
                crate::pause::wait_while_paused_timed(&mut pause, &mut cancel).await;
            if cancelled {
                break;
            }
            // Extend the deadline by the time spent paused (LAN-230)
            paused_total += paused;
            deadline += paused;
            continue;
        }

        // Duration::ZERO means infinite - only check deadline if finite
        if !is_infinite && tokio::time::Instant::now() >= deadline {
            break;
        }

        // Race the write against cancel, deadline, and pause so a blocked
        // write can be interrupted (LAN-230).
        tokio::select! {
            biased;
            res = cancel.changed() => {
                if res.is_err() || *cancel.borrow() {
                    debug!("QUIC send cancelled during write");
                    break;
                }
            }
            _ = pause.changed(), if crate::pause::channel_is_open(&pause) => {
                // Pause toggled during write — loop back to handle it
                // and extend the deadline (LAN-230).
                continue;
            }
            _ = tokio::time::sleep_until(deadline), if !is_infinite => {
                debug!("QUIC send deadline reached during write");
                break;
            }
            result = send.write(&buffer) => {
                match result {
                    Ok(n) => stats.add_bytes_sent(n as u64),
                    Err(e) => {
                        error!("QUIC send error: {}", e);
                        return Err(e.into());
                    }
                }
            }
        }
    }

    send.finish()?;
    Ok(())
}

/// Decision returned by [`handle_cancel_change`] — extracted from
/// `receive_quic_data` so the cancel-sender-drop logic is unit-testable
/// without a live QUIC stream.
enum CancelAction {
    /// Keep receiving
    Continue,
    /// Stop — cancelled or sender dropped
    Stop,
}

/// Handle a `cancel.changed()` result the way `receive_quic_data` does.
/// `Err` (sender dropped) → `Stop` to avoid busy-loop (LAN-170 #33).
/// `Ok` with `true` → `Stop` (cancelled). `Ok` with `false` → `Continue`.
fn handle_cancel_change(
    res: Result<(), watch::error::RecvError>,
    cancel: &watch::Receiver<bool>,
) -> CancelAction {
    if res.is_err() {
        debug!("QUIC receive: cancel sender dropped, stopping");
        return CancelAction::Stop;
    }
    if *cancel.borrow() {
        debug!("QUIC receive cancelled");
        return CancelAction::Stop;
    }
    CancelAction::Continue
}

/// Receive data from a QUIC stream
pub async fn receive_quic_data(
    mut recv: RecvStream,
    stats: Arc<StreamStats>,
    mut cancel: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut buffer = vec![0u8; DEFAULT_BUFFER_SIZE];

    loop {
        tokio::select! {
            result = recv.read(&mut buffer) => {
                match result? {
                    Some(n) => stats.add_bytes_received(n as u64),
                    None => break, // Stream finished
                }
            }
            res = cancel.changed() => {
                if matches!(handle_cancel_change(res, &cancel), CancelAction::Stop) {
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Connect to a QUIC server
pub async fn connect(endpoint: &Endpoint, addr: SocketAddr) -> anyhow::Result<Connection> {
    let connection = endpoint.connect(addr, "xfr")?.await?;
    debug!("QUIC connected to {}", addr);
    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::watch;

    /// Regression for LAN-170 #33: `handle_cancel_change` must return
    /// `Stop` when the cancel sender drops (RecvError), preventing the
    /// busy-loop that the old `_ = cancel.changed()` pattern caused.
    #[test]
    fn test_handle_cancel_change_sender_dropped() {
        let (_tx, rx) = watch::channel(false);
        drop(_tx);
        // Derive a real RecvError: has_changed() returns Err when closed.
        let res = rx.has_changed().map(|_| ());
        assert!(res.is_err(), "has_changed() should Err when sender dropped");
        let (_tx2, rx2) = watch::channel(false);
        let action = handle_cancel_change(res, &rx2);
        assert!(
            matches!(action, CancelAction::Stop),
            "sender drop must Stop"
        );
    }

    /// Regression for LAN-170 #33: `handle_cancel_change` must return
    /// `Stop` when cancel is true.
    #[test]
    fn test_handle_cancel_change_cancel_true() {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        cancel_tx.send(true).unwrap();
        let action = handle_cancel_change(Ok(()), &cancel_rx);
        assert!(
            matches!(action, CancelAction::Stop),
            "cancel=true must Stop"
        );
    }

    /// `handle_cancel_change` must return `Continue` when cancel is false.
    #[test]
    fn test_handle_cancel_change_cancel_false() {
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let action = handle_cancel_change(Ok(()), &cancel_rx);
        assert!(
            matches!(action, CancelAction::Continue),
            "cancel=false must Continue"
        );
    }

    /// Regression for LAN-170 #32: the module-level
    /// `MAX_DIVERT_ITERATIONS` must be reasonable.  Testing the actual
    /// `poll_recv` would require a mock `AsyncUdpSocket`; here we at
    /// least verify the real constant the production code uses.
    #[test]
    fn test_divert_bound_is_reasonable() {
        let bound = MAX_DIVERT_ITERATIONS;
        assert!(bound > 0, "bound must be > 0");
        assert!(bound <= 64, "bound too high, starvation risk");
    }
}
