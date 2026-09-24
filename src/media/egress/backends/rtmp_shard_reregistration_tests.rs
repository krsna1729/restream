//! Regression tests for RTMP shard feed-wake scheduling after moving active
//! sockets to Compio completion workers.

use super::*;

use crate::media::egress::backends::tcp::TcpEgressInterest;
use std::sync::Mutex;
/// Wraps the test TCP poller and records registration calls so the feed-wake
/// test can prove that scheduling media does not invoke the socket poller.
struct CountingPoller {
    inner: TcpEgressPoller,
    register_calls: Arc<Mutex<Vec<TcpEgressInterest>>>,
}

impl RtmpReadinessPoller for CountingPoller {
    fn ready_capacity(&self) -> usize {
        self.inner.ready_capacity()
    }

    fn start_connect(
        &mut self,
        peer_addr: SocketAddr,
        key: LeafKey,
        generation: u64,
        timeout: Duration,
    ) -> Result<TcpConnectAttempt, TcpEgressPollError> {
        self.inner
            .start_connect(peer_addr, key, generation, timeout)
    }

    fn register_leaf(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        interest: TcpEgressInterest,
    ) -> Result<(), TcpEgressPollError> {
        self.register_calls.lock().unwrap().push(interest);
        self.inner.register_leaf(fd, key, generation, interest)
    }

    fn remove(&mut self, fd: RawFd) -> Result<(), TcpEgressPollError> {
        self.inner.remove(fd)
    }

    fn poll_leaves(
        &mut self,
        timeout_ms: i32,
        ready: &mut Vec<TcpReadyLeaf>,
    ) -> Result<usize, TcpEgressPollError> {
        self.inner.poll_leaves(timeout_ms, ready)
    }
}

/// Proves the direct-enqueue `FeedWake` mechanism does not touch the socket
/// poller: it drives a leaf to publish acceptance, lets it settle idle, pushes
/// a unit, then asserts that the unit is delivered without a poller call.
#[test]
fn feed_wake_enqueues_the_leaf_without_any_poller_call() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (publish_tx, publish_rx) = std::sync::mpsc::channel::<()>();
    let (video_tx, video_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_accepting_server_peer_reporting_video_after_idle(stream, publish_tx, video_tx);
    });

    let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(4));
    let register_calls = Arc::new(Mutex::new(Vec::new()));
    let poller = CountingPoller {
        inner: TcpEgressPoller::new(4).unwrap(),
        register_calls: register_calls.clone(),
    };
    let mut backend = RtmpShardBackend::new(
        poller,
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

    let settle_deadline = std::time::Instant::now() + Duration::from_millis(200);
    while std::time::Instant::now() < settle_deadline {
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }
    assert!(video_rx.try_recv().is_err());

    let calls_before_wake = register_calls.lock().unwrap().len();

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
    assert_eq!(
        register_calls.lock().unwrap().len(),
        calls_before_wake,
        "FeedWake's direct-enqueue path must not call register_leaf"
    );
    assert!(
        !backend.ready.is_empty(),
        "FeedWake must directly enqueue the feed-waiting leaf"
    );

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

/// Regression proof for the failure mode a previous direct-enqueue attempt
/// hit (see `enqueue_feed_waiting_leaves`'s doc comment): a leaf still mid
/// handshake/negotiation only ever reports `WaitCondition::Io(_)` (pure
/// I/O wait, never `Feed`/`FeedOrIo`), so `FeedWake` must never enqueue it
/// — it stays discoverable only via real `poll_ready()`, exactly as
/// before. Drives a connection up to (but not through) the handshake and
/// hammers `FeedWake` throughout, asserting the leaf never gets enqueued.
#[test]
fn feed_wake_never_enqueues_a_handshaking_leaf() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    // Accept the connection and then do nothing further: the client leaf
    // stays parked mid-handshake for the whole test, guaranteeing it never
    // reaches a Feed/FeedOrIo wait condition.
    let server = thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_secs(5));
    });

    let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(4));
    let mut backend = RtmpShardBackend::new(
        TcpEgressPoller::new(4).unwrap(),
        RingFeed::new(ring, Arc::new(FeedEpoch::new())),
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

    // Give the connect a moment to land, then hammer FeedWake without ever
    // driving on_ready — proving the direct-enqueue path itself (not luck
    // in visit timing) is what excludes this leaf.
    thread::sleep(Duration::from_millis(50));
    backend.on_ready();
    assert!(
        backend.ready.is_empty(),
        "after its synthetic connect write, the handshaking leaf must wait on socket readiness"
    );
    for _ in 0..20 {
        backend.on_command(EgressCommand::FeedWake);
        assert!(
            backend.ready.is_empty(),
            "a handshaking leaf must never be directly enqueued by FeedWake"
        );
    }

    let _ = server;
}
