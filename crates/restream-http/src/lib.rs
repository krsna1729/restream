//! The one constructor of Restream's `reqwest` clients.
//!
//! reqwest is built with `rustls-no-provider`. A client left to reqwest's own
//! rustls setup takes the rustls process-default crypto provider, panics when
//! none is installed, and trusts the platform store. Every builder here hands
//! reqwest a finished rustls `ClientConfig` instead, so no client depends on
//! process state: ring is its provider, and it trusts webpki-roots plus the
//! roots its caller names. `clippy.toml` bans reqwest's own constructors, and
//! the builder methods that would bring its rustls setup back, everywhere else.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

use std::sync::Arc;

use reqwest::{Client, ClientBuilder};
use tokio_rustls::rustls::crypto::ring;
use tokio_rustls::rustls::pki_types::CertificateDer;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

/// A client builder that trusts webpki-roots.
pub fn client_builder() -> ClientBuilder {
    client_builder_with_tls(tls_config(webpki_roots()))
}

/// A client builder that trusts webpki-roots and every certificate in the PEM
/// bundle `pem`.
pub fn client_builder_with_pem_roots(pem: &[u8]) -> Result<ClientBuilder, String> {
    let mut roots = webpki_roots();
    for certificate in CertificateDer::pem_slice_iter(pem) {
        let certificate =
            certificate.map_err(|error| format!("not a PEM certificate bundle: {error}"))?;
        roots
            .add(certificate)
            .map_err(|error| format!("unusable root certificate: {error}"))?;
    }
    Ok(client_builder_with_tls(tls_config(roots)))
}

/// A client builder that uses `tls` as it is, for a caller that owns its TLS
/// policy (the egress cipher suites and trust roots).
pub fn client_builder_with_tls(tls: ClientConfig) -> ClientBuilder {
    #[expect(
        clippy::disallowed_methods,
        reason = "the single reqwest client constructor (clippy.toml)"
    )]
    Client::builder().tls_backend_preconfigured(tls)
}

/// [`client_builder`] with no other settings. Like `reqwest::Client::new`, it
/// panics only if the connector cannot be built; it never needs a crypto
/// provider installed.
pub fn client() -> Client {
    client_builder()
        .build()
        .expect("an HTTP client with preconfigured TLS builds")
}

fn webpki_roots() -> RootCertStore {
    RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    }
}

#[allow(clippy::disallowed_methods)]
fn tls_config(roots: RootCertStore) -> ClientConfig {
    let mut config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring's default provider supports TLS 1.2 and TLS 1.3")
        .with_root_certificates(roots)
        .with_no_client_auth();
    // What reqwest offers on its own without the `http2` feature; it leaves a
    // preconfigured config's ALPN alone.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
}
