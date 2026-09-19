//! Idle waiting inside the shard's Compio runtime, Owner protocol deadlines
//! in the park bound, and bounded, quiescent shutdown.

use super::super::*;
use super::support::*;
use crate::media::egress::shard::{EgressShardConfig, EgressShardHandle};
use bytes::Bytes;
use srt_transport::compio::OwnerRxMode;
use std::time::{Duration, Instant};

const LONG: Duration = Duration::from_secs(30);

fn silent_peer() -> (std::net::UdpSocket, std::net::SocketAddr) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    let addr = socket.local_addr().expect("addr");
    (socket, addr)
}

fn rx() -> (flume::Sender<EgressCommand>, flume::Receiver<EgressCommand>) {
    flume::bounded(8)
}

/// U. With the Compio runtime parked and a 30 s bound, a command wakes the
/// shard immediately through the async receive on the SAME runtime.
#[test]
fn command_wakes_a_parked_compio_wait_promptly() {
    let (_hold, silent) = silent_peer();
    let mut harness = Harness::with_settings(SrtOwnerSettings::new(4, LONG));
    harness.add_resolved(srt_spec("out", 1, &url_for(silent)), vec![silent]);
    assert!(
        harness.backend.owners.has_owner(),
        "an Owner exists, so the wait is Compio's"
    );
    let (tx, commands) = rx();
    // The Owner's own next deadline must be later than the command, or this
    // would not distinguish a command wake from a deadline wake.
    assert!(harness.backend.owners.park_bound(LONG) >= Duration::from_millis(250));

    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        tx.send(EgressCommand::FeedWake).unwrap();
    });
    let started = Instant::now();
    let wake = harness.backend.wait_idle(&commands, LONG);
    let elapsed = started.elapsed();
    sender.join().unwrap();

    assert!(
        matches!(wake, EgressShardIdleWake::Command(EgressCommand::FeedWake)),
        "{wake:?}"
    );
    assert!(
        elapsed < Duration::from_millis(220),
        "woke after {elapsed:?}, not at a deadline"
    );
}

/// V. Network/completion activity wakes the shard with no command at all:
/// across the handshake's parks (each bounded at 30 s by the generic shard)
/// the Owner's TX completions and the peer's reply end waits as
/// `BackendActivity`, and the handshake completes without one command.
#[test]
fn owner_network_activity_wakes_the_shard_without_a_command() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::with_settings(SrtOwnerSettings::new(4, LONG));
    harness.add_resolved(srt_spec("out", 1, &url_for(sink.addr)), vec![sink.addr]);
    let (_tx, commands) = rx();

    let started = Instant::now();
    let mut activity_wakes = 0;
    while started.elapsed() < Duration::from_secs(5)
        && sink.stats.sources.lock().unwrap().is_empty()
    {
        // Service, then park, exactly like the shard loop.
        harness.backend.on_ready();
        match harness.backend.wait_idle(&commands, LONG) {
            EgressShardIdleWake::BackendActivity => activity_wakes += 1,
            EgressShardIdleWake::Timeout => {}
            other => panic!("no command was sent, got {other:?}"),
        }
    }
    assert!(
        !sink.stats.sources.lock().unwrap().is_empty(),
        "the handshake reached the peer with no command"
    );
    assert!(
        activity_wakes >= 1,
        "Owner activity ended at least one park"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// W. An Owner's protocol/pool deadline wakes the shard no later than it is
/// due even though the generic bound is 30 s.
#[test]
fn owner_protocol_deadline_bounds_the_park() {
    let (_hold, silent) = silent_peer();
    let mut harness = Harness::with_settings(SrtOwnerSettings::new(4, Duration::from_millis(200)));
    harness.add_resolved(srt_spec("out", 1, &url_for(silent)), vec![silent]);
    let (_tx, commands) = rx();

    let started = Instant::now();
    let _ = harness.backend.wait_idle(&commands, LONG);
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "parked {:?} despite a 200ms attempt deadline",
        started.elapsed()
    );
}

/// X. A shorter bound from the generic shard (an application timer, the idle
/// wait, or the drain deadline) still wins over the Owner's later deadline.
#[test]
fn an_earlier_generic_bound_still_wins() {
    let (_hold, silent) = silent_peer();
    let mut harness = Harness::with_settings(SrtOwnerSettings::new(4, LONG));
    harness.add_resolved(srt_spec("out", 1, &url_for(silent)), vec![silent]);
    let (_tx, commands) = rx();
    let bound = harness.backend.owners.park_bound(Duration::from_millis(20));
    assert!(bound <= Duration::from_millis(20), "park bound {bound:?}");

    let started = Instant::now();
    let _ = harness
        .backend
        .wait_idle(&commands, Duration::from_millis(20));
    assert!(started.elapsed() < Duration::from_millis(250));
}

/// A closed command channel ends the wait as `Disconnected`.
#[test]
fn a_closed_command_channel_disconnects_the_wait() {
    let (_hold, silent) = silent_peer();
    let mut harness = Harness::with_settings(SrtOwnerSettings::new(4, LONG));
    harness.add_resolved(srt_spec("out", 1, &url_for(silent)), vec![silent]);
    let (tx, commands) = rx();
    drop(tx);
    let wake = harness.backend.wait_idle(&commands, LONG);
    assert!(
        matches!(wake, EgressShardIdleWake::Disconnected),
        "{wake:?}"
    );
}

fn spawn_shard(idle: Duration, drain: Duration) -> (EgressShardHandle, TestFeed) {
    let feed = TestFeed::new();
    let reader = feed.reader();
    let config = EgressShardConfig::new(16, 4, 4, 4, idle)
        .unwrap()
        .with_drain_timeout(drain);
    let handle = EgressShardHandle::try_spawn_with(
        crate::media::egress::command::ShardId::new(0),
        config,
        move || {
            resolve_runtime::resolving_srt_shard_backend(reader, budget(), drain, 64, settings())
        },
    )
    .expect("shard starts");
    (handle, feed)
}

/// Y. The generic drain deadline still bounds shutdown when the backend
/// parks in Compio and `idle_wait` is 30 s.
#[test]
fn drain_deadline_bounds_shutdown_with_a_long_idle_wait() {
    let sink = SinkPeer::v4();
    let (handle, feed) = spawn_shard(LONG, Duration::from_millis(400));
    handle
        .try_send(EgressCommand::Add(srt_spec("out", 1, &url_for(sink.addr))))
        .unwrap();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    let start = Instant::now();
    while sink.payloads() < 3 && start.elapsed() < Duration::from_secs(10) {
        feed.publish(unit.clone());
        let _ = handle.deliver_feed_wake();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(sink.payloads() >= 3, "the leaf connected and delivered");

    let started = Instant::now();
    let snapshot = handle.shutdown_and_join();
    assert!(snapshot.stopped && !snapshot.panicked);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "shutdown took {:?} (idle_wait is 30s)",
        started.elapsed()
    );
}

/// Z. Shutting down an idle shard neither hangs nor sits out `idle_wait`,
/// with and without an Owner.
#[test]
fn idle_shutdown_does_not_wait_out_the_idle_wait() {
    // No Owner ever created.
    let (handle, _feed) = spawn_shard(LONG, Duration::from_millis(300));
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    let snapshot = handle.shutdown_and_join();
    assert!(snapshot.stopped && !snapshot.panicked);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );

    // An Owner exists but has no leaves left.
    let sink = SinkPeer::v4();
    let (handle, _feed) = spawn_shard(LONG, Duration::from_millis(300));
    handle
        .try_send(EgressCommand::Add(srt_spec("out", 1, &url_for(sink.addr))))
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    handle
        .try_send(EgressCommand::Remove(OutputId::new("out")))
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    let snapshot = handle.shutdown_and_join();
    assert!(snapshot.stopped && !snapshot.panicked);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// AA/AB. Shutting down while a leaf is actively sending drains every
/// in-flight buffer and, for each instantiated Owner, reaches quiescence:
/// nothing in flight, TX pool whole, receive consumer gone.
#[test]
fn shutdown_under_active_tx_reaches_owner_quiescence() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 8 * 1316]);
    harness.add_resolved(srt_spec("out", 1, &url_for(sink.addr)), vec![sink.addr]);
    assert!(harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 10));

    // Shutdown while data is still flowing.
    harness.publish(unit.clone());
    harness.backend.on_command(EgressCommand::Shutdown);
    let started = Instant::now();
    harness.pump(Duration::from_secs(3), |backend| {
        backend.output_sockets.is_empty()
    });
    harness.backend.on_shutdown();
    assert!(started.elapsed() < Duration::from_secs(4), "bounded");

    let owners = &harness.backend.owners;
    assert_eq!(
        owners.shutdown_incomplete(),
        0,
        "every Owner reached quiescence"
    );
    let owner = owners.owner(AddressFamily::V4).expect("owner");
    assert_eq!(owner.tx_in_flight(), 0);
    let pool = owner.tx_pool_snapshot();
    assert_eq!(pool.free, pool.capacity, "the TX pool is whole");

    // AB. The receive consumer is gone and its ring empty; where this host
    // has no provided-buffer ring the selected mode is the readiness reader
    // and the metrics say so (never a silent claim of managed RX).
    let mut metrics = ShardMetrics::default();
    owners.observe(&mut metrics);
    let family = metrics.srt_owners[AddressFamily::V4.index()];
    assert_eq!(family.rx_ring_depth, 0);
    let expected = if metrics.srt_managed_rx_available {
        OwnerRxMode::ManagedMultishot
    } else {
        OwnerRxMode::RawReadiness
    };
    assert_eq!(owners.rx_mode(AddressFamily::V4), Some(expected));
    assert_eq!(family.managed_rx, metrics.srt_managed_rx_available);
    if let Some(rx) = owner.rx_stats().caller {
        assert_eq!(rx.depth, 0);
        assert!(!rx.staged, "no staged lease survives shutdown");
    }
}
