//! The RTMP leaf announces the ring's per-sequence video configuration.

use super::*;
use restream_sync::RingBuffer;

const SPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80];
const PPS_A: &[u8] = &[0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2];
const PPS_B: &[u8] = &[0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80];
const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88];
const P_SLICE: &[u8] = &[0, 0, 0, 1, 0x41, 0x9a, 0x01];

fn raw_video(parts: &[&[u8]], keyframe: bool, dts: i64) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: keyframe,
        track_index: 0,
        pts: dts,
        dts,
        payload: Bytes::from(parts.concat()),
    }
}

/// Server peer that accepts connect and publish, then forwards every video
/// message body it receives until the client closes.
fn run_server_peer_forwarding_video(
    mut stream: TcpStream,
    video_tx: std::sync::mpsc::Sender<Bytes>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut handshake = PeerHandshake::new(rml_rtmp::handshake::PeerType::Server);
    let mut buf = [0u8; 4096];
    let mut pending_input = loop {
        let n = stream.read(&mut buf).expect("server handshake read");
        assert_ne!(n, 0);
        match handshake.process_bytes(&buf[..n]).unwrap() {
            PeerResult::InProgress { response_bytes } => {
                stream.write_all(&response_bytes).unwrap();
            }
            PeerResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                stream.write_all(&response_bytes).unwrap();
                break remaining_bytes;
            }
        }
    };
    let (mut session, initial_results) = ServerSession::new(ServerSessionConfig::new()).unwrap();
    for result in initial_results {
        if let ServerSessionResult::OutboundResponse(packet) = result {
            stream.write_all(&packet.bytes).unwrap();
        }
    }
    loop {
        for result in session.handle_input(&pending_input).unwrap() {
            match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    stream.write_all(&packet.bytes).unwrap();
                }
                ServerSessionResult::RaisedEvent(
                    ServerSessionEvent::ConnectionRequested { request_id, .. }
                    | ServerSessionEvent::PublishStreamRequested { request_id, .. },
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
                    let _ = video_tx.send(data);
                }
                _ => {}
            }
        }
        pending_input = match stream.read(&mut buf) {
            Ok(n) if n > 0 => buf[..n].to_vec(),
            _ => return,
        };
    }
}

fn connected_client() -> (
    TlsTcpConnection,
    std::sync::mpsc::Receiver<Bytes>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (video_tx, video_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_server_peer_forwarding_video(stream, video_tx);
    });
    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    (TlsTcpConnection::plain(client_stream), video_rx, server)
}

fn header(parameter_sets: &[&[u8]]) -> Bytes {
    restream_core::codec::build_avcc_sequence_header(&parameter_sets.concat())
        .expect("valid parameter sets")
}

/// Drive `engine` until the peer has received `count` video messages in all.
fn advance_until_videos(
    engine: &mut RtmpFabricEngine,
    client_stream: &mut TlsTcpConnection,
    feed: &RingFeed,
    cursor: &mut FeedCursor,
    video_rx: &std::sync::mpsc::Receiver<Bytes>,
    received: &mut Vec<Bytes>,
    count: usize,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while received.len() < count {
        assert!(
            std::time::Instant::now() < deadline,
            "expected {count} video messages, got {received:?}"
        );
        match engine.advance(client_stream, Readiness::BOTH, feed, cursor, budget()) {
            EngineProgress::Failed(failure) => panic!("engine failed: {failure:?}"),
            EngineProgress::FeedOverrun => panic!("no unit was overwritten"),
            _ => thread::sleep(Duration::from_millis(1)),
        }
        received.extend(video_rx.try_iter());
    }
}

/// Each unit's configuration is looked up by its own feed sequence, across
/// feed reads: the leaf announces A before the first keyframe (after
/// skipping the leading frames it cannot decode) and B before the keyframe
/// that carries it, published after the leaf drained the first burst.
#[test]
fn leaf_announces_each_ring_configuration_at_its_keyframe() {
    let (mut client_stream, video_rx, server) = connected_client();
    let ring = Arc::new(RingBuffer::new(16));
    ring.push_drained_batch_recording_config(&mut vec![
        raw_video(&[P_SLICE], false, 0),
        raw_video(&[P_SLICE], false, 40),
        raw_video(&[SPS, PPS_A, IDR], true, 80),
        raw_video(&[P_SLICE], false, 120),
    ]);
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
    let mut engine =
        RtmpFabricEngine::new_client(test_parts(), 4096, RtmpPublishStartup::default()).unwrap();
    let mut cursor = FeedCursor::new(0, 0);
    let mut received = Vec::new();
    advance_until_videos(
        &mut engine,
        &mut client_stream,
        &feed,
        &mut cursor,
        &video_rx,
        &mut received,
        3,
    );
    ring.push_drained_batch_recording_config(&mut vec![raw_video(&[SPS, PPS_B, IDR], true, 160)]);
    advance_until_videos(
        &mut engine,
        &mut client_stream,
        &feed,
        &mut cursor,
        &video_rx,
        &mut received,
        5,
    );

    assert_eq!(received[0], header(&[SPS, PPS_A]));
    assert_eq!(&received[1][..2], &[0x17, 0x01], "keyframe A");
    assert_eq!(&received[2][..2], &[0x27, 0x01], "interframe");
    assert_eq!(received[3], header(&[SPS, PPS_B]));
    assert_eq!(&received[4][..2], &[0x17, 0x01], "keyframe B");

    drop(client_stream);
    server.join().unwrap();
}

/// The startup header announces the ring configuration it was built from,
/// so the first keyframe of that generation is not preceded by a repeat.
#[test]
fn leaf_does_not_repeat_the_startup_configuration() {
    let (mut client_stream, video_rx, server) = connected_client();
    let ring = Arc::new(RingBuffer::new(16));
    ring.push_drained_batch_recording_config(&mut vec![raw_video(&[SPS, PPS_A, IDR], true, 0)]);
    let (startup_header, generation) =
        restream_rtmp::startup_video_sequence_header(&ring, None, false)
            .expect("ring configuration builds a startup header");
    let startup = RtmpPublishStartup {
        startup_video_sequence_header: Some(startup_header),
        startup_video_generation: generation,
        ..RtmpPublishStartup::default()
    };
    let feed = RingFeed::new(ring, Arc::new(FeedEpoch::new()));
    let mut engine = RtmpFabricEngine::new_client(test_parts(), 4096, startup).unwrap();
    let mut cursor = FeedCursor::new(0, 0);
    let mut received = Vec::new();
    advance_until_videos(
        &mut engine,
        &mut client_stream,
        &feed,
        &mut cursor,
        &video_rx,
        &mut received,
        2,
    );

    assert_eq!(received[0], header(&[SPS, PPS_A]), "startup header");
    assert_eq!(
        &received[1][..2],
        &[0x17, 0x01],
        "keyframe, no second header"
    );

    drop(client_stream);
    server.join().unwrap();
}

/// A keyframe the producer overwrote while it waited in the leaf's pending
/// burst resynchronizes the leaf instead of being sent with a configuration
/// that no longer belongs to it.
#[test]
fn leaf_resyncs_when_a_pending_keyframe_was_overwritten() {
    let (mut client_stream, _video_rx, server) = connected_client();
    let ring = Arc::new(RingBuffer::new(8));
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
    let mut engine =
        RtmpFabricEngine::new_client(test_parts(), 4096, RtmpPublishStartup::default()).unwrap();
    let mut cursor = FeedCursor::new(0, 0);
    drive_to(
        &mut engine,
        &mut client_stream,
        &feed,
        &mut cursor,
        RtmpFabricEngine::is_publish_accepted,
    );

    let keyframes = |count: i64, from: i64| {
        (0..count)
            .map(|index| raw_video(&[SPS, PPS_A, IDR], true, (from + index) * 40))
            .collect::<Vec<_>>()
    };
    ring.push_drained_batch_recording_config(&mut keyframes(5, 0));
    let write_blocked = Readiness {
        readable: false,
        writable: false,
    };
    engine.advance(
        &mut client_stream,
        write_blocked,
        &feed,
        &mut cursor,
        budget(),
    );
    assert_eq!(engine.publisher_pending_units_len(), Some(4));

    ring.push_drained_batch_recording_config(&mut keyframes(8, 5));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "the overwritten pending keyframe never resynchronized the leaf"
        );
        match engine.advance(
            &mut client_stream,
            Readiness::BOTH,
            &feed,
            &mut cursor,
            budget(),
        ) {
            EngineProgress::FeedOverrun => break,
            EngineProgress::Failed(failure) => panic!("engine failed: {failure:?}"),
            _ => thread::sleep(Duration::from_millis(1)),
        }
    }
    assert_eq!(
        engine.publisher_pending_units_len(),
        Some(0),
        "stale pending units are dropped with the resync"
    );

    drop(client_stream);
    server.join().unwrap();
}
