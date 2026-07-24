use super::*;
use crate::media::egress::journal::FeedEpoch;
use rml_rtmp::handshake::{Handshake as PeerHandshake, HandshakeProcessResult as PeerResult};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream as StdTcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Same real, synchronous rml_rtmp server-side peer used to prove the
/// standalone handshake driver (`rtmp_handshake_tests.rs`), reused here to
/// prove the *engine* reaches `HandshakeComplete` through the same
/// `ProtocolEngine::advance` visit loop the shard scheduler drives.
fn run_server_peer(mut stream: StdTcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut handshake = PeerHandshake::new(rml_rtmp::handshake::PeerType::Server);
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).expect("server read");
        assert_ne!(n, 0);
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

fn dummy_feed() -> RingFeed {
    RingFeed::new(
        Arc::new(crate::media::ring_buffer::RingBuffer::new(4)),
        Arc::new(FeedEpoch::new()),
    )
}

fn budget() -> WorkBudget {
    WorkBudget::new(8, 4096, Duration::from_millis(50))
}

#[test]
fn engine_reaches_handshake_complete_through_the_visit_loop() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_server_peer(stream);
    });

    let mut client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();

    let mut engine = RtmpFabricEngine::new_client().unwrap();
    let feed = dummy_feed();
    let mut cursor = FeedCursor::new(0, 0);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "engine did not reach HandshakeComplete in time"
        );
        let progress = engine.advance(
            &mut client_stream,
            Readiness::BOTH,
            &feed,
            &mut cursor,
            budget(),
        );
        match progress {
            EngineProgress::HandshakeComplete => break,
            EngineProgress::Needs(_) => thread::sleep(Duration::from_millis(1)),
            EngineProgress::Failed(failure) => panic!("engine failed: {failure:?}"),
            other => panic!("unexpected progress: {other:?}"),
        }
    }

    assert!(engine.is_handshake_done());
    server.join().unwrap();
}

#[test]
fn engine_reports_protocol_failure_when_peer_closes_mid_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        // Read C0/C1 then close immediately without responding.
        let mut buf = [0u8; 4096];
        let mut stream = stream;
        let _ = stream.read(&mut buf);
        drop(stream);
    });

    let mut client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();

    let mut engine = RtmpFabricEngine::new_client().unwrap();
    let feed = dummy_feed();
    let mut cursor = FeedCursor::new(0, 0);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(std::time::Instant::now() < deadline, "engine never failed");
        let progress = engine.advance(
            &mut client_stream,
            Readiness::BOTH,
            &feed,
            &mut cursor,
            budget(),
        );
        match progress {
            EngineProgress::Failed(failure) => {
                assert_eq!(failure.reason, "rtmp_handshake");
                break;
            }
            EngineProgress::Needs(_) => thread::sleep(Duration::from_millis(1)),
            other => panic!("unexpected progress before failure: {other:?}"),
        }
    }

    server.join().unwrap();
}
