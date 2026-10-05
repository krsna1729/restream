use super::*;

static TEST_COUNTERS: TlsCounters = TlsCounters::new();
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
    let mut connection = TlsTcpConnection::plain(client);

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
    let mut connection = TlsTcpConnection::plain(client);
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
    let connection = TlsTcpConnection::plain(client);

    assert_eq!(connection.raw_fd(), expected_fd);
}

#[test]
fn tls_connection_rejects_an_invalid_host_name() {
    let (client, _server) = connected_pair();

    let result = TlsTcpConnection::tls(client, "");

    assert!(result.is_err());
}

#[test]
fn plain_connection_never_reports_a_rustls_buffer_estimate() {
    let (client, _server) = connected_pair();
    let connection = TlsTcpConnection::plain(client);

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
    let connection = TlsTcpConnection::tls(client, "example.com").unwrap();

    assert_eq!(connection.rustls_pending_bytes_estimate(), 64 * 1024);
}

#[test]
fn tls_connection_raw_fd_matches_the_underlying_socket() {
    use std::os::unix::io::AsRawFd;

    let (client, _server) = connected_pair();
    let expected_fd = client.as_raw_fd();
    let connection = TlsTcpConnection::tls(client, "example.com").unwrap();

    assert_eq!(connection.raw_fd(), expected_fd);
}

// ---------------------------------------------------------------------------
// Real handshake round trip: a locally generated self-signed certificate
// (via `rcgen`, a test-only dependency) served by a real
// `rustls::ServerConnection` on a blocking background thread, and the
// client driven non-blocking through `TlsTcpConnection` exactly the way the
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
        TlsTcpConnection::tls_with_config(client_stream, "localhost", config, &TEST_COUNTERS)
            .unwrap();

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
    if !super::ktls::supports(
        tokio_rustls::rustls::ProtocolVersion::TLSv1_2,
        tokio_rustls::rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    ) {
        return;
    }
    assert_ktls_application_data_round_trip(test_client_config_tls12());
}

#[test]
fn tls13_aes256_connection_hands_off_and_exchanges_application_data() {
    if !super::ktls::supports(
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
    let mut connection = TlsTcpConnection::tls_with_config(
        client_stream,
        "localhost",
        test_client_config_tls12(),
        &TEST_COUNTERS,
    )
    .unwrap();
    connection.ktls_state = KtlsState::NotRequested;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "TLS handshake timed out"
        );
        let handshaking = match &connection.state {
            ConnectionState::Tls(Some(stream)) => stream.conn.is_handshaking(),
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

    let ConnectionState::Tls(Some(stream)) = &mut connection.state else {
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
        early_plaintext: Vec::new(),
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
            Ok((TICKET.len(), ktls::RECORD_TYPE_HANDSHAKE))
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
            Ok((5, ktls::RECORD_TYPE_DATA))
        })
        .unwrap();
    assert_eq!(&buffer[..count], b"world");
}

#[derive(Debug, Clone)]
enum ServerRecords {
    Data(Vec<u8>),
    /// NewSessionTickets with these body lengths, cut into handshake records
    /// of these sizes (cycled).
    Tickets(Vec<usize>, Vec<usize>),
}

fn server_records() -> impl proptest::strategy::Strategy<Value = (Vec<ServerRecords>, bool)> {
    use proptest::prelude::*;
    let item = prop_oneof![
        prop::collection::vec(any::<u8>(), 1..64).prop_map(ServerRecords::Data),
        (
            prop::collection::vec(0usize..300, 1..4),
            prop::collection::vec(1usize..64, 1..6)
        )
            .prop_map(|(tickets, cuts)| ServerRecords::Tickets(tickets, cuts)),
    ];
    (prop::collection::vec(item, 0..12), any::<bool>())
}

proptest::proptest! {
    /// After the kTLS handoff the server's records reach the RTMP reader as
    /// exactly its application data, in order: TLS 1.3 tickets split across
    /// records anywhere (headers included) are consumed whole, the per-read
    /// control budget only defers, and close_notify (whole or as two
    /// one-byte records) ends the stream.
    #[test]
    fn ktls_reader_yields_exactly_the_application_data(
        (items, split_close_notify) in server_records()
    ) {
        let mut records = std::collections::VecDeque::new();
        let mut expected = Vec::new();
        for item in items {
            match item {
                ServerRecords::Data(bytes) => {
                    expected.extend_from_slice(&bytes);
                    records.push_back((bytes, ktls::RECORD_TYPE_DATA));
                }
                ServerRecords::Tickets(lengths, cuts) => {
                    let mut handshake = Vec::new();
                    for len in lengths {
                        handshake.push(4);
                        handshake.extend_from_slice(&(len as u32).to_be_bytes()[1..]);
                        handshake.extend(std::iter::repeat_n(0xab, len));
                    }
                    let mut cuts = cuts.into_iter().cycle();
                    while !handshake.is_empty() {
                        let size = cuts.next().unwrap().min(handshake.len());
                        let record: Vec<u8> = handshake.drain(..size).collect();
                        records.push_back((record, ktls::RECORD_TYPE_HANDSHAKE));
                    }
                }
            }
        }
        if split_close_notify {
            records.push_back((vec![1], ktls::RECORD_TYPE_ALERT));
            records.push_back((vec![0], ktls::RECORD_TYPE_ALERT));
        } else {
            records.push_back((vec![1, 0], ktls::RECORD_TYPE_ALERT));
        }

        let (stream, _server) = connected_pair();
        let mut connection = KtlsConnection {
            stream: CompioTcpStream::from_std(stream),
            version: tokio_rustls::rustls::ProtocolVersion::TLSv1_3,
            early_plaintext: Vec::new(),
            handshake_buffer: Vec::new(),
            pending_alert_level: None,
            peer_closed: false,
        };
        let mut received = Vec::new();
        let mut buffer = [0; 64];
        loop {
            let read = connection.read_with(&mut buffer, |buffer| {
                let (record, record_type) = records
                    .pop_front()
                    .expect("the reader stops at close_notify");
                buffer[..record.len()].copy_from_slice(&record);
                Ok((record.len(), record_type))
            });
            match read {
                Ok(0) => break,
                Ok(count) => received.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => proptest::prop_assert!(false, "read failed: {error}"),
            }
        }
        proptest::prop_assert_eq!(received, expected);
        proptest::prop_assert!(connection.handshake_buffer.is_empty());
        proptest::prop_assert!(records.is_empty());
    }
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
    let mut connection = TlsTcpConnection::tls_with_config(
        client_stream,
        "localhost",
        test_client_config_tls13_chacha(),
        &TEST_COUNTERS,
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
        super::ConnectionState::Failed(_)
    ));
    server.join().unwrap();
}

/// Move the client's staged bytes to an in-memory TLS server and the
/// server's answer back into the client's receive buffer.
fn exchange_with(
    client: &TlsTcpConnection,
    server: &mut tokio_rustls::rustls::ServerConnection,
) -> Vec<u8> {
    let sent = client.tcp_stream().take_staged_for_test();
    let mut input = &sent[..];
    while !input.is_empty() {
        server.read_tls(&mut input).unwrap();
        server.process_new_packets().unwrap();
    }
    let mut answer = Vec::new();
    while server.wants_write() {
        server.write_tls(&mut answer).unwrap();
    }
    answer
}

/// A client `TlsTcpConnection` on a Compio stream whose bytes the test
/// moves by hand, driven through the handshake to the point where its
/// Finished is staged but still being sent, and the server's session
/// tickets (returned) are in hand. `None` without kTLS for the suite.
fn client_awaiting_handoff(
    runtime: &compio::runtime::Runtime,
) -> Option<(TlsTcpConnection, Vec<u8>, TcpStream)> {
    if !super::ktls::supports(
        tokio_rustls::rustls::ProtocolVersion::TLSv1_3,
        tokio_rustls::rustls::CipherSuite::TLS13_AES_256_GCM_SHA384,
    ) {
        return None;
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client_socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (peer, _) = listener.accept().unwrap();
    client_socket.set_nonblocking(true).unwrap();
    let socket = runtime
        .enter(|| compio::net::TcpStream::from_std(client_socket))
        .unwrap();
    let mut client = TlsTcpConnection::tls_with_config(
        CompioTcpStream::from_compio(socket),
        "localhost",
        test_client_config_tls13_aes256_gcm(),
        &TEST_COUNTERS,
    )
    .unwrap();
    let cert_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_key.cert.der().clone()],
            PrivatePkcs8KeyDer::from(cert_key.signing_key.serialize_der()).into(),
        )
        .unwrap();
    assert!(
        server_config.send_tls13_tickets > 0,
        "the server sends tickets"
    );
    let mut server = tokio_rustls::rustls::ServerConnection::new(Arc::new(server_config)).unwrap();

    // ClientHello out; the server's flight back.
    assert!(client.write(b"GET").is_err());
    let flight = exchange_with(&client, &mut server);
    client.tcp_stream().push_received_for_test(&flight);
    client.tcp_stream().drain_for_test();
    // The client completes and stages its Finished; it is still being sent.
    assert!(client.write(b"GET").is_err());
    let tickets = exchange_with(&client, &mut server);
    assert!(!server.is_handshaking());
    assert!(
        !tickets.is_empty(),
        "the server answered Finished with tickets"
    );
    Some((client, tickets, peer))
}

/// A TLS 1.3 server sends its session tickets once it has the client's
/// Finished. When they reach the receive buffer before the hand-off (the
/// Finished still being sent, as under load), the hand-off must still happen
/// when that write completes: nothing else reads the buffer in this state,
/// so the connection used to sit in userspace TLS until its timeout.
#[test]
fn session_tickets_received_before_the_ktls_handoff_do_not_hold_it_back() {
    let runtime = compio::runtime::Runtime::new().unwrap();
    let Some((mut client, tickets, _peer)) = client_awaiting_handoff(&runtime) else {
        return;
    };
    client.tcp_stream().push_received_for_test(&tickets);
    assert!(
        client.write(b"GET").is_err(),
        "the Finished is not sent yet"
    );
    client.tcp_stream().drain_for_test();

    assert_eq!(client.write(b"GET").unwrap(), 3);
    assert!(client.is_ktls(), "the connection stayed in userspace TLS");
}

/// Only whole records go to rustls before the hand-off. A record whose end
/// has not arrived stays in the buffer and holds the hand-off back, or its
/// start would stay in rustls and its end go to kernel TLS.
#[test]
fn a_partial_record_before_the_ktls_handoff_waits_for_its_end() {
    let runtime = compio::runtime::Runtime::new().unwrap();
    let Some((mut client, tickets, _peer)) = client_awaiting_handoff(&runtime) else {
        return;
    };
    let (head, tail) = tickets.split_at(tickets.len() - 3);
    client.tcp_stream().push_received_for_test(head);
    client.tcp_stream().drain_for_test();
    assert!(client.write(b"GET").is_err());
    assert!(!client.is_ktls(), "handed off with a record cut in two");
    let last_record_start = head.len() - client.tcp_stream().pending_receive_bytes();
    assert!(
        tickets.len() - last_record_start > 3,
        "only the cut record is left"
    );

    client.tcp_stream().push_received_for_test(tail);
    assert_eq!(client.write(b"GET").unwrap(), 3);
    assert!(client.is_ktls());
}

/// Records that follow the server's Finished in the same flight (here
/// half-RTT application data) arrive with it. rustls must get them as whole
/// records: a raw 4 KiB read would end inside the data record, leaving its
/// start in rustls and its end for the kernel at the hand-off. The data is
/// then read after the hand-off, not lost with the rustls connection.
#[test]
fn records_after_the_servers_finished_are_not_cut_and_their_data_survives_the_handoff() {
    if !super::ktls::supports(
        tokio_rustls::rustls::ProtocolVersion::TLSv1_3,
        tokio_rustls::rustls::CipherSuite::TLS13_AES_256_GCM_SHA384,
    ) {
        return;
    }
    let runtime = compio::runtime::Runtime::new().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client_socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_peer, _) = listener.accept().unwrap();
    client_socket.set_nonblocking(true).unwrap();
    let socket = runtime
        .enter(|| compio::net::TcpStream::from_std(client_socket))
        .unwrap();
    let mut client = TlsTcpConnection::tls_with_config(
        CompioTcpStream::from_compio(socket),
        "localhost",
        test_client_config_tls13_aes256_gcm(),
        &TEST_COUNTERS,
    )
    .unwrap();
    let cert_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let mut server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_key.cert.der().clone()],
            PrivatePkcs8KeyDer::from(cert_key.signing_key.serialize_der()).into(),
        )
        .unwrap();
    server_config.send_half_rtt_data = true;
    server_config.send_tls13_tickets = 0;
    let mut server = tokio_rustls::rustls::ServerConnection::new(Arc::new(server_config)).unwrap();

    assert!(client.write(b"GET").is_err());
    let hello = client.tcp_stream().take_staged_for_test();
    server.read_tls(&mut &hello[..]).unwrap();
    server.process_new_packets().unwrap();
    let mut finished_flight = Vec::new();
    while server.wants_write() {
        server.write_tls(&mut finished_flight).unwrap();
    }
    let data: Vec<u8> = (0..6000).map(|i| (i % 251) as u8).collect();
    server.writer().write_all(&data).unwrap();
    let mut data_records = Vec::new();
    while server.wants_write() {
        server.write_tls(&mut data_records).unwrap();
    }
    assert!(
        finished_flight.len() < 4096 && finished_flight.len() + data_records.len() > 4096,
        "a 4 KiB read must end inside the data record"
    );
    let mut flight = finished_flight;
    flight.extend_from_slice(&data_records);
    client.tcp_stream().push_received_for_test(&flight);
    client.tcp_stream().drain_for_test();

    assert!(
        client.write(b"GET").is_err(),
        "the Finished is not sent yet"
    );
    client.tcp_stream().drain_for_test();
    assert_eq!(client.write(b"GET").unwrap(), 3);
    assert!(client.is_ktls(), "the connection stayed in userspace TLS");
    let mut received = vec![0_u8; data.len()];
    let mut filled = 0;
    while filled < data.len() {
        let count = client.read(&mut received[filled..]).unwrap();
        assert!(count > 0, "the early data ended short");
        filled += count;
    }
    assert!(received == data, "the early data arrived altered");
}
