//! Regression tests for `visit_one_ready_leaf`/`refresh_registrations_for_feed_wake`
//! epoll_ctl re-registration behavior, split out of `rtmp_shard_tests.rs` to stay
//! under the source-audit line cap.

use super::*;

/// Wraps a real [`TcpEgressPoller`] and records every `register_leaf`
/// interest, so tests can prove `visit_one_ready_leaf` skips the
/// `epoll_ctl` syscall when the requested interest hasn't changed, without
/// giving up the real epoll behavior the rest of these tests rely on.
struct CountingPoller {
    inner: TcpEgressPoller,
    register_calls: Arc<std::sync::Mutex<Vec<TcpEgressInterest>>>,
}

impl RtmpReadinessPoller for CountingPoller {
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

#[test]
fn visit_one_ready_leaf_skips_reregistration_when_interest_is_unchanged() {
    // Drives a real connection through handshake, negotiation, publish
    // acceptance, an idle settle window, and a feed-wake-triggered publish
    // — the same lifecycle `feed_wake_delivers_media_published_after_the_leaf_goes_idle`
    // exercises — while recording every `register_leaf` call's interest.
    //
    // Real socket timing is too noisy to assert "N calls happened during
    // this window": consecutive visits legitimately see fluctuating
    // readiness (a partial write leaving more queued, a control-channel
    // read arriving, etc.), so the number of visits and their individual
    // interests aren't predictable run to run. What *is* invariant
    // regardless of timing: if `visit_one_ready_leaf`'s skip check is
    // working, `register_leaf` is only ever called when the interest
    // actually differs from the last registration — so the recorded
    // sequence can never contain two consecutive equal entries. The old,
    // unconditional-`register_leaf` code could and did produce adjacent
    // duplicates (confirmed by instrumenting it during development: the
    // same connection's visits frequently repeat `WRITE` several times in
    // a row while draining a multi-packet batch).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (publish_tx, publish_rx) = std::sync::mpsc::channel::<()>();
    let (video_tx, video_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_accepting_server_peer_reporting_video_after_idle(stream, publish_tx, video_tx);
    });

    let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(4));
    let register_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
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
            "media published after the leaf went idle was never delivered"
        );
        if video_rx.try_recv().is_ok() {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }

    let calls = register_calls.lock().unwrap().clone();
    assert!(
        calls.len() >= 3,
        "expected a realistic number of registrations across this lifecycle, got {calls:?}"
    );
    for window in calls.windows(2) {
        assert_ne!(
            window[0], window[1],
            "register_leaf must not be called twice in a row with the same interest: {calls:?}"
        );
    }

    server.join().unwrap();
}

/// Wraps a real [`TcpEgressPoller`] and fails every `register_leaf` call
/// once `should_fail` is set, returning a synthetic
/// [`TcpEgressPollError`] without touching the real epoll set — so a test
/// can force the exact failure mode that left a leaf permanently
/// unrecoverable: registration succeeds normally up to some point in the
/// leaf's real lifecycle (connect, handshake), then a specific later
/// re-registration fails.
struct FailingRegisterPoller {
    inner: TcpEgressPoller,
    should_fail: Arc<std::sync::atomic::AtomicBool>,
}

impl RtmpReadinessPoller for FailingRegisterPoller {
    fn register_leaf(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        interest: TcpEgressInterest,
    ) -> Result<(), TcpEgressPollError> {
        if self.should_fail.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(TcpEgressPollError {
                operation: "epoll_ctl",
                code: libc::EBADF,
                message: "synthetic failure injected by FailingRegisterPoller".to_string(),
            });
        }
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

/// Proves the fix for a real, CI-reproduced bug: a leaf whose poller
/// re-registration fails after a successful connect used to have
/// `registered_interest` updated to the new value regardless (the
/// `register_leaf` Result was discarded), permanently desyncing tracked
/// state from the real kernel registration. The leaf would then never be
/// rediscovered by `poll_ready()` again -- indistinguishable from a
/// healthy idle leaf until the stall sweep eventually force-closed it
/// (or, worse, if `pending_bytes` stayed nonzero, forever until the
/// no-progress deadline). The fix treats a failed re-registration as
/// leaf-fatal: close immediately so the existing retry/reconnect path
/// recovers it, exactly like `EngineProgress::PeerClosed`/`Failed`
/// already do.
#[test]
fn visit_one_ready_leaf_closes_the_leaf_when_reregistration_fails() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (publish_tx, publish_rx) = std::sync::mpsc::channel::<()>();
    let (video_tx, _video_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_accepting_server_peer_reporting_video_after_idle(stream, publish_tx, video_tx);
    });

    let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(4));
    let should_fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let poller = FailingRegisterPoller {
        inner: TcpEgressPoller::new(4).unwrap(),
        should_fail: should_fail.clone(),
    };
    let mut backend = RtmpShardBackend::new(
        poller,
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
    assert!(
        backend.output_sockets.contains_key(&output_id),
        "connect-time registration must succeed"
    );
    // From here on, every re-registration fails -- matching the exact
    // failure point the CI reproduction showed (the first post-handshake
    // interest widen).
    should_fail.store(true, std::sync::atomic::Ordering::Relaxed);

    // Drive the handshake far enough that the engine needs to widen its
    // registration (WRITE -> READ or READ_WRITE) at least once -- that
    // re-registration attempt is the one wired to fail.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "leaf was never closed after a failed re-registration \
             (publish_rx recv: {:?})",
            publish_rx.try_recv()
        );
        if !backend.output_sockets.contains_key(&output_id) {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }

    assert!(
        !backend.output_sockets.contains_key(&output_id),
        "leaf must be removed, not left with a stale registered_interest"
    );

    // The peer never sees a completed handshake in this scenario (the
    // leaf is closed first), so the server thread's read loop exits via
    // EOF once the connection drops -- not joined here since the test's
    // pass/fail doesn't depend on the peer's own teardown timing.
    let _ = server;
}

/// Proves the new direct-enqueue `FeedWake` mechanism never touches the
/// poller at all: drives a leaf to publish acceptance, lets it settle idle
/// (empty feed, `wants_feed_wake` true from `FeedOrIo(READ)`), pushes a
/// unit, then delivers `FeedWake` and asserts zero `register_leaf`/
/// `poll_leaves` calls happen while the leaf still gets visited and the
/// unit gets pulled from the feed. Replaces
/// `refresh_registrations_for_feed_wake_skips_leaves_already_read_write`,
/// which tested the old epoll_ctl-widening implementation this one
/// replaces — this is a strictly stronger proof (zero calls, not just
/// "not repeated").
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
    let register_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
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
    for _ in 0..20 {
        backend.on_command(EgressCommand::FeedWake);
        assert!(
            backend.ready.is_empty(),
            "a handshaking leaf must never be directly enqueued by FeedWake"
        );
    }

    let _ = server;
}
