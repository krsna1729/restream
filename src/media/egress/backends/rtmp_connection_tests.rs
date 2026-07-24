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
