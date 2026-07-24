use super::*;
use crate::media::egress::journal::FeedEpoch;
use rml_rtmp::handshake::{Handshake as PeerHandshake, HandshakeProcessResult as PeerResult};
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream as StdTcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn test_parts() -> RtmpUrlParts {
    crate::media::rtmp::parse_rtmp_url("rtmp://127.0.0.1:1935/live/stream-key").unwrap()
}

/// Real, synchronous `rml_rtmp` server-side peer that performs only the
/// handshake, for tests that stop driving the client once the handshake
/// completes (before any connect-request bytes are flushed).
fn run_handshake_only_server_peer(mut stream: StdTcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut handshake = PeerHandshake::new(rml_rtmp::handshake::PeerType::Server);
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).expect("server handshake read");
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

/// Real, synchronous `rml_rtmp` server-side peer: performs the handshake,
/// then auto-accepts the connect and publish requests via `ServerSession`,
/// mirroring the minimal subset of `src/media/rtmp/ingest.rs`'s real accept
/// path needed to prove the fabric engine reaches `PublishAccepted` against
/// an actual protocol state machine, not a hand-rolled byte fixture.
fn run_full_server_peer(mut stream: StdTcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut handshake = PeerHandshake::new(rml_rtmp::handshake::PeerType::Server);
    let mut buf = [0u8; 4096];
    let remaining;
    loop {
        let n = stream.read(&mut buf).expect("server handshake read");
        assert_ne!(n, 0);
        match handshake.process_bytes(&buf[..n]).unwrap() {
            PeerResult::InProgress { response_bytes } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes).unwrap();
                }
            }
            PeerResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes).unwrap();
                }
                remaining = remaining_bytes;
                break;
            }
        }
    }

    let config = ServerSessionConfig::new();
    let (mut session, initial_results) = ServerSession::new(config).unwrap();
    for result in initial_results {
        if let ServerSessionResult::OutboundResponse(packet) = result {
            stream.write_all(&packet.bytes).unwrap();
        }
    }

    let mut publish_accepted = false;
    let mut pending_input = remaining;
    loop {
        if !pending_input.is_empty() {
            let input = std::mem::take(&mut pending_input);
            let results = session.handle_input(&input).unwrap();
            for result in results {
                match result {
                    ServerSessionResult::OutboundResponse(packet) => {
                        stream.write_all(&packet.bytes).unwrap();
                    }
                    ServerSessionResult::RaisedEvent(ServerSessionEvent::ConnectionRequested {
                        request_id,
                        ..
                    }) => {
                        for response in session.accept_request(request_id).unwrap() {
                            if let ServerSessionResult::OutboundResponse(packet) = response {
                                stream.write_all(&packet.bytes).unwrap();
                            }
                        }
                    }
                    ServerSessionResult::RaisedEvent(
                        ServerSessionEvent::PublishStreamRequested { request_id, .. },
                    ) => {
                        for response in session.accept_request(request_id).unwrap() {
                            if let ServerSessionResult::OutboundResponse(packet) = response {
                                stream.write_all(&packet.bytes).unwrap();
                            }
                        }
                        publish_accepted = true;
                    }
                    _ => {}
                }
            }
            if publish_accepted {
                return;
            }
        }

        let n = stream.read(&mut buf).expect("server session read");
        assert_ne!(n, 0);
        pending_input = buf[..n].to_vec();
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

fn drive_to<F>(
    engine: &mut RtmpFabricEngine,
    client_stream: &mut TcpStream,
    feed: &RingFeed,
    cursor: &mut FeedCursor,
    mut is_done: F,
) where
    F: FnMut(&RtmpFabricEngine) -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "engine did not reach the expected state in time"
        );
        let progress = engine.advance(client_stream, Readiness::BOTH, feed, cursor, budget());
        match progress {
            EngineProgress::HandshakeComplete => {
                if is_done(engine) {
                    return;
                }
            }
            EngineProgress::Needs(_) => thread::sleep(Duration::from_millis(1)),
            EngineProgress::Failed(failure) => panic!("engine failed: {failure:?}"),
            other => panic!("unexpected progress: {other:?}"),
        }
    }
}

#[test]
fn engine_reaches_handshake_complete_through_the_visit_loop() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_handshake_only_server_peer(stream);
    });

    let mut client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();

    let mut engine = RtmpFabricEngine::new_client(test_parts(), 4096, false).unwrap();
    let feed = dummy_feed();
    let mut cursor = FeedCursor::new(0, 0);

    drive_to(
        &mut engine,
        &mut client_stream,
        &feed,
        &mut cursor,
        RtmpFabricEngine::is_handshake_done,
    );

    assert!(engine.is_handshake_done());
    assert!(!engine.is_publish_accepted());
    server.join().unwrap();
}

#[test]
fn engine_reaches_publish_accepted_through_the_visit_loop() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_full_server_peer(stream);
    });

    let mut client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();

    let mut engine = RtmpFabricEngine::new_client(test_parts(), 4096, false).unwrap();
    let feed = dummy_feed();
    let mut cursor = FeedCursor::new(0, 0);

    drive_to(
        &mut engine,
        &mut client_stream,
        &feed,
        &mut cursor,
        RtmpFabricEngine::is_publish_accepted,
    );

    assert!(engine.is_publish_accepted());
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

    let mut engine = RtmpFabricEngine::new_client(test_parts(), 4096, false).unwrap();
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

#[test]
fn engine_reports_protocol_failure_when_peer_closes_mid_negotiation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut stream = stream;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut handshake = PeerHandshake::new(rml_rtmp::handshake::PeerType::Server);
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).expect("server handshake read");
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
                    break;
                }
            }
        }
        // Handshake done, but close before responding to the connect request.
        drop(stream);
    });

    let mut client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();

    let mut engine = RtmpFabricEngine::new_client(test_parts(), 4096, false).unwrap();
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
                assert_eq!(failure.reason, "rtmp_session_negotiation");
                break;
            }
            EngineProgress::HandshakeComplete | EngineProgress::Needs(_) => {
                thread::sleep(Duration::from_millis(1));
            }
            other => panic!("unexpected progress before failure: {other:?}"),
        }
    }

    server.join().unwrap();
}
