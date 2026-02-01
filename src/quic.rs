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

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::{
    ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig, TransportConfig,
    VarInt,
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::watch;
use tracing::{debug, error, info};

use crate::net::AddressFamily;
use crate::stats::StreamStats;

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
pub fn create_server_endpoint(
    addr: SocketAddr,
    cert: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
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

    let endpoint = Endpoint::server(server_config, addr)?;
    info!("QUIC server listening on {}", addr);

    Ok(endpoint)
}

/// Create a QUIC client endpoint
pub fn create_client_endpoint(address_family: AddressFamily) -> anyhow::Result<Endpoint> {
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

    // Bind to appropriate address based on address family
    let bind_addr: SocketAddr = match address_family {
        AddressFamily::V6Only => "[::]:0".parse()?,
        _ => "0.0.0.0:0".parse()?, // IPv4 or dual-stack default to IPv4 bind
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
    cancel: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let buffer = vec![0u8; 128 * 1024]; // 128KB buffer
    let deadline = tokio::time::Instant::now() + duration;

    loop {
        if *cancel.borrow() {
            debug!("QUIC send cancelled");
            break;
        }

        if tokio::time::Instant::now() >= deadline {
            break;
        }

        match send.write(&buffer).await {
            Ok(n) => stats.add_bytes_sent(n as u64),
            Err(e) => {
                error!("QUIC send error: {}", e);
                return Err(e.into());
            }
        }
    }

    send.finish()?;
    Ok(())
}

/// Receive data from a QUIC stream
pub async fn receive_quic_data(
    mut recv: RecvStream,
    stats: Arc<StreamStats>,
    cancel: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut buffer = vec![0u8; 128 * 1024];

    loop {
        if *cancel.borrow() {
            debug!("QUIC receive cancelled");
            break;
        }

        match recv.read(&mut buffer).await? {
            Some(n) => stats.add_bytes_received(n as u64),
            None => break, // Stream finished
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
