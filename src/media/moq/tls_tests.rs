use std::time::Duration;

use compio::quic::{ClientConfig, Endpoint, ServerConfig};

use restream_moq::tls::*;

const ALPN: &str = "moq-lite-06";

/// A self-signed certificate for `localhost`: (certificate PEM, key PEM).
fn self_signed() -> (Vec<u8>, Vec<u8>) {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    (
        certified.cert.pem().into_bytes(),
        certified.signing_key.serialize_pem().into_bytes(),
    )
}

/// Run one QUIC handshake over loopback; the ALPN the client negotiated, or
/// `None` if the handshake failed.
fn handshake(client: ClientConfig, server: ServerConfig) -> Option<Vec<u8>> {
    let runtime = compio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let server_endpoint = Endpoint::server("127.0.0.1:0", server).await.unwrap();
        let address = server_endpoint.local_addr().unwrap();
        let client_endpoint = Endpoint::client("127.0.0.1:0").await.unwrap();
        let accept = async {
            let incoming = server_endpoint.wait_incoming().await?;
            incoming.await.ok()
        };
        let dial = async {
            let connecting = client_endpoint
                .connect(address, "localhost", Some(client))
                .ok()?;
            connecting.await.ok()
        };
        let (_accepted, dialed) = compio::time::timeout(
            Duration::from_secs(10),
            futures_util::future::join(accept, dial),
        )
        .await
        .expect("the handshake settles");
        dialed.and_then(|mut connection| connection.handshake_data().ok()?.protocol)
    })
}

#[test]
fn a_pinned_ca_completes_the_handshake_with_the_profile_alpn() {
    let (cert, key) = self_signed();
    let server = server_config(&[ALPN], &cert, &key).unwrap();
    let client = client_config(&[ALPN], Some(cert.as_slice()), false).unwrap();
    assert_eq!(handshake(client, server), Some(ALPN.as_bytes().to_vec()));
}

#[test]
fn an_untrusted_certificate_fails_without_the_pin() {
    let (cert, key) = self_signed();
    let server = server_config(&[ALPN], &cert, &key).unwrap();
    let client = client_config(&[ALPN], None, false).unwrap();
    assert_eq!(handshake(client, server), None);
}

#[test]
fn insecure_mode_accepts_a_self_signed_relay() {
    let (cert, key) = self_signed();
    let server = server_config(&[ALPN], &cert, &key).unwrap();
    let client = client_config(&[ALPN], None, true).unwrap();
    assert_eq!(handshake(client, server), Some(ALPN.as_bytes().to_vec()));
}

#[test]
fn a_different_alpn_is_refused() {
    let (cert, key) = self_signed();
    let server = server_config(&[ALPN], &cert, &key).unwrap();
    let client = client_config(&["moqt-16"], Some(cert.as_slice()), false).unwrap();
    assert_eq!(handshake(client, server), None);
}

#[test]
fn unusable_pem_is_an_error() {
    let (cert, key) = self_signed();
    assert!(server_config(&[ALPN], b"junk", &key).is_err());
    assert!(server_config(&[ALPN], &cert, b"").is_err());
    assert!(server_config(&[ALPN], &key, &key).is_err());
    assert!(client_config(&[ALPN], Some(b"junk".as_slice()), false).is_err());
    assert!(client_config(&[ALPN], Some(key.as_slice()), false).is_err());
    // The checked-in RTMPS harness pair also serves MoQ.
    let fixture = |name: &str| {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("test/fixtures/tls")
                .join(name),
        )
        .unwrap()
    };
    assert!(
        server_config(
            &[ALPN],
            &fixture("mediamtx-rtmps-cert.pem"),
            &fixture("mediamtx-rtmps-key.pem")
        )
        .is_ok()
    );
}

#[test]
fn the_pinned_hash_is_the_leaf_of_the_chain() {
    let (leaf, _) = self_signed();
    let (issuer, _) = self_signed();
    let chain = [leaf.as_slice(), issuer.as_slice()].concat();
    let hash = leaf_sha256_hex(&chain).expect("a certificate");
    assert_eq!(hash.len(), 64);
    assert!(hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(leaf_sha256_hex(&leaf).as_deref(), Some(hash.as_str()));
    assert_ne!(leaf_sha256_hex(&issuer).as_deref(), Some(hash.as_str()));
    assert_eq!(leaf_sha256_hex(b"not a certificate"), None);
}
