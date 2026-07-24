use super::*;
use std::net::{TcpListener, TcpStream};

fn connected_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = TcpStream::connect(addr).unwrap();
    let (server, _) = listener.accept().unwrap();
    client.set_nonblocking(true).unwrap();
    (client, server)
}

#[test]
fn plain_connection_delegates_read_and_write() {
    let (client, mut server) = connected_pair();
    let mut connection = RtmpConnection::plain(client);

    connection.write_all(b"hello").unwrap();
    let mut received = [0u8; 5];
    server.read_exact(&mut received).unwrap();
    assert_eq!(&received, b"hello");

    server.write_all(b"world").unwrap();
    // Give the peer a moment to deliver before the non-blocking read.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut buffer = [0u8; 5];
    let mut read_total = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while read_total < 5 {
        assert!(std::time::Instant::now() < deadline, "read timed out");
        match connection.read(&mut buffer[read_total..]) {
            Ok(n) => read_total += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => panic!("unexpected read error: {error}"),
        }
    }
    assert_eq!(&buffer, b"world");
}

#[test]
fn plain_connection_interest_hint_always_returns_the_fallback() {
    let (client, _server) = connected_pair();
    let connection = RtmpConnection::plain(client);

    assert_eq!(connection.interest_hint(Interest::READ), Interest::READ);
    assert_eq!(connection.interest_hint(Interest::WRITE), Interest::WRITE);
    assert_eq!(
        connection.interest_hint(Interest::READ_WRITE),
        Interest::READ_WRITE
    );
}

#[test]
fn plain_connection_raw_fd_matches_the_underlying_socket() {
    use std::os::unix::io::AsRawFd;

    let (client, _server) = connected_pair();
    let expected_fd = client.as_raw_fd();
    let connection = RtmpConnection::plain(client);

    assert_eq!(connection.raw_fd(), expected_fd);
}

#[test]
fn tls_connection_rejects_an_invalid_host_name() {
    let (client, _server) = connected_pair();

    let result = RtmpConnection::tls(client, "");

    assert!(result.is_err());
}

/// Before any I/O happens, a freshly constructed client TLS connection
/// already wants to write (it has a ClientHello queued) — proving
/// `interest_hint` reflects `rustls::ClientConnection`'s real internal
/// state rather than the naive "direction that just blocked" guess plain
/// TCP uses. This is the exact correctness gap the module doc calls out:
/// without it, a leaf that blocks on `write()` while TLS internally needs
/// to `read_tls()` first would only ever be registered for write
/// readiness and could stall forever waiting for a read event that never
/// gets requested.
///
/// A full round-trip handshake against a real TLS server peer is not
/// covered here (this repo has no certificate-generation dependency yet);
/// this test instead proves the interest-derivation logic this slice
/// exists for, using a real (but unhandshaked) `rustls::ClientConnection`.
#[test]
fn tls_connection_wants_write_before_any_io() {
    let (client, _server) = connected_pair();

    let connection = RtmpConnection::tls(client, "example.com").unwrap();

    let hint = connection.interest_hint(Interest::READ);
    assert!(
        hint.writable,
        "a fresh client TLS connection must want to write its ClientHello"
    );
}

#[test]
fn tls_connection_raw_fd_matches_the_underlying_socket() {
    use std::os::unix::io::AsRawFd;

    let (client, _server) = connected_pair();
    let expected_fd = client.as_raw_fd();
    let connection = RtmpConnection::tls(client, "example.com").unwrap();

    assert_eq!(connection.raw_fd(), expected_fd);
}

// ---------------------------------------------------------------------------
// Real handshake round trip: a locally generated self-signed certificate
// (via `rcgen`, a test-only dependency) served by a real
// `rustls::ServerConnection` on a blocking background thread, and the
// client driven non-blocking through `RtmpConnection` exactly the way the
// fabric engine's handshake/negotiation drivers do (WouldBlock -> retry).
// The client trusts the test cert via a verifier that still performs real
// signature verification (`rustls::crypto::verify_tls12/13_signature`) but
// skips chain-to-root validation — appropriate for a locally generated
// test cert with no CA, not a security shortcut in production code (the
// production path always uses `rustls_client_config()`'s real webpki-roots
// trust store; this verifier only exists in this test module).
// ---------------------------------------------------------------------------

use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::crypto::CryptoProvider;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, UnixTime};
use tokio_rustls::rustls::{DigitallySignedStruct, ServerConfig, SignatureScheme};

#[derive(Debug)]
struct AcceptAnyServerCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, tokio_rustls::rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        tokio_rustls::rustls::crypto::verify_tls12_signature(
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
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        tokio_rustls::rustls::crypto::verify_tls13_signature(
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

fn test_client_config() -> Arc<ClientConfig> {
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    Arc::new(
        ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert(provider)))
            .with_no_client_auth(),
    )
}

fn run_tls_server_peer(
    mut stream: TcpStream,
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
) {
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key.into())
        .unwrap();
    let mut conn = tokio_rustls::rustls::ServerConnection::new(Arc::new(server_config)).unwrap();
    let mut tls = tokio_rustls::rustls::Stream::new(&mut conn, &mut stream);
    let mut buf = [0u8; 5];
    tls.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"hello");
    tls.write_all(b"world").unwrap();
    tls.flush().unwrap();
}

#[test]
fn tls_connection_completes_a_real_handshake_and_exchanges_application_data() {
    let cert_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = cert_key.cert.der().clone();
    let key = PrivatePkcs8KeyDer::from(cert_key.signing_key.serialize_der());

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_tls_server_peer(stream, cert, key);
    });

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut connection =
        RtmpConnection::tls_with_config(client_stream, "localhost", test_client_config()).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        assert!(std::time::Instant::now() < deadline, "write timed out");
        match connection.write(b"hello") {
            Ok(5) => break,
            Ok(n) => panic!("unexpected partial write: {n}"),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => panic!("unexpected write error: {error}"),
        }
    }
    connection.flush().unwrap();

    let mut buffer = [0u8; 5];
    let mut read_total = 0;
    while read_total < 5 {
        assert!(std::time::Instant::now() < deadline, "read timed out");
        match connection.read(&mut buffer[read_total..]) {
            Ok(n) => read_total += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => panic!("unexpected read error: {error}"),
        }
    }
    assert_eq!(&buffer, b"world");

    server.join().unwrap();
}
