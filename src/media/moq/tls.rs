//! TLS 1.3 (rustls, ring) configurations for the MoQ QUIC endpoints. The
//! caller picks the ALPN list (e.g. [`super::transport::SERVER_ALPNS`]).

use std::sync::Arc;

use compio::quic::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use compio::quic::{ClientConfig, ServerConfig, TransportConfig, VarInt};
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::crypto::{self, CryptoProvider};
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use tokio_rustls::rustls::{
    self, DigitallySignedStruct, RootCertStore, SignatureScheme, version::TLS13,
};

/// Concurrent uni streams a peer may open towards an ingest endpoint: one
/// per object in flight, so this bounds the parser state per connection.
pub(crate) const MAX_INCOMING_UNI_STREAMS: u32 = 64;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(crypto::ring::default_provider())
}

/// A client configuration trusting the bundled web roots (as RTMPS egress
/// does) plus `extra_ca_pem`, or, with `insecure_skip_verify`, any server
/// certificate whose handshake signature is valid (for self-signed relays
/// in development; the certificate chain and name are not checked).
pub(crate) fn client_config(
    alpn: &[&str],
    extra_ca_pem: Option<&[u8]>,
    insecure_skip_verify: bool,
) -> Result<ClientConfig, String> {
    let provider = provider();
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&TLS13])
        .map_err(|error| format!("MoQ TLS client: {error}"))?;
    let mut config = if insecure_skip_verify {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SignatureOnlyVerifier(provider)))
            .with_no_client_auth()
    } else {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Some(pem) = extra_ca_pem {
            let certs = certificates(pem, "MoQ TLS CA")?;
            let (added, _ignored) = roots.add_parsable_certificates(certs);
            if added == 0 {
                return Err("MoQ TLS CA: no usable certificate".to_string());
            }
        }
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    config.alpn_protocols = alpn_protocols(alpn);
    let crypto =
        QuicClientConfig::try_from(config).map_err(|error| format!("MoQ TLS client: {error}"))?;
    Ok(ClientConfig::new(Arc::new(crypto)))
}

/// A server configuration from a PEM certificate chain and private key,
/// accepting at most [`MAX_INCOMING_UNI_STREAMS`] concurrent uni streams
/// per connection.
pub(crate) fn server_config(
    alpn: &[&str],
    cert_chain_pem: &[u8],
    key_pem: &[u8],
) -> Result<ServerConfig, String> {
    let certs = certificates(cert_chain_pem, "MoQ TLS certificate")?;
    let key =
        PrivateKeyDer::from_pem_slice(key_pem).map_err(|error| format!("MoQ TLS key: {error}"))?;
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&TLS13])
        .map_err(|error| format!("MoQ TLS server: {error}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|error| format!("MoQ TLS server: {error}"))?;
    config.alpn_protocols = alpn_protocols(alpn);
    let crypto =
        QuicServerConfig::try_from(config).map_err(|error| format!("MoQ TLS server: {error}"))?;
    let mut transport = TransportConfig::default();
    transport.max_concurrent_uni_streams(VarInt::from_u32(MAX_INCOMING_UNI_STREAMS));
    let mut server = ServerConfig::with_crypto(Arc::new(crypto));
    server.transport_config(Arc::new(transport));
    Ok(server)
}

fn alpn_protocols(alpn: &[&str]) -> Vec<Vec<u8>> {
    alpn.iter()
        .map(|protocol| protocol.as_bytes().to_vec())
        .collect()
}

fn certificates(pem: &[u8], what: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let certs = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{what}: {error}"))?;
    if certs.is_empty() {
        return Err(format!("{what}: no certificate in PEM"));
    }
    Ok(certs)
}

/// Hex SHA-256 of the first (leaf) certificate in a PEM chain: what a
/// browser pins in WebTransport `serverCertificateHashes`. `None` when the
/// PEM holds no readable certificate.
pub(crate) fn leaf_sha256_hex(pem: &[u8]) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let leaf = CertificateDer::pem_slice_iter(pem).next()?.ok()?;
    let digest = Sha256::digest(leaf.as_ref());
    Some(
        digest
            .iter()
            .fold(String::with_capacity(64), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            }),
    )
}

/// Accepts any certificate chain but still checks the handshake signature
/// against the presented certificate.
#[derive(Debug)]
struct SignatureOnlyVerifier(Arc<CryptoProvider>);

impl ServerCertVerifier for SignatureOnlyVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
#[path = "tls_tests.rs"]
mod tests;
