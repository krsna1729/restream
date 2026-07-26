use super::*;
use crate::media::egress::command::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::FeedEpoch;
use crate::media::egress::leaf::EgressProgressSink;
use crate::media::egress::policy::LeafPolicy;
use crate::media::egress::shard::{EgressShardConfig, EgressShardHandle};
use crate::media::engine::IngestRegistration;
use crate::media::input_gate::InputPacketGate;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::RingBuffer;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn config() -> EgressShardConfig {
    EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(5)).unwrap()
}

fn registration() -> IngestRegistration {
    IngestRegistration {
        cancel_token: CancellationToken::new(),
        attempt_id: 1,
        input_id: "recirculated-input".to_string(),
        gate: Arc::new(InputPacketGate::active()),
        last_forwarded_dts: Arc::new(AtomicI64::new(i64::MIN)),
        preview_ring: Arc::new(arc_swap::ArcSwapOption::empty()),
    }
}

fn output_spec(id: &str, bytes_sent: Arc<AtomicU64>) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation: 1,
        feed: FeedId::new("feed-pipeline"),
        protocol: ProtocolSpec::Pipeline {
            target_pipeline_id: "target".to_string(),
            target_input_id: "input".to_string(),
        },
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

/// Proves `PipelineShardBackend` publishes real feed units into the
/// claimed target ring on a real shard OS thread, through the same
/// `EgressCommand`/`EgressShardHandle` path SRT/RTMP/Sink use.
#[test]
fn pipeline_shard_backend_publishes_a_real_unit_into_the_target_ring_on_a_real_shard_thread() {
    let source_ring = Arc::new(RingBuffer::new(4));
    let feed = RingFeed::new(source_ring.clone(), Arc::new(FeedEpoch::new()));
    let target_ring = Arc::new(RingBuffer::new(4));
    let target_source = SharedPipelineTargetSource::new();
    target_source.set(
        OutputId::new("out-pipeline"),
        PipelineTarget {
            target_ring: target_ring.clone(),
            input_registration: registration(),
        },
    );

    let handle = EgressShardHandle::spawn(
        crate::media::egress::command::ShardId::new(0),
        config(),
        PipelineShardBackend::new(
            feed,
            WorkBudget::new(8, 4096, Duration::from_millis(50)),
            target_source,
        ),
    );

    let bytes_sent = Arc::new(AtomicU64::new(0));
    handle
        .try_send(EgressCommand::Add(output_spec(
            "out-pipeline",
            bytes_sent.clone(),
        )))
        .unwrap();

    // Let the leaf's initial (Add-time) enqueue run and go idle against an
    // empty feed *before* publishing — otherwise the packet can land before
    // the shard thread has even processed `Add`, and the very first visit
    // (not `FeedWake`) would pick it up, silently not exercising the path
    // this test exists to prove (`FeedWake` is the only readiness signal a
    // pipeline leaf has once idle — see the module doc).
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(bytes_sent.load(Ordering::Relaxed), 0);

    push_video_packet(&source_ring);
    handle.try_send(EgressCommand::FeedWake).unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while bytes_sent.load(Ordering::Relaxed) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "pipeline leaf never published the unit into the target ring"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(bytes_sent.load(Ordering::Relaxed), 5);
    assert_eq!(target_ring.read_at(0).unwrap().dts, 0);

    let snapshot = handle.shutdown_and_join();
    assert!(!snapshot.panicked);
}

/// An `Add` with no matching target (application layer never claimed one)
/// must be rejected, not panic or silently create a broken leaf.
#[test]
fn pipeline_shard_backend_rejects_add_with_no_claimed_target() {
    let source_ring = Arc::new(RingBuffer::new(4));
    let feed = RingFeed::new(source_ring, Arc::new(FeedEpoch::new()));
    let handle = EgressShardHandle::spawn(
        crate::media::egress::command::ShardId::new(0),
        config(),
        PipelineShardBackend::new(
            feed,
            WorkBudget::new(8, 4096, Duration::from_millis(50)),
            EmptyPipelineTargetSource,
        ),
    );

    let bytes_sent = Arc::new(AtomicU64::new(0));
    handle
        .try_send(EgressCommand::Add(output_spec("out-pipeline", bytes_sent)))
        .unwrap();
    std::thread::sleep(Duration::from_millis(50));

    let snapshot = handle.shutdown_and_join();
    assert!(!snapshot.panicked);
}
