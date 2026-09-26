//! Graceful-drain regression tests for `RtmpShardBackend`, split out of
//! `rtmp_shard_tests.rs` to stay under the source-audit line cap.

use super::*;

/// Connects one real leaf through a full handshake against
/// `run_accepting_server_peer` (mirrors
/// `sweep_stalled_leaves_closes_only_the_leaf_with_no_recent_progress`'s
/// setup) and returns the backend plus the connected output id, so drain
/// tests can manipulate `pending_application_bytes`/`draining_since`
/// deterministically instead of racing real I/O timing.
fn connected_backend_for_drain_tests() -> (
    RtmpShardBackend<CompioTcpPoller>,
    OutputId,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_accepting_server_peer(stream, done_tx);
    });

    let mut backend =
        RtmpShardBackend::new(CompioTcpPoller::new(4).unwrap(), feed(), budget(), 4096);
    let output_id = OutputId::new("draining-leaf");
    backend.on_command(EgressCommand::Add(output_spec(
        "draining-leaf",
        &format!("rtmp://{addr}/live/key"),
        1,
    )));
    backend.complete_pending_connect(&output_id, 1, addr);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "leaf never reached publish acceptance"
        );
        if done_rx.try_recv().is_ok() {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }
    (backend, output_id, server)
}

#[test]
fn remove_defers_close_until_pending_bytes_are_flushed() {
    // Before this change, `Remove` called `remove_leaf_by_output`
    // immediately, tearing down the transport regardless of whatever the
    // engine still had queued in `pending_application_bytes` — that data
    // was silently lost. `begin_graceful_close` must instead keep a leaf
    // with nonzero pending bytes registered (not closed) until a later
    // sweep or visit finds it flushed.
    let (mut backend, output_id, server) = connected_backend_for_drain_tests();
    server.join().unwrap();

    {
        let socket_ref = *backend.output_sockets.get(&output_id).unwrap();
        let leaf = backend.leaves[socket_ref.key.0].as_mut().unwrap();
        leaf.common.pending_application_bytes = 4096;
    }

    backend.on_command(EgressCommand::Remove(output_id.clone()));

    assert!(
        backend.output_sockets.contains_key(&output_id),
        "a leaf with pending bytes must not be closed immediately on Remove"
    );
    {
        let socket_ref = *backend.output_sockets.get(&output_id).unwrap();
        let leaf = backend.leaves[socket_ref.key.0].as_ref().unwrap();
        assert!(
            leaf.draining_since.is_some(),
            "a deferred close must mark the leaf as draining"
        );
    }

    // Once flushed (pending bytes reach zero), the next sweep must close it
    // for real rather than leaving it registered forever.
    {
        let socket_ref = *backend.output_sockets.get(&output_id).unwrap();
        let leaf = backend.leaves[socket_ref.key.0].as_mut().unwrap();
        leaf.common.pending_application_bytes = 0;
    }
    backend.sweep_draining_leaves(std::time::Instant::now());

    assert!(
        !backend.output_sockets.contains_key(&output_id),
        "a fully flushed draining leaf must close on the next sweep"
    );
}

#[test]
fn remove_closes_immediately_when_nothing_is_queued() {
    // The common case — a leaf with no pending bytes at removal time —
    // must not pay a drain delay it doesn't need.
    let (mut backend, output_id, server) = connected_backend_for_drain_tests();
    server.join().unwrap();
    {
        let socket_ref = *backend.output_sockets.get(&output_id).unwrap();
        let leaf = backend.leaves[socket_ref.key.0].as_mut().unwrap();
        leaf.common.pending_application_bytes = 0;
    }

    backend.on_command(EgressCommand::Remove(output_id.clone()));

    assert!(
        !backend.output_sockets.contains_key(&output_id),
        "a leaf with nothing queued must close immediately, not wait for a drain sweep"
    );
}

#[test]
fn draining_leaf_force_closes_once_the_drain_deadline_passes() {
    // A peer that stops reading mid-drain must not be able to hang a
    // removal or shutdown forever — `sweep_draining_leaves` force-closes
    // once `draining_since` is older than `drain_timeout`, regardless of
    // remaining pending bytes.
    let (mut backend, output_id, server) = connected_backend_for_drain_tests();
    server.join().unwrap();
    backend = backend.with_drain_timeout(Duration::from_millis(50));

    {
        let socket_ref = *backend.output_sockets.get(&output_id).unwrap();
        let leaf = backend.leaves[socket_ref.key.0].as_mut().unwrap();
        leaf.common.pending_application_bytes = 4096;
    }
    backend.on_command(EgressCommand::Remove(output_id.clone()));
    assert!(backend.output_sockets.contains_key(&output_id));

    // Simulate the deadline already having passed, the same way the stall
    // test simulates "no progress for a long time" — real elapsed-time
    // waiting would make this test slow and flaky for no benefit.
    let long_ago = std::time::Instant::now() - Duration::from_secs(3600);
    {
        let socket_ref = *backend.output_sockets.get(&output_id).unwrap();
        let leaf = backend.leaves[socket_ref.key.0].as_mut().unwrap();
        leaf.draining_since = Some(long_ago);
        // Still nonzero — proves the close is deadline-driven, not
        // flush-driven.
        assert_ne!(leaf.common.pending_application_bytes, 0);
    }

    backend.sweep_draining_leaves(std::time::Instant::now());

    assert!(
        !backend.output_sockets.contains_key(&output_id),
        "a leaf stuck draining past its deadline must be force-closed"
    );
}

#[test]
fn shutdown_marks_every_connected_leaf_draining() {
    // `Shutdown` (unlike the old immediate-stop behavior) must give every
    // leaf a chance to flush, not just the one being explicitly removed.
    let (mut backend, output_id, server) = connected_backend_for_drain_tests();
    server.join().unwrap();
    {
        let socket_ref = *backend.output_sockets.get(&output_id).unwrap();
        let leaf = backend.leaves[socket_ref.key.0].as_mut().unwrap();
        leaf.common.pending_application_bytes = 4096;
    }

    backend.on_command(EgressCommand::Shutdown);

    assert!(
        backend.output_sockets.contains_key(&output_id),
        "Shutdown must not close a leaf with pending bytes immediately"
    );
    let socket_ref = *backend.output_sockets.get(&output_id).unwrap();
    let leaf = backend.leaves[socket_ref.key.0].as_ref().unwrap();
    assert!(leaf.draining_since.is_some());
    assert_eq!(
        leaf.draining_reason,
        Some(crate::media::egress::backend::CloseReason::ShardShutdown)
    );
}

/// A peer that accepts TCP but never answers the handshake queues no
/// application bytes, so the pending-byte stall classifier reports it idle.
/// The startup deadline must still terminate it instead of holding the leaf
/// slot, silently, forever.
#[test]
fn startup_deadline_terminates_a_leaf_that_never_reaches_publish() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (_silent_peer, _) = listener.accept().unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
    });

    let mut backend =
        RtmpShardBackend::new(CompioTcpPoller::new(4).unwrap(), feed(), budget(), 4096);
    let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut spec = output_spec("silent-peer", &format!("rtmp://{addr}/live/key"), 1);
    let output_id = spec.id.clone();
    spec.policy.connect_timeout = Duration::from_millis(200);
    spec.progress.terminated_unexpectedly = Some(Arc::clone(&terminated));
    backend.on_command(EgressCommand::Add(spec));
    assert!(backend.complete_pending_connect(&output_id, 1, addr));
    for _ in 0..8 {
        backend.on_ready();
    }

    let before_deadline = Instant::now();
    backend.sweep_stalled_leaves(before_deadline);
    assert!(backend.output_sockets.contains_key(&output_id));
    assert!(!terminated.load(std::sync::atomic::Ordering::Relaxed));

    backend.last_stall_sweep = None;
    backend.sweep_stalled_leaves(before_deadline + Duration::from_millis(250));
    assert!(
        !backend.output_sockets.contains_key(&output_id),
        "a wedged startup must release its leaf slot"
    );
    assert!(terminated.load(std::sync::atomic::Ordering::Relaxed));

    release_tx.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn stall_sweep_samples_each_leaf_once_so_two_sample_rates_have_a_real_window() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (_peer, _) = listener.accept().unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
    });

    let mut backend =
        RtmpShardBackend::new(CompioTcpPoller::new(4).unwrap(), feed(), budget(), 4096);
    let quality = Arc::new(std::sync::Mutex::new(
        crate::media::snapshots::PublisherQuality::default(),
    ));
    let mut spec = output_spec("sampled", &format!("rtmp://{addr}/live/key"), 1);
    let output_id = spec.id.clone();
    spec.policy.connect_timeout = Duration::from_secs(30);
    spec.progress.quality = Some(Arc::clone(&quality));
    backend.on_command(EgressCommand::Add(spec));
    assert!(backend.complete_pending_connect(&output_id, 1, addr));
    for _ in 0..8 {
        backend.on_ready();
    }

    let start = Instant::now();
    backend.sweep_stalled_leaves(start);
    backend.last_stall_sweep = None;
    backend.sweep_stalled_leaves(start + crate::media::egress::delivery::DELIVERY_WINDOW);
    let sampled = quality
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(
        sampled.delivered_bps.is_some() && sampled.tcp_send_rate_mbps.is_some(),
        "the second sweep must rate against the first, not against itself: {sampled:?}"
    );

    release_tx.send(()).unwrap();
    server.join().unwrap();
}
