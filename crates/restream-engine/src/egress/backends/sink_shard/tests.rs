use super::*;
use crate::egress::command::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::egress::journal::FeedEpoch;
use crate::egress::leaf::EgressProgressSink;
use crate::egress::policy::{LeafPolicy, WorkBudgetConfig};
use crate::egress::shard::{EgressShardConfig, EgressShardHandle};
use restream_core::{MediaPacket, MediaType, PayloadFormat};
use restream_sync::RingBuffer;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

fn config() -> EgressShardConfig {
    EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(5)).unwrap()
}

fn output_spec(id: &str, bytes_sent: Arc<AtomicU64>) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation: 1,
        feed: FeedId::new("feed-sink"),
        protocol: ProtocolSpec::Sink,
        policy: LeafPolicy::default(),
        progress: EgressProgressSink {
            bytes_sent: Some(bytes_sent),
            ..Default::default()
        },
    }
}

fn push_video_packet(ring: &RingBuffer) {
    ring.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: bytes::Bytes::from_static(b"abcde"),
    });
}

/// Proves `SinkShardBackend` actually runs `SinkEngine` on a real shard OS
/// thread through the same `EgressCommand`/`EgressShardHandle` path
/// SRT/RTMP use, not the plain `tokio::spawn` task of `start_sink_egress`.
#[test]
fn sink_shard_backend_discards_a_real_unit_on_a_real_shard_thread() {
    let ring = Arc::new(RingBuffer::new(4));
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
    let handle = EgressShardHandle::spawn(
        crate::egress::command::ShardId::new(0),
        config(),
        SinkShardBackend::new(
            feed,
            WorkBudgetConfig::new(8, 4096, Duration::from_millis(50)),
        ),
    );

    let bytes_sent = Arc::new(AtomicU64::new(0));
    handle
        .try_send(EgressCommand::Add(output_spec(
            "out-sink",
            bytes_sent.clone(),
        )))
        .unwrap();

    push_video_packet(&ring);
    handle.try_send(EgressCommand::FeedWake).unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while bytes_sent.load(Ordering::Relaxed) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "sink leaf never discarded the published unit"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(bytes_sent.load(Ordering::Relaxed), 5);

    let snapshot = handle.shutdown_and_join();
    assert!(!snapshot.panicked);
}

/// `FeedWake` is the only readiness signal this backend has (no socket, no
/// poller — see the module doc) — proves a second published unit is picked
/// up by a second `FeedWake`, not just the first one at `Add` time.
#[test]
fn sink_shard_backend_discards_units_published_after_the_leaf_goes_idle() {
    let ring = Arc::new(RingBuffer::new(4));
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
    let handle = EgressShardHandle::spawn(
        crate::egress::command::ShardId::new(0),
        config(),
        SinkShardBackend::new(
            feed,
            WorkBudgetConfig::new(8, 4096, Duration::from_millis(50)),
        ),
    );

    let bytes_sent = Arc::new(AtomicU64::new(0));
    handle
        .try_send(EgressCommand::Add(output_spec(
            "out-sink",
            bytes_sent.clone(),
        )))
        .unwrap();

    // Let the leaf go idle (nothing published yet, so no progress).
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(bytes_sent.load(Ordering::Relaxed), 0);

    push_video_packet(&ring);
    handle.try_send(EgressCommand::FeedWake).unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while bytes_sent.load(Ordering::Relaxed) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "media published after the leaf went idle was never delivered \
             (feed-wake liveness regression)"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let snapshot = handle.shutdown_and_join();
    assert!(!snapshot.panicked);
}

/// `Remove` must actually stop the leaf from being visited again — proves
/// the removal path (`remove_leaf_by_output` -> `remove_leaf_key`) works on
/// a real shard thread, not just that `Add` does.
#[test]
fn sink_shard_backend_stops_discarding_after_remove() {
    let ring = Arc::new(RingBuffer::new(4));
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
    let handle = EgressShardHandle::spawn(
        crate::egress::command::ShardId::new(0),
        config(),
        SinkShardBackend::new(
            feed,
            WorkBudgetConfig::new(8, 4096, Duration::from_millis(50)),
        ),
    );

    let bytes_sent = Arc::new(AtomicU64::new(0));
    handle
        .try_send(EgressCommand::Add(output_spec(
            "out-sink",
            bytes_sent.clone(),
        )))
        .unwrap();
    push_video_packet(&ring);
    handle.try_send(EgressCommand::FeedWake).unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while bytes_sent.load(Ordering::Relaxed) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "first unit never discarded"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    handle
        .try_send(EgressCommand::Remove(OutputId::new("out-sink")))
        .unwrap();
    std::thread::sleep(Duration::from_millis(20));

    push_video_packet(&ring);
    handle.try_send(EgressCommand::FeedWake).unwrap();
    std::thread::sleep(Duration::from_millis(100));

    assert_eq!(
        bytes_sent.load(Ordering::Relaxed),
        5,
        "removed leaf must not keep discarding"
    );

    let snapshot = handle.shutdown_and_join();
    assert!(!snapshot.panicked);
}

/// The shard prices its outputs from this sweep: over a closed delivery
/// window a leaf that keeps up is rated at the feed's rate, and one that
/// consumed nothing the feed published falls under the delivery floor.
#[test]
fn the_service_sweep_rates_each_leaf_against_what_the_feed_published() {
    let ring = Arc::new(RingBuffer::new(64));
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
    let mut backend = SinkShardBackend::new(
        feed,
        WorkBudgetConfig::new(64, 1 << 20, Duration::from_millis(50)),
    );
    let service = |backend: &SinkShardBackend| {
        let mut metrics = ShardMetrics::default();
        backend.observe_metrics(&mut metrics);
        metrics.service
    };
    backend.on_command(EgressCommand::Add(output_spec(
        "out-sink",
        Arc::new(AtomicU64::new(0)),
    )));
    // A new leaf joins at the newest keyframe: let it join before the window.
    push_video_packet(&ring);
    backend.drain_ready_leaves();
    let window = crate::egress::delivery::DELIVERY_WINDOW;
    let start = Instant::now();
    backend.sweep_service(start);

    for _ in 0..10 {
        push_video_packet(&ring);
    }
    backend.on_command(EgressCommand::FeedWake);
    backend.drain_ready_leaves();
    backend.sweep_service(start + window);
    let kept_up = service(&backend);
    assert_eq!((kept_up.rated, kept_up.under_floor), (1, 0));
    assert_eq!(kept_up.offered_bps, 50.0 * 8.0 / window.as_secs_f64());
    assert_eq!(kept_up.delivered_bps, kept_up.offered_bps);

    // Published, never consumed: a starved leaf.
    for _ in 0..10 {
        push_video_packet(&ring);
    }
    backend.sweep_service(start + 2 * window);
    let starved = service(&backend);
    assert_eq!((starved.rated, starved.under_floor), (1, 1));
    assert_eq!(starved.delivered_bps, 0.0);
}

/// One sweep rates at most 256 leaves, so a tick's cost stays bounded; a
/// rotation over more spans sweeps, and the published summary changes only
/// when the rotation that rated every leaf completes.
#[test]
fn a_sweep_rotation_longer_than_one_budget_publishes_when_it_completes() {
    let ring = Arc::new(RingBuffer::new(64));
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
    let mut backend = SinkShardBackend::new(
        feed,
        WorkBudgetConfig::new(64, 1 << 20, Duration::from_millis(50)),
    );
    for i in 0..300 {
        backend.on_command(EgressCommand::Add(output_spec(
            &format!("out-{i}"),
            Arc::new(AtomicU64::new(0)),
        )));
    }
    let published = |backend: &SinkShardBackend| {
        let mut metrics = ShardMetrics::default();
        backend.observe_metrics(&mut metrics);
        (metrics.service.visited, metrics.service.rated)
    };
    let second = Duration::from_secs(1);
    let window = crate::egress::delivery::DELIVERY_WINDOW;
    let start = Instant::now();
    // Rotation 1 marks every leaf: 256, then the other 44.
    backend.sweep_service(start);
    assert_eq!(published(&backend), (0, 0), "nothing until a rotation ends");
    backend.sweep_service(start + second);
    assert_eq!(published(&backend), (300, 0), "marked, no window closed");

    push_video_packet(&ring);
    // Rotation 2 closes every leaf's window; halfway, rotation 1 stands.
    backend.sweep_service(start + window + second);
    assert_eq!(published(&backend), (300, 0));
    backend.sweep_service(start + window + 2 * second);
    assert_eq!(published(&backend), (300, 300));
}

/// Replacing and removing outputs takes their keys out of the sweep
/// rotation, so its storage tracks the live leaves however much they churn
/// between sweeps.
#[test]
fn the_sweep_rotation_holds_only_live_leaves_under_churn() {
    let ring = Arc::new(RingBuffer::new(8));
    let feed = RingFeed::new(ring, Arc::new(FeedEpoch::new()));
    let mut backend = SinkShardBackend::new(
        feed,
        WorkBudgetConfig::new(64, 1 << 20, Duration::from_millis(50)),
    );
    let add = |backend: &mut SinkShardBackend, id: &str| {
        backend.on_command(EgressCommand::Add(output_spec(
            id,
            Arc::new(AtomicU64::new(0)),
        )));
    };
    add(&mut backend, "steady");
    for _ in 0..1000 {
        // A re-Add replaces the leaf under a new key.
        add(&mut backend, "steady");
        add(&mut backend, "transient");
        backend.on_command(EgressCommand::Remove(OutputId::new("transient")));
    }
    assert_eq!(backend.leaves.len(), 1);
    assert_eq!(backend.sweep.rotation_len(), 1);
}
