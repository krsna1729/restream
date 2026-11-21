//! What a shared client trusts, against a self-signed `localhost` server.
//!
//! This binary holds one test: it sets `SSL_CERT_FILE`, which only a test
//! alone in its process can do soundly.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::{CryptoProvider, ring};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};

#[test]
fn clients_trust_webpki_roots_and_named_pem_roots_without_a_process_provider() {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let pem = certified.cert.pem();
    let pem_path =
        std::env::temp_dir().join(format!("restream-http-trust-{}.pem", std::process::id()));
    std::fs::write(&pem_path, &pem).unwrap();
    // The platform store (rustls-platform-verifier, reqwest's own default)
    // reads this variable, so a client trusting the platform store would
    // accept the server below; webpki-roots does not.
    // SAFETY: this is the only test in its binary, and it sets the variable
    // before it starts the runtime, so no other thread reads the environment.
    unsafe { std::env::set_var("SSL_CERT_FILE", &pem_path) };

    let server_config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(certified.cert.der().to_vec())],
            PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der()).into(),
        )
        .unwrap();

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(serve(listener, TlsAcceptor::from(Arc::new(server_config))));

            let untrusted = get(restream_http::client_builder(), address).await;
            let error = untrusted.expect_err("a self-signed server is not in webpki-roots");
            assert!(
                format!("{error:?}").contains("UnknownIssuer"),
                "rejected for its issuer: {error:?}"
            );

            let named = restream_http::client_builder_with_pem_roots(pem.as_bytes()).unwrap();
            let status = get(named, address).await.expect("its PEM root is trusted");
            assert_eq!(status, reqwest::StatusCode::OK);
        });

    // reqwest would have panicked building a client without one; none of
    // them read it or installed it.
    assert!(CryptoProvider::get_default().is_none());
    std::fs::remove_file(&pem_path).unwrap();
}

async fn get(
    builder: reqwest::ClientBuilder,
    address: SocketAddr,
) -> reqwest::Result<reqwest::StatusCode> {
    let client = builder.no_proxy().resolve("localhost", address).build()?;
    let response = client
        .get(format!("https://localhost:{}/", address.port()))
        .send()
        .await?;
    Ok(response.status())
}

async fn serve(listener: TcpListener, acceptor: TlsAcceptor) {
    loop {
        let (stream, _) = listener.accept().await.unwrap();
        let Ok(mut stream) = acceptor.accept(stream).await else {
            continue;
        };
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => request.extend_from_slice(&buffer[..read]),
            }
        }
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await;
        let _ = stream.shutdown().await;
    }
}
