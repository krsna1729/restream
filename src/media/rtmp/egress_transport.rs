use crate::domain::output_spec::OutputUrlScheme;
use percent_encoding::percent_decode_str;
use reqwest::Url;
use std::sync::Arc;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

pub(crate) struct RtmpUrlParts {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) app: String,
    pub(crate) stream_key: String,
    pub(crate) tls: bool,
}

pub(crate) fn rustls_client_config() -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// Same trust store as [`rustls_client_config`] plus any CA certificates
/// found in `extra_trust_roots_pem_path`, for private-CA RTMPS
/// destinations (and, incidentally, for testing against a locally
/// generated cert). Reads and parses the PEM file each call — this is only
/// invoked once per shard-group spawn, not on any packet-level hot path.
pub(crate) fn rustls_client_config_with_extra_roots(
    extra_trust_roots_pem_path: &str,
) -> Result<Arc<ClientConfig>, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let certs = extra_trust_roots_from_pem_file(extra_trust_roots_pem_path)?;
    let (added, _ignored) = roots.add_parsable_certificates(certs);
    if added == 0 {
        return Err(format!(
            "RTMPS extra trust roots file {extra_trust_roots_pem_path} contained no usable certificates"
        ));
    }

    Ok(Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

fn extra_trust_roots_from_pem_file(
    path: &str,
) -> Result<Vec<tokio_rustls::rustls::pki_types::CertificateDer<'static>>, String> {
    let certs: Vec<_> = tokio_rustls::rustls::pki_types::CertificateDer::pem_file_iter(path)
        .map_err(|error| format!("failed to read RTMPS extra trust roots {path}: {error}"))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("failed to parse RTMPS extra trust roots {path}: {error}"))?;
    if certs.is_empty() {
        return Err(format!(
            "RTMPS extra trust roots file {path} contained no certificates"
        ));
    }
    Ok(certs)
}

/// Resolves the trust store to use for RTMPS connections: the default
/// webpki-roots-only config when `extra_trust_roots_pem_path` is `None`
/// (the overwhelmingly common case, unchanged cost), or that plus the
/// configured extra CA certificates otherwise.
pub(crate) fn resolve_rtmps_client_config(
    extra_trust_roots_pem_path: Option<&str>,
) -> Result<Arc<ClientConfig>, String> {
    match extra_trust_roots_pem_path {
        Some(path) => rustls_client_config_with_extra_roots(path),
        None => Ok(rustls_client_config()),
    }
}

// Standard RTMP URL parser helper
pub(crate) fn parse_rtmp_url(url: &str) -> Option<RtmpUrlParts> {
    let tls = match OutputUrlScheme::from_url(url) {
        OutputUrlScheme::Rtmp => false,
        OutputUrlScheme::Rtmps => true,
        _ => return None,
    };
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?.trim_matches(['[', ']']).to_string();
    let port = parsed.port().unwrap_or(1935);
    let mut path_segments = parsed.path_segments()?;
    let app = path_segments.next()?;
    let stream_key = path_segments.collect::<Vec<_>>().join("/");
    if app.is_empty() || stream_key.is_empty() {
        return None;
    }
    // Path segments are percent-encoded as parsed; decode them so an
    // app/stream key containing URL-reserved characters (e.g. a stream key
    // with a literal '/' encoded as %2F) reaches the destination RTMP
    // server as the operator intended, not still escaped.
    let app = percent_decode_str(app).decode_utf8_lossy().into_owned();
    let stream_key = percent_decode_str(&stream_key)
        .decode_utf8_lossy()
        .into_owned();

    Some(RtmpUrlParts {
        host,
        port,
        app,
        stream_key,
        tls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "restream-rtmps-trust-roots-{name}-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, file_name: &str, contents: &[u8]) -> std::path::PathBuf {
            let path = self.0.join(file_name);
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn self_signed_cert_pem() -> Vec<u8> {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        cert.cert.pem().into_bytes()
    }

    #[test]
    fn resolve_rtmps_client_config_succeeds_with_no_override_configured() {
        // No filesystem access is attempted for the `None` case; this just
        // proves the default path still builds a usable config.
        assert!(resolve_rtmps_client_config(None).is_ok());
    }

    #[test]
    fn extra_trust_roots_from_pem_file_parses_the_configured_certificate() {
        let dir = ScratchDir::new("adds-root");
        let pem_path = dir.write("extra-root.pem", &self_signed_cert_pem());

        let certs = extra_trust_roots_from_pem_file(pem_path.to_str().unwrap()).unwrap();

        assert_eq!(certs.len(), 1);
    }

    #[test]
    fn rustls_client_config_with_extra_roots_succeeds_for_a_valid_certificate() {
        let dir = ScratchDir::new("builds-config");
        let pem_path = dir.write("extra-root.pem", &self_signed_cert_pem());

        assert!(rustls_client_config_with_extra_roots(pem_path.to_str().unwrap()).is_ok());
    }

    #[test]
    fn rustls_client_config_with_extra_roots_rejects_a_missing_file() {
        let error = rustls_client_config_with_extra_roots("/nonexistent/rtmps-trust-roots.pem")
            .unwrap_err();
        assert!(
            error.contains("failed to read"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rustls_client_config_with_extra_roots_rejects_a_file_with_no_certificates() {
        let dir = ScratchDir::new("no-certs");
        let path = dir.write("not-a-cert.pem", b"this is not PEM data\n");

        let error = rustls_client_config_with_extra_roots(path.to_str().unwrap()).unwrap_err();
        assert!(
            error.contains("no certificates"),
            "unexpected error: {error}"
        );
    }
}
