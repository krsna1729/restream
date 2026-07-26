use super::*;
use crate::media::egress::command::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::FeedEpoch;
use crate::media::egress::leaf::EgressProgressSink;
use crate::media::egress::policy::LeafPolicy;
use crate::media::egress::shard::{EgressShardConfig, EgressShardHandle};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::RingBuffer;
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
/// SRT/RTMP use — the production wiring `start_sink_egress`'s plain
/// `tokio::spawn` task never exercised (`docs/egress-implementation.md`
/// Phase 4a status).
#[test]
fn sink_shard_backend_discards_a_real_unit_on_a_real_shard_thread() {
    let ring = Arc::new(RingBuffer::new(4));
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
    let handle = EgressShardHandle::spawn(
        crate::media::egress::command::ShardId::new(0),
        config(),
        SinkShardBackend::new(feed, WorkBudget::new(8, 4096, Duration::from_millis(50))),
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
        crate::media::egress::command::ShardId::new(0),
        config(),
        SinkShardBackend::new(feed, WorkBudget::new(8, 4096, Duration::from_millis(50))),
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
        crate::media::egress::command::ShardId::new(0),
        config(),
        SinkShardBackend::new(feed, WorkBudget::new(8, 4096, Duration::from_millis(50))),
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
