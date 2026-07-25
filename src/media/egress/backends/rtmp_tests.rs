use super::*;
use crate::media::egress::journal::FeedEpoch;
use rml_rtmp::handshake::{Handshake as PeerHandshake, HandshakeProcessResult as PeerResult};
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn test_parts() -> RtmpUrlParts {
    crate::media::rtmp::parse_rtmp_url("rtmp://127.0.0.1:1935/live/stream-key").unwrap()
}

/// Real, synchronous `rml_rtmp` server-side peer that performs only the
/// handshake, for tests that stop driving the client once the handshake
/// completes (before any connect-request bytes are flushed).
fn run_handshake_only_server_peer(mut stream: TcpStream) {
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
fn run_full_server_peer(mut stream: TcpStream) {
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

/// Real, synchronous server peer that completes handshake, connect, and
/// publish negotiation like [`run_full_server_peer`], then keeps reading and
/// reports on `video_tx` once it observes a `VideoDataReceived` event —
/// proving media bytes the client engine encoded and wrote actually parse as
/// a valid RTMP video message on the wire, not just that bytes were sent.
fn run_full_server_peer_until_video(
    mut stream: TcpStream,
    video_tx: std::sync::mpsc::Sender<usize>,
) {
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
                    })
                    | ServerSessionResult::RaisedEvent(
                        ServerSessionEvent::PublishStreamRequested { request_id, .. },
                    ) => {
                        for response in session.accept_request(request_id).unwrap() {
                            if let ServerSessionResult::OutboundResponse(packet) = response {
                                stream.write_all(&packet.bytes).unwrap();
                            }
                        }
                    }
                    ServerSessionResult::RaisedEvent(ServerSessionEvent::VideoDataReceived {
                        data,
                        ..
                    }) => {
                        let _ = video_tx.send(data.len());
                        return;
                    }
                    _ => {}
                }
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
    client_stream: &mut RtmpConnection,
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

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = RtmpConnection::plain(client_stream);

    let mut engine =
        RtmpFabricEngine::new_client(test_parts(), 4096, false, RtmpPublishStartup::default())
            .unwrap();
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

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = RtmpConnection::plain(client_stream);

    let mut engine =
        RtmpFabricEngine::new_client(test_parts(), 4096, false, RtmpPublishStartup::default())
            .unwrap();
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

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = RtmpConnection::plain(client_stream);

    let mut engine =
        RtmpFabricEngine::new_client(test_parts(), 4096, false, RtmpPublishStartup::default())
            .unwrap();
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

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = RtmpConnection::plain(client_stream);

    let mut engine =
        RtmpFabricEngine::new_client(test_parts(), 4096, false, RtmpPublishStartup::default())
            .unwrap();
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

#[test]
fn engine_publishes_a_raw_keyframe_once_publish_is_accepted() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (video_tx, video_rx) = std::sync::mpsc::channel::<usize>();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_full_server_peer_until_video(stream, video_tx);
    });

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = RtmpConnection::plain(client_stream);

    let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(4));
    let payload = Bytes::from_static(&[
        0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce, 0x06,
        0xe2, 0, 0, 0, 1, 0x65, 0x88,
    ]);
    ring.push(crate::media::packet::MediaPacket {
        media_type: crate::media::packet::MediaType::Video,
        format: crate::media::packet::PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 100,
        dts: 80,
        payload,
    });
    let feed = RingFeed::new(ring, Arc::new(FeedEpoch::new()));

    let mut engine =
        RtmpFabricEngine::new_client(test_parts(), 4096, false, RtmpPublishStartup::default())
            .unwrap();
    let mut cursor = FeedCursor::new(0, 0);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "video was never received by the server peer"
        );
        if video_rx.try_recv().is_ok() {
            break;
        }
        let progress = engine.advance(
            &mut client_stream,
            Readiness::BOTH,
            &feed,
            &mut cursor,
            budget(),
        );
        match progress {
            EngineProgress::Failed(failure) => panic!("engine failed: {failure:?}"),
            EngineProgress::PeerClosed => panic!("peer closed unexpectedly"),
            _ => thread::sleep(Duration::from_millis(1)),
        }
    }

    server.join().unwrap();
}

#[test]
fn engine_detects_peer_close_during_steady_state_publishing() {
    // `run_full_server_peer` returns (and drops its socket, closing the
    // connection) immediately after accepting the publish request, without
    // ever reading the client's subsequent media writes.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_full_server_peer(stream);
    });

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = RtmpConnection::plain(client_stream);

    let mut engine =
        RtmpFabricEngine::new_client(test_parts(), 4096, false, RtmpPublishStartup::default())
            .unwrap();
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

    // Steady-state publishing against an empty feed and an already-closed
    // peer: nothing is ever queued to write, so a write call never runs to
    // discover the close. Before the control-channel read fix, `advance`
    // never called `stream.read()` once Publishing, and `Needs` interest
    // dropped to `Interest::NONE`/`WRITE` — this loop would spin forever
    // instead of observing the close. It must be discovered by reading.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "engine never detected the peer close"
        );
        let progress = engine.advance(
            &mut client_stream,
            Readiness::BOTH,
            &feed,
            &mut cursor,
            budget(),
        );
        match progress {
            EngineProgress::Failed(failure) => {
                assert_eq!(failure.reason, "rtmp_control_read");
                break;
            }
            EngineProgress::Needs(interest) => {
                assert!(
                    interest.readable,
                    "an idle publishing leaf must stay read-registered: {interest:?}"
                );
                thread::sleep(Duration::from_millis(1));
            }
            other => panic!("unexpected progress: {other:?}"),
        }
    }
}
