//! TLS support for control channel encryption
//!
//! Provides TLS wrapping for TCP connections using rustls.

use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use rustls_pemfile::{certs, private_key};
use tokio::net::TcpStream;
use tokio_rustls::{
    TlsAcceptor, TlsConnector, client::TlsStream as ClientTlsStream,
    server::TlsStream as ServerTlsStream,
};

/// Load certificates from a PEM file
pub fn load_certs(path: &Path) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let certs: Vec<CertificateDer<'static>> = certs(&mut reader).collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        anyhow::bail!("No certificates found in {}", path.display());
    }
    Ok(certs)
}

/// Load private key from a PEM file
pub fn load_private_key(path: &Path) -> anyhow::Result<PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    private_key(&mut reader)?
        .ok_or_else(|| anyhow::anyhow!("No private key found in {}", path.display()))
}

/// TLS configuration for the server
#[derive(Debug, Clone, Default)]
pub struct TlsServerConfig {
    pub enabled: bool,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
    pub ca_path: Option<String>,
}

impl TlsServerConfig {
    /// Create a TLS acceptor from this configuration
    pub fn create_acceptor(&self) -> anyhow::Result<Option<TlsAcceptor>> {
        if !self.enabled {
            return Ok(None);
        }

        let cert_path = self
            .cert_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("TLS enabled but no certificate path provided"))?;
        let key_path = self
            .key_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("TLS enabled but no key path provided"))?;

        let certs = load_certs(Path::new(cert_path))?;
        let key = load_private_key(Path::new(key_path))?;

        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)?;

        // Enable client certificate verification if CA provided
        if let Some(ca_path) = &self.ca_path {
            let ca_certs = load_certs(Path::new(ca_path))?;
            let mut root_store = RootCertStore::empty();
            for cert in ca_certs {
                root_store.add(cert)?;
            }
            let verifier =
                rustls::server::WebPkiClientVerifier::builder(Arc::new(root_store)).build()?;
            config = ServerConfig::builder()
                .with_client_cert_verifier(verifier)
                .with_single_cert(
                    load_certs(Path::new(cert_path))?,
                    load_private_key(Path::new(key_path))?,
                )?;
        }

        Ok(Some(TlsAcceptor::from(Arc::new(config))))
    }
}

/// TLS configuration for the client
#[derive(Debug, Clone, Default)]
pub struct TlsClientConfig {
    pub enabled: bool,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
    pub insecure: bool,
}

impl TlsClientConfig {
    /// Create a TLS connector from this configuration
    pub fn create_connector(&self) -> anyhow::Result<Option<TlsConnector>> {
        if !self.enabled {
            return Ok(None);
        }

        let config = if self.insecure {
            // Skip certificate verification (for testing only)
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(InsecureServerCertVerifier))
                .with_no_client_auth()
        } else if let (Some(cert_path), Some(key_path)) = (&self.cert_path, &self.key_path) {
            // Mutual TLS with client certificate
            let certs = load_certs(Path::new(cert_path))?;
            let key = load_private_key(Path::new(key_path))?;

            let mut root_store = RootCertStore::empty();
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

            ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_client_auth_cert(certs, key)?
        } else {
            // Standard TLS without client certificate
            let mut root_store = RootCertStore::empty();
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

            ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth()
        };

        Ok(Some(TlsConnector::from(Arc::new(config))))
    }
}

/// Insecure certificate verifier that accepts any certificate (for testing)
#[derive(Debug)]
struct InsecureServerCertVerifier;

impl rustls::client::danger::ServerCertVerifier for InsecureServerCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

/// Wrapper for a connection that may or may not be TLS-encrypted
#[allow(clippy::large_enum_variant)]
pub enum MaybeTlsStream {
    Plain(TcpStream),
    Tls(ClientTlsStream<TcpStream>),
}

/// Wrapper for server-side connection
#[allow(clippy::large_enum_variant)]
pub enum MaybeTlsServerStream {
    Plain(TcpStream),
    Tls(ServerTlsStream<TcpStream>),
}

/// Accept a TLS connection if TLS is enabled
pub async fn accept_tls(
    stream: TcpStream,
    acceptor: &Option<TlsAcceptor>,
) -> anyhow::Result<MaybeTlsServerStream> {
    match acceptor {
        Some(acceptor) => {
            let tls_stream = acceptor.accept(stream).await?;
            Ok(MaybeTlsServerStream::Tls(tls_stream))
        }
        None => Ok(MaybeTlsServerStream::Plain(stream)),
    }
}

/// Connect with TLS if enabled
pub async fn connect_tls(
    stream: TcpStream,
    connector: &Option<TlsConnector>,
    server_name: &str,
) -> anyhow::Result<MaybeTlsStream> {
    match connector {
        Some(connector) => {
            let server_name = rustls::pki_types::ServerName::try_from(server_name.to_string())?;
            let tls_stream = connector.connect(server_name, stream).await?;
            Ok(MaybeTlsStream::Tls(tls_stream))
        }
        None => Ok(MaybeTlsStream::Plain(stream)),
    }
}
