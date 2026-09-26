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
fn plain_connection_delegates_vectored_write() {
    let (client, mut server) = connected_pair();
    let mut connection = RtmpConnection::plain(client);
    let buffers = [
        std::io::IoSlice::new(b"hello"),
        std::io::IoSlice::new(b" "),
        std::io::IoSlice::new(b"world"),
    ];

    assert_eq!(connection.write_vectored(&buffers).unwrap(), 11);
    let mut received = [0u8; 11];
    server.read_exact(&mut received).unwrap();
    assert_eq!(&received, b"hello world");
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

#[test]
fn plain_connection_never_reports_a_rustls_buffer_estimate() {
    let (client, _server) = connected_pair();
    let connection = RtmpConnection::plain(client);

    assert_eq!(connection.rustls_pending_bytes_estimate(), 0);
}

/// Mirrors `tls_connection_wants_write_before_any_io`: a fresh client TLS
/// connection wants to write its queued ClientHello, so the conservative
/// estimate must be non-zero — proving the leaf-limit accounting can see
/// rustls-internal buffering exists at all, not that it counts it exactly
/// (rustls exposes no occupancy getter; see the method's own doc comment).
#[test]
fn tls_connection_reports_a_nonzero_rustls_buffer_estimate_before_any_io() {
    let (client, _server) = connected_pair();
    let connection = RtmpConnection::tls(client, "example.com").unwrap();

    assert_eq!(connection.rustls_pending_bytes_estimate(), 64 * 1024);
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

fn test_client_config_tls12() -> Arc<ClientConfig> {
    let mut provider = tokio_rustls::rustls::crypto::ring::default_provider();
    provider.cipher_suites.retain(|suite| {
        suite.suite() == tokio_rustls::rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256
    });
    let provider = Arc::new(provider);
    Arc::new(
        ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&tokio_rustls::rustls::version::TLS12])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert(provider)))
            .with_no_client_auth(),
    )
}

fn test_client_config_tls13_chacha() -> Arc<ClientConfig> {
    let mut provider = tokio_rustls::rustls::crypto::ring::default_provider();
    provider.cipher_suites.retain(|suite| {
        suite.suite() == tokio_rustls::rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
    });
    let provider = Arc::new(provider);
    Arc::new(
        ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&tokio_rustls::rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert(provider)))
            .with_no_client_auth(),
    )
}

fn test_client_config_tls13_aes256_gcm() -> Arc<ClientConfig> {
    let mut provider = tokio_rustls::rustls::crypto::ring::default_provider();
    provider.cipher_suites.retain(|suite| {
        suite.suite() == tokio_rustls::rustls::CipherSuite::TLS13_AES_256_GCM_SHA384
    });
    let provider = Arc::new(provider);
    Arc::new(
        ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&tokio_rustls::rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert(provider)))
            .with_no_client_auth(),
    )
}

fn assert_ktls_application_data_round_trip(config: Arc<ClientConfig>) {
    let cert_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = cert_key.cert.der().clone();
    let key = PrivatePkcs8KeyDer::from(cert_key.signing_key.serialize_der());

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_tls_server_peer(stream, cert, key)
    });

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut connection =
        RtmpConnection::tls_with_config(client_stream, "localhost", config).unwrap();

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
    assert!(connection.is_ktls(), "TLS connection did not enter kTLS");
    connection.flush().unwrap();
    server
        .join()
        .expect("TLS peer thread panicked")
        .expect("TLS peer could not read client application data");

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
    assert_eq!(
        connection.read(&mut [0u8; 1]).unwrap(),
        0,
        "kTLS close_notify must be a clean EOF"
    );
}

#[test]
fn tls12_connection_hands_off_and_exchanges_application_data() {
    if !super::rtmp_ktls::supports(
        tokio_rustls::rustls::ProtocolVersion::TLSv1_2,
        tokio_rustls::rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    ) {
        return;
    }
    assert_ktls_application_data_round_trip(test_client_config_tls12());
}

#[test]
fn tls13_aes256_connection_hands_off_and_exchanges_application_data() {
    if !super::rtmp_ktls::supports(
        tokio_rustls::rustls::ProtocolVersion::TLSv1_3,
        tokio_rustls::rustls::CipherSuite::TLS13_AES_256_GCM_SHA384,
    ) {
        return;
    }
    assert_ktls_application_data_round_trip(test_client_config_tls13_aes256_gcm());
}

#[test]
fn tls_connection_flushes_pending_write_after_handshake_completes() {
    let cert_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = cert_key.cert.der().clone();
    let key = PrivatePkcs8KeyDer::from(cert_key.signing_key.serialize_der());

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        run_tls_server_peer(stream, cert, key)
    });

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut connection =
        RtmpConnection::tls_with_config(client_stream, "localhost", test_client_config_tls12())
            .unwrap();
    connection.ktls_state = KtlsState::NotRequested;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "TLS handshake timed out"
        );
        let handshaking = match &connection.state {
            RtmpConnectionState::Tls(Some(stream)) => stream.conn.is_handshaking(),
            _ => panic!("TLS connection left the userspace state unexpectedly"),
        };
        if !handshaking {
            break;
        }
        match connection.advance_tls_handshake() {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("unexpected TLS handshake error: {error}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    let RtmpConnectionState::Tls(Some(stream)) = &mut connection.state else {
        panic!("TLS connection left the userspace state unexpectedly");
    };
    stream.conn.writer().write_all(b"hello").unwrap();
    assert!(!stream.conn.is_handshaking());
    assert!(stream.conn.wants_write());

    let error = connection.advance_tls_handshake().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    server
        .join()
        .expect("TLS peer thread panicked")
        .expect("pending rustls data was not flushed to the peer");
}

#[test]
fn ktls_read_yields_after_a_bounded_number_of_ticket_records() {
    let (stream, _server) = connected_pair();
    let mut connection = KtlsConnection {
        stream: CompioTcpStream::from_std(stream),
        version: tokio_rustls::rustls::ProtocolVersion::TLSv1_3,
        handshake_buffer: Vec::new(),
        pending_alert_level: None,
        peer_closed: false,
    };
    // A minimal NewSessionTicket with a one-byte ticket. The reader ignores
    // the ticket contents after the kTLS handoff, but still validates framing.
    const TICKET: [u8; 18] = [4, 0, 0, 14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xaa, 0, 0];
    let mut records_read = 0;
    let mut buffer = [0; 32];
    let error = connection
        .read_with(&mut buffer, |buffer| {
            records_read += 1;
            buffer[..TICKET.len()].copy_from_slice(&TICKET);
            Ok((TICKET.len(), rtmp_ktls::RECORD_TYPE_HANDSHAKE))
        })
        .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(
        error.to_string(),
        "kTLS control-record work budget exhausted"
    );
    assert_eq!(records_read, KtlsConnection::MAX_CONTROL_RECORDS_PER_READ);

    let count = connection
        .read_with(&mut buffer, |buffer| {
            buffer[..5].copy_from_slice(b"world");
            Ok((5, rtmp_ktls::RECORD_TYPE_DATA))
        })
        .unwrap();
    assert_eq!(&buffer[..count], b"world");
}

fn run_tls_server_peer(
    mut stream: TcpStream,
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
) -> std::io::Result<()> {
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key.into())
        .unwrap();
    let mut conn = tokio_rustls::rustls::ServerConnection::new(Arc::new(server_config)).unwrap();
    let mut tls = tokio_rustls::rustls::Stream::new(&mut conn, &mut stream);
    let mut buf = [0u8; 5];
    tls.read_exact(&mut buf)?;
    assert_eq!(&buf, b"hello");
    tls.write_all(b"world")?;
    tls.flush()?;
    tls.conn.send_close_notify();
    tls.flush()
}

fn run_tls_handshake_only(
    mut stream: TcpStream,
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
) {
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key.into())
        .unwrap();
    let mut conn = tokio_rustls::rustls::ServerConnection::new(Arc::new(server_config)).unwrap();
    while conn.is_handshaking() {
        conn.complete_io(&mut stream).unwrap();
    }
}

#[test]
fn unsupported_tls_suite_fails_without_userspace_fallback() {
    let cert_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = cert_key.cert.der().clone();
    let key = PrivatePkcs8KeyDer::from(cert_key.signing_key.serialize_der());

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_tls_handshake_only(stream, cert, key);
    });

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut connection = RtmpConnection::tls_with_config(
        client_stream,
        "localhost",
        test_client_config_tls13_chacha(),
    )
    .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let error = loop {
        assert!(
            std::time::Instant::now() < deadline,
            "TLS handshake timed out"
        );
        match connection.write(b"hello") {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => break error,
            Ok(n) => panic!("unsupported TLS suite accepted application bytes: {n}"),
        }
    };
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    assert!(matches!(
        connection.ktls_state,
        super::KtlsState::Unsupported
    ));
    assert!(matches!(
        &connection.state,
        super::RtmpConnectionState::Failed(_)
    ));
    server.join().unwrap();
}
