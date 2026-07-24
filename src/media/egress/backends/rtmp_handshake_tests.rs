use super::*;
use rml_rtmp::handshake::{Handshake as PeerHandshake, HandshakeProcessResult as PeerResult};
use std::net::{TcpListener, TcpStream as StdTcpStream};
use std::thread;
use std::time::Duration;

/// Drive the client-side non-blocking handshake to completion against a
/// real TCP peer running rml_rtmp's own (pure, synchronous) server-side
/// `Handshake` state machine on a background thread — no Tokio anywhere in
/// this test, proving the non-blocking driver is protocol-compatible with
/// the same library the existing async adapter uses.
fn run_server_peer(mut stream: StdTcpStream) {
    // Bounded so a regression fails the test instead of hanging the suite:
    // if the client's non-blocking driver ever again abandons a pending
    // write before reporting completion (see the bug this test caught),
    // this read blocks forever without it.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.set_nonblocking(false).unwrap();
    let mut handshake = PeerHandshake::new(rml_rtmp::handshake::PeerType::Server);
    let mut buf = [0u8; 4096];
    loop {
        let n = stream
            .read(&mut buf)
            .expect("server read (see bounded timeout above)");
        assert_ne!(n, 0, "client closed before completing handshake");
        match handshake.process_bytes(&buf[..n]).unwrap() {
            PeerResult::InProgress { response_bytes } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes).unwrap();
                }
            }
            PeerResult::Completed { response_bytes, .. } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes).unwrap();
                }
                return;
            }
        }
    }
}

fn drive_to_completion(stream: &mut TcpStream) -> Vec<u8> {
    let mut client = NonBlockingRtmpHandshake::new_client().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "handshake did not complete in time"
        );
        // No real poller in this test: probe both interests non-blockingly
        // by always offering full readiness and letting WouldBlock naturally
        // yield — mirrors what the fabric poller would report as ready.
        match client.advance(stream, Readiness::BOTH) {
            HandshakeOutcome::Pending(_) => {
                thread::sleep(Duration::from_millis(1));
            }
            HandshakeOutcome::Complete { remaining } => return remaining,
            HandshakeOutcome::Failed(reason) => panic!("handshake failed: {reason}"),
        }
    }
}

#[test]
fn client_handshake_completes_against_a_real_server_state_machine() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_server_peer(stream);
    });

    let mut client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();

    let remaining = drive_to_completion(&mut client_stream);

    // The server sent no post-handshake chunk-stream bytes in this test, so
    // nothing should be carried over.
    assert!(remaining.is_empty());
    server.join().unwrap();
}

#[test]
fn advance_before_any_readiness_reports_pending_write_without_touching_the_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    // Never accepted -- the client's first advance() must not block or
    // error just because nothing is listening on the read side yet.
    let Ok(mut client_stream) = TcpStream::connect(addr) else {
        // Some platforms complete a loopback connect() only once accepted;
        // either way, an error here proves the point below doesn't apply --
        // skip gracefully.
        return;
    };
    client_stream.set_nonblocking(true).unwrap();

    let mut client = NonBlockingRtmpHandshake::new_client().unwrap();
    let outcome = client.advance(&mut client_stream, Readiness::default());

    assert!(matches!(
        outcome,
        HandshakeOutcome::Pending(interest) if interest.writable
    ));
}
