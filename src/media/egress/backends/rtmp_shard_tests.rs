use super::*;
use crate::media::egress::journal::FeedEpoch;
use crate::media::egress::leaf::EgressProgressSink;
use crate::media::egress::policy::LeafPolicy;
use std::sync::Arc;

fn budget() -> WorkBudget {
    WorkBudget::new(8, 4096, Duration::from_millis(50))
}

fn feed() -> RingFeed {
    RingFeed::new(
        Arc::new(crate::media::ring_buffer::RingBuffer::new(4)),
        Arc::new(FeedEpoch::new()),
    )
}

fn output_spec(id: &str, url: &str, generation: u64) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation,
        feed: crate::media::egress::command::FeedId::new("feed"),
        protocol: ProtocolSpec::Rtmp {
            url: url.to_string(),
            tls: false,
        },
        policy: LeafPolicy::default(),
        progress: EgressProgressSink::default(),
    }
}

#[test]
fn queues_a_pending_connect_on_add() {
    let mut backend =
        RtmpShardBackend::new(TcpEgressPoller::new(4).unwrap(), feed(), budget(), 4096);

    backend.on_command(EgressCommand::Add(output_spec(
        "out-1",
        "rtmp://127.0.0.1:1935/live/key",
        1,
    )));

    let pending = backend
        .pending_connect(&OutputId::new("out-1"))
        .expect("pending connect must be queued");
    assert_eq!(pending.parts.host, "127.0.0.1");
    assert_eq!(pending.parts.port, 1935);
}

#[test]
fn invalid_url_is_rejected_without_panicking() {
    let mut backend =
        RtmpShardBackend::new(TcpEgressPoller::new(4).unwrap(), feed(), budget(), 4096);

    backend.on_command(EgressCommand::Add(output_spec("out-1", "not a url", 1)));

    assert!(backend.pending_connect(&OutputId::new("out-1")).is_none());
}

#[test]
fn remove_drops_a_pending_connect() {
    let mut backend =
        RtmpShardBackend::new(TcpEgressPoller::new(4).unwrap(), feed(), budget(), 4096);
    let output_id = OutputId::new("out-1");

    backend.on_command(EgressCommand::Add(output_spec(
        "out-1",
        "rtmp://127.0.0.1:1935/live/key",
        1,
    )));
    assert!(backend.pending_connect(&output_id).is_some());

    backend.on_command(EgressCommand::Remove(output_id.clone()));
    assert!(backend.pending_connect(&output_id).is_none());
}

#[test]
fn stale_generation_resolve_completion_is_ignored() {
    let mut backend =
        RtmpShardBackend::new(TcpEgressPoller::new(4).unwrap(), feed(), budget(), 4096);
    let output_id = OutputId::new("out-1");

    backend.on_command(EgressCommand::Add(output_spec(
        "out-1",
        "rtmp://127.0.0.1:1/live/key",
        5,
    )));

    // Completion for an older generation than the current pending connect
    // must not consume it — a newer Update may already be in flight.
    backend.complete_pending_connect(&output_id, 4, "127.0.0.1:1".parse().unwrap());

    assert!(
        backend.pending_connect(&output_id).is_some(),
        "stale-generation completion must not consume the current pending connect"
    );
}

#[test]
fn connect_failure_drops_the_pending_connect_without_panicking() {
    let mut backend =
        RtmpShardBackend::new(TcpEgressPoller::new(4).unwrap(), feed(), budget(), 4096);
    let output_id = OutputId::new("out-1");

    backend.on_command(EgressCommand::Add(output_spec(
        "out-1",
        "rtmp://127.0.0.1:1/live/key",
        1,
    )));

    // Port 0 in a resolved addr is not connectable; connect must fail fast
    // instead of blocking for the full connect_timeout on an unroutable
    // combination, and must not leave a stale pending entry behind.
    backend.complete_pending_connect(&output_id, 1, "127.0.0.1:0".parse().unwrap());

    assert!(backend.pending_connect(&output_id).is_none());
    assert!(!backend.output_sockets.contains_key(&output_id));
}

// ---------------------------------------------------------------------------
// Real end-to-end: shard-driven leaf reaches PublishAccepted against a real
// rml_rtmp::sessions::ServerSession peer, proving the ready-queue +
// per-visit poller-interest-reregistration loop actually drives the engine
// to completion, not just that a socket gets connected.
// ---------------------------------------------------------------------------

use rml_rtmp::handshake::{
    Handshake as PeerHandshake, HandshakeProcessResult as PeerResult, PeerType,
};
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream as StdTcpStream};
use std::thread;

/// Accepts a publish request and then immediately drops the connection, so the
/// shard observes the close. Tests that need the peer to stay connected use
/// [`run_accepting_server_peer_until_stopped`] instead.
fn run_accepting_server_peer(mut stream: StdTcpStream, done_tx: std::sync::mpsc::Sender<()>) {
    run_server_peer_until_publish_accepted(&mut stream, done_tx);
}

/// Accepts a publish request, signals `done_tx`, then holds the connection open
/// until `stop_rx` fires.
///
/// Tests that drive two peers to `Publishing` before asserting on shard state
/// need this: `run_accepting_server_peer` closes as soon as it accepts, so the
/// peer that finishes first would sit closed while the pump loop waits on the
/// second, and a later `on_ready()` would legitimately reap that first leaf
/// (the behavior `shard_removes_the_leaf_once_the_peer_closes_after_publish_acceptance`
/// asserts) out from under the test.
fn run_accepting_server_peer_until_stopped(
    mut stream: StdTcpStream,
    done_tx: std::sync::mpsc::Sender<()>,
    stop_rx: std::sync::mpsc::Receiver<()>,
) {
    run_server_peer_until_publish_accepted(&mut stream, done_tx);
    // Park without reading: the caller's feed is empty, so the shard writes
    // nothing that could fill the socket buffer while we wait.
    let _ = stop_rx.recv();
}

fn run_server_peer_until_publish_accepted(
    stream: &mut StdTcpStream,
    done_tx: std::sync::mpsc::Sender<()>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut handshake = PeerHandshake::new(PeerType::Server);
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
                        let _ = done_tx.send(());
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

#[test]
fn shard_driven_leaf_reaches_publish_accepted_against_a_real_peer() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_accepting_server_peer(stream, done_tx);
    });

    let mut backend =
        RtmpShardBackend::new(TcpEgressPoller::new(4).unwrap(), feed(), budget(), 4096);
    let output_id = OutputId::new("out-1");
    backend.on_command(EgressCommand::Add(output_spec(
        "out-1",
        &format!("rtmp://{}/live/key", addr),
        1,
    )));
    backend.complete_pending_connect(&output_id, 1, addr);
    assert!(
        backend.output_sockets.contains_key(&output_id),
        "leaf must be connected and registered"
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "shard-driven leaf never reached publish acceptance"
        );
        if done_rx.try_recv().is_ok() {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }

    server.join().unwrap();
}

#[test]
fn sweep_stalled_leaves_closes_only_the_leaf_with_no_recent_progress() {
    // `LeafCommon::pending_application_bytes` (wired up in
    // `visit_one_ready_leaf`) used to mean nothing: nothing ever read it.
    // `sweep_stalled_leaves` (mirroring `SrtShardBackend`'s identical
    // mechanism) is what makes it actually enforce something: a leaf with
    // pending bytes and no byte/protocol progress within
    // `LeafLimits::max_backpressure_duration` gets closed. This test
    // drives two real leaves to `Publishing` (real handshake/negotiation
    // against `run_accepting_server_peer` peers), then directly sets each
    // leaf's stall-relevant state (`pending_application_bytes`,
    // `observed_since` — both otherwise driven by real I/O timing, which
    // would make this test slow and flaky) to deterministically simulate
    // "genuinely stuck for a long time" on one leaf and "healthy" on the
    // other, and asserts the sweep closes only the stuck one.
    // Both peers must stay connected until the sweep assertions run. If a peer
    // closed as soon as it accepted (what `run_accepting_server_peer` does),
    // the one that finished first would sit closed while the pump loop below
    // waits on the second, and the next `on_ready()` would reap that leaf
    // before the test could touch it.
    let listener_a = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr_a = listener_a.local_addr().unwrap();
    let (done_a_tx, done_a_rx) = std::sync::mpsc::channel::<()>();
    let (stop_a_tx, stop_a_rx) = std::sync::mpsc::channel::<()>();
    let server_a = thread::spawn(move || {
        let (stream, _) = listener_a.accept().unwrap();
        run_accepting_server_peer_until_stopped(stream, done_a_tx, stop_a_rx);
    });

    let listener_b = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr_b = listener_b.local_addr().unwrap();
    let (done_b_tx, done_b_rx) = std::sync::mpsc::channel::<()>();
    let (stop_b_tx, stop_b_rx) = std::sync::mpsc::channel::<()>();
    let server_b = thread::spawn(move || {
        let (stream, _) = listener_b.accept().unwrap();
        run_accepting_server_peer_until_stopped(stream, done_b_tx, stop_b_rx);
    });

    let mut backend =
        RtmpShardBackend::new(TcpEgressPoller::new(4).unwrap(), feed(), budget(), 4096);
    let stuck_id = OutputId::new("stuck");
    let healthy_id = OutputId::new("healthy");
    backend.on_command(EgressCommand::Add(output_spec(
        "stuck",
        &format!("rtmp://{}/live/key", addr_a),
        1,
    )));
    backend.complete_pending_connect(&stuck_id, 1, addr_a);
    backend.on_command(EgressCommand::Add(output_spec(
        "healthy",
        &format!("rtmp://{}/live/key", addr_b),
        1,
    )));
    backend.complete_pending_connect(&healthy_id, 1, addr_b);

    let mut a_done = false;
    let mut b_done = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "leaves never reached publish acceptance (a_done={a_done} b_done={b_done})"
        );
        a_done = a_done || done_a_rx.try_recv().is_ok();
        b_done = b_done || done_b_rx.try_recv().is_ok();
        if a_done && b_done {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }

    let now = std::time::Instant::now();
    let long_ago = now - Duration::from_secs(3600);
    {
        let socket_ref = *backend.output_sockets.get(&stuck_id).unwrap();
        let leaf = backend.leaves[socket_ref.key.0].as_mut().unwrap();
        leaf.common.pending_application_bytes = 4096;
        leaf.common.progress.last_byte_progress = None;
        leaf.common.progress.last_protocol_progress = None;
        leaf.observed_since = long_ago;
    }
    {
        let socket_ref = *backend.output_sockets.get(&healthy_id).unwrap();
        let leaf = backend.leaves[socket_ref.key.0].as_mut().unwrap();
        leaf.common.pending_application_bytes = 4096;
        leaf.common.progress.last_byte_progress = Some(now);
        leaf.common.progress.last_protocol_progress = None;
        leaf.observed_since = long_ago;
    }

    backend.sweep_stalled_leaves(now);

    assert!(
        !backend.output_sockets.contains_key(&stuck_id),
        "a leaf with pending bytes and no progress for the deadline must be closed"
    );
    assert!(
        backend.output_sockets.contains_key(&healthy_id),
        "a leaf with recent byte progress must not be closed, even with the same pending bytes"
    );

    let _ = stop_a_tx.send(());
    let _ = stop_b_tx.send(());
    server_a.join().unwrap();
    server_b.join().unwrap();
}

#[test]
fn shard_removes_the_leaf_once_the_peer_closes_after_publish_acceptance() {
    // `run_accepting_server_peer` returns (and drops its socket, closing
    // the connection) immediately after accepting the publish request.
    // With an empty feed, nothing is ever queued to write, so only a read
    // (the steady-state control-channel read fix) discovers the close;
    // that must map to `VisitDecision::Close` and the shard must then
    // actually remove the leaf from `output_sockets`/`leaves` -- proving
    // the `Option<OutputId>` plumbing (only cloned on Close) still gets a
    // real `OutputId` through to `remove_leaf_by_output` end to end.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_accepting_server_peer(stream, done_tx);
    });

    let mut backend =
        RtmpShardBackend::new(TcpEgressPoller::new(4).unwrap(), feed(), budget(), 4096);
    let output_id = OutputId::new("out-1");
    backend.on_command(EgressCommand::Add(output_spec(
        "out-1",
        &format!("rtmp://{}/live/key", addr),
        1,
    )));
    backend.complete_pending_connect(&output_id, 1, addr);

    let publish_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < publish_deadline,
            "leaf never reached publish acceptance"
        );
        if done_rx.try_recv().is_ok() {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }
    server.join().unwrap();

    let removed_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < removed_deadline,
            "leaf was never removed after the peer closed"
        );
        if !backend.output_sockets.contains_key(&output_id) {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }
    assert!(backend.leaves.iter().all(Option::is_none));
}

/// Server peer that accepts connect/publish like [`run_accepting_server_peer`],
/// signals `publish_tx` once publish is accepted, then keeps reading (with
/// nothing further to send) and signals `video_tx` and returns on the first
/// `VideoDataReceived` event — proving media published *after* the feed
/// went idle is still delivered, the exact gap a feed-wake liveness
/// regression would miss.
fn run_accepting_server_peer_reporting_video_after_idle(
    mut stream: StdTcpStream,
    publish_tx: std::sync::mpsc::Sender<()>,
    video_tx: std::sync::mpsc::Sender<()>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut handshake = PeerHandshake::new(PeerType::Server);
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
                        let _ = publish_tx.send(());
                    }
                    ServerSessionResult::RaisedEvent(ServerSessionEvent::VideoDataReceived {
                        ..
                    }) => {
                        let _ = video_tx.send(());
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

/// Reproduces and proves the fix for a real liveness bug: once a
/// publishing leaf fully drains its feed, its poller registration stops
/// watching any I/O direction (`EngineProgress::Needs(Interest::NONE)`).
/// Before `RtmpShardBackend::on_command` handled `EgressCommand::FeedWake`,
/// that leaf would never be revisited — a `FeedWake` delivered after the
/// feed went idle was a silent no-op, so a second unit published later
/// would never be sent. This drives the feed empty first, waits past
/// publish acceptance with nothing queued, *then* pushes a unit and
/// delivers `FeedWake`, and asserts the server actually receives it.
#[test]
fn feed_wake_delivers_media_published_after_the_leaf_goes_idle() {
    let _expected_panic_silencer = crate::media::test_support::silence_expected_panics();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (publish_tx, publish_rx) = std::sync::mpsc::channel::<()>();
    let (video_tx, video_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_accepting_server_peer_reporting_video_after_idle(stream, publish_tx, video_tx);
    });

    let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(4));
    let mut backend = RtmpShardBackend::new(
        TcpEgressPoller::new(4).unwrap(),
        RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new())),
        budget(),
        4096,
    );
    let output_id = OutputId::new("out-1");
    backend.on_command(EgressCommand::Add(output_spec(
        "out-1",
        &format!("rtmp://{}/live/key", addr),
        1,
    )));
    backend.complete_pending_connect(&output_id, 1, addr);

    // Drive with an empty feed until the server confirms publish is
    // accepted -- the same proven wait pattern
    // `shard_driven_leaf_reaches_publish_accepted_against_a_real_peer` uses,
    // rather than assuming a fixed wall-clock window is enough.
    let publish_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < publish_deadline,
            "leaf never reached publish acceptance"
        );
        if publish_rx.try_recv().is_ok() {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }

    // Publish is accepted; drive a bit longer with the still-empty feed so
    // the leaf settles into Interest::NONE (nothing left to send) -- the
    // exact idle state a stale FeedWake would fail to wake from.
    let settle_deadline = std::time::Instant::now() + Duration::from_millis(200);
    while std::time::Instant::now() < settle_deadline {
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        video_rx.try_recv().is_err(),
        "no media was published yet, so nothing should have been received"
    );

    // Now publish a real unit and deliver the coalesced feed wake exactly
    // the way the fabric's feed-wake watcher does in production.
    let payload = bytes::Bytes::from_static(&[
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
    backend.on_command(EgressCommand::FeedWake);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "media published after the leaf went idle was never delivered \
             (feed-wake liveness regression)"
        );
        if video_rx.try_recv().is_ok() {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }

    server.join().unwrap();
}

#[path = "rtmp_shard_backpressure_tests.rs"]
mod backpressure_tests;
#[path = "rtmp_shard_drain_tests.rs"]
mod drain_tests;
#[path = "rtmp_shard_media_tick_tests.rs"]
mod media_tick_tests;
#[path = "rtmp_shard_reregistration_tests.rs"]
mod reregistration_tests;
