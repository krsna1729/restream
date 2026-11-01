use super::*;
use crate::egress::command::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::egress::journal::FeedEpoch;
use crate::egress::leaf::EgressProgressSink;
use crate::egress::policy::{LeafPolicy, WorkBudgetConfig};
use crate::egress::shard::{EgressShardConfig, EgressShardHandle};
use crate::engine::IngestRegistration;
use crate::input_gate::InputPacketGate;
use restream_core::{MediaPacket, MediaType, PayloadFormat};
use restream_sync::RingBuffer;
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
        crate::egress::command::ShardId::new(0),
        config(),
        PipelineShardBackend::new(
            feed,
            WorkBudgetConfig::new(8, 4096, Duration::from_millis(50)),
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
        crate::egress::command::ShardId::new(0),
        config(),
        PipelineShardBackend::new(
            feed,
            WorkBudgetConfig::new(8, 4096, Duration::from_millis(50)),
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

fn backend_with_targets(
    ids: &[&str],
) -> (
    Arc<RingBuffer>,
    PipelineShardBackend<SharedPipelineTargetSource>,
) {
    let ring = Arc::new(RingBuffer::new(64));
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
    let target_source = SharedPipelineTargetSource::new();
    for id in ids {
        target_source.set(
            OutputId::new(*id),
            PipelineTarget {
                target_ring: Arc::new(RingBuffer::new(64)),
                input_registration: registration(),
            },
        );
    }
    let backend = PipelineShardBackend::new(
        feed,
        WorkBudgetConfig::new(64, 1 << 20, Duration::from_millis(50)),
        target_source,
    );
    (ring, backend)
}

/// The fabric prices pipeline outputs from this sweep: over a closed
/// delivery window a leaf that forwards everything into its target is rated
/// at the feed's rate, and one that forwarded nothing the feed published
/// falls under the delivery floor.
#[test]
fn the_service_sweep_rates_what_each_leaf_forwarded_into_its_target() {
    let (ring, mut backend) = backend_with_targets(&["out-pipeline"]);
    let service = |backend: &PipelineShardBackend<SharedPipelineTargetSource>| {
        let mut metrics = crate::egress::metrics::ShardMetrics::default();
        backend.observe_metrics(&mut metrics);
        metrics.service
    };
    backend.on_command(EgressCommand::Add(output_spec(
        "out-pipeline",
        Arc::new(AtomicU64::new(0)),
    )));
    // A new leaf joins at the newest keyframe: let it join before the window.
    push_video_packet(&ring);
    backend.drain_ready_leaves();
    let window = crate::egress::delivery::DELIVERY_WINDOW;
    let start = std::time::Instant::now();
    backend.sweep_service(start);

    for _ in 0..10 {
        push_video_packet(&ring);
    }
    backend.on_command(EgressCommand::FeedWake);
    backend.drain_ready_leaves();
    backend.sweep_service(start + window);
    let kept_up = service(&backend);
    assert_eq!((kept_up.rated, kept_up.under_floor), (1, 0));
    assert_eq!(kept_up.delivered_bps, kept_up.offered_bps);

    // Published, never forwarded: a starved leaf.
    for _ in 0..10 {
        push_video_packet(&ring);
    }
    backend.sweep_service(start + 2 * window);
    let starved = service(&backend);
    assert_eq!((starved.rated, starved.under_floor), (1, 1));
    assert_eq!(starved.delivered_bps, 0.0);
}

/// Replacing and removing pipeline outputs takes their keys out of the sweep
/// rotation, so it tracks the live leaves however much they churn.
#[test]
fn the_sweep_rotation_holds_only_live_pipeline_leaves_under_churn() {
    let (_ring, mut backend) = backend_with_targets(&["steady", "transient"]);
    let add = |backend: &mut PipelineShardBackend<SharedPipelineTargetSource>, id: &str| {
        backend.on_command(EgressCommand::Add(output_spec(
            id,
            Arc::new(AtomicU64::new(0)),
        )));
    };
    add(&mut backend, "steady");
    for _ in 0..1000 {
        add(&mut backend, "steady");
        add(&mut backend, "transient");
        backend.on_command(EgressCommand::Remove(OutputId::new("transient")));
    }
    assert_eq!(backend.leaves.len(), 1);
    assert_eq!(backend.sweep.rotation_len(), 1);
}
