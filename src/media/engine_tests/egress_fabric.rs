use super::*;
use std::num::NonZeroU32;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::media::egress::command::{
    EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec, ShardId,
};
use crate::media::egress::journal::{FeedEpoch, TsFeed};
use crate::media::egress::manager::{EgressManagerConfig, ManagerCommandOutcome};
use crate::media::egress::policy::LeafPolicy;
use crate::media::egress::runtime::EgressFabricRuntime;
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardCommandEffect, EgressShardConfig, EgressShardGroup,
};
use crate::media::ts_chunk_ring::TsChunkRing;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
struct ProbeState {
    commands: Vec<String>,
    shutdowns: u64,
}

#[derive(Clone, Debug, Default)]
struct Probe {
    inner: Arc<(Mutex<ProbeState>, Condvar)>,
}

impl Probe {
    fn wait_for_commands(&self, target: usize) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.commands.len() < target
            })
            .unwrap();
        assert!(result.0.commands.len() >= target);
    }

    fn state(&self) -> ProbeState {
        let state = self.inner.0.lock().unwrap();
        ProbeState {
            commands: state.commands.clone(),
            shutdowns: state.shutdowns,
        }
    }
}

#[derive(Debug)]
struct ProbeBackend {
    probe: Probe,
}

impl EgressShardBackend for ProbeBackend {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        let label = match command {
            EgressCommand::Add(spec) => format!("add:{}", spec.id),
            EgressCommand::Update(spec) => format!("update:{}", spec.id),
            EgressCommand::Remove(output_id) => format!("remove:{output_id}"),
            EgressCommand::FeedWake => "feed-wake".to_string(),
            EgressCommand::DrainShard(shard_id) => format!("drain:{shard_id}"),
            EgressCommand::Shutdown => "shutdown".to_string(),
        };
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.commands.push(label);
        condvar.notify_all();
        EgressShardCommandEffect::Continue
    }

    fn on_shutdown(&mut self) {
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.shutdowns = state.shutdowns.saturating_add(1);
        condvar.notify_all();
    }
}

fn shard_config() -> EgressShardConfig {
    EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(1)).unwrap()
}

fn runtime(probe: Probe) -> EgressFabricRuntime {
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(1).unwrap(),
        shard_config(),
        vec![ProbeBackend {
            probe: probe.clone(),
        }],
    )
    .unwrap();
    EgressFabricRuntime::new(EgressManagerConfig::new(1, 16).unwrap(), group).unwrap()
}

fn output_spec(id: &str, feed: &FeedId) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation: 1,
        feed: feed.clone(),
        protocol: ProtocolSpec::Sink,
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}

#[tokio::test]
async fn srt_fabric_registry_dispatches_to_feed_runtime() {
    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-srt");
    let probe = Probe::default();
    engine
        .insert_srt_fabric_runtime_for_test(feed_id.clone(), runtime(probe.clone()))
        .await;

    let outcome = engine
        .dispatch_srt_fabric_command(&feed_id, EgressCommand::Add(output_spec("out-1", &feed_id)))
        .await;

    assert_eq!(
        outcome,
        Ok(ManagerCommandOutcome::Enqueued {
            shard_id: ShardId::new(0)
        })
    );
    probe.wait_for_commands(1);
    assert_eq!(probe.state().commands, vec!["add:out-1"]);
}

#[tokio::test]
async fn srt_fabric_registry_retains_native_runtime_once_per_feed() {
    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-native-srt");
    let ts_ring = TsChunkRing::new(8, CancellationToken::new());
    let feed = TsFeed::new(&ts_ring, Arc::new(FeedEpoch::new()));

    let first = engine
        .retain_srt_fabric_runtime(feed_id.clone(), &feed, "pipeline-a")
        .await;
    let second = engine
        .retain_srt_fabric_runtime(feed_id.clone(), &feed, "pipeline-a")
        .await;
    let snapshots = engine.srt_fabric_runtime_snapshots(&feed_id).await;

    assert_eq!(first, Ok(true));
    assert_eq!(second, Ok(false));
    assert_eq!(
        snapshots.map(|snapshots| snapshots.len()),
        Some(engine.config.egress_fabric.shard_count().get() as usize)
    );
    assert!(!engine.release_srt_fabric_runtime(&feed_id).await);
    assert!(
        engine
            .srt_fabric_runtime_snapshots(&feed_id)
            .await
            .is_some()
    );
    assert!(engine.release_srt_fabric_runtime(&feed_id).await);
    assert!(
        engine
            .srt_fabric_runtime_snapshots(&feed_id)
            .await
            .is_none()
    );
}

/// End-to-end wiring proof for the libsrt egress-multiplexer scoping.
///
/// libsrt creates one `CMultiplexer` per bound local UDP port and gives each
/// one exactly one `CSndQueue` and one `CRcvQueue` worker thread
/// (`srtcore/api.cpp::updateMux` -> `srtcore/queue.cpp`). An engine-wide
/// port therefore put every SRT egress connection on one libsrt sender
/// thread, which is what saturated at ~120 concurrent egress outputs and
/// drove libsrt's TLPKTDROP to discard packets past their deadline. This
/// asserts the real `retain_srt_fabric_runtime` path claims one port per
/// `(pipeline, shard)`, that shard *N* is shared across feeds *of the same
/// pipeline* so the libsrt thread count tracks shard count rather than feed
/// count, and that a different pipeline's shard *N* never shares that
/// multiplexer (the cross-tenant isolation fix).
#[tokio::test]
async fn srt_fabric_runtime_claims_one_libsrt_muxer_port_per_shard_shared_across_feeds() {
    // Explicit config, not `MediaEngine::new()`'s `AppConfig::from_env()`:
    // the process environment is shared with the config tests, which
    // temporarily set `RESTREAM_SRT_EGRESS_MUXER_PORT_PIPELINE_SCOPED=false`
    // while asserting the override. Reading env here raced that window and
    // silently flipped this test's premise (both pipelines then share one
    // scope key, so the cross-tenant assertion below sees `shard_count`
    // entries instead of `shard_count * 2`). Mirrors the sibling
    // `..._shares_muxer_ports_across_pipelines_when_scoping_disabled`.
    let engine = MediaEngine::new_with_config(Arc::new(crate::AppConfig {
        srt_egress_muxer_port_pipeline_scoped: true,
        ..crate::AppConfig::default()
    }));
    assert!(
        engine.config.srt_egress_reuse_local_port,
        "this test covers the reuse-enabled default"
    );
    let shard_count = engine.config.egress_fabric.shard_count().get();
    assert!(shard_count > 1, "need more than one shard to be meaningful");
    let ts_ring = TsChunkRing::new(8, CancellationToken::new());
    let feed = TsFeed::new(&ts_ring, Arc::new(FeedEpoch::new()));
    let first_feed = FeedId::new("feed-engine-srt-muxer-source");
    let second_feed = FeedId::new("feed-engine-srt-muxer-720p");
    let other_pipeline_feed = FeedId::new("feed-engine-srt-muxer-other-pipeline");

    assert_eq!(
        engine
            .retain_srt_fabric_runtime(first_feed.clone(), &feed, "pipeline-a")
            .await,
        Ok(true)
    );
    let ports = engine.srt_egress_muxer_ports_handle();
    assert_eq!(
        ports.tracked_shards(),
        shard_count as usize,
        "one libsrt multiplexer port per shard"
    );

    // A second feed on the SAME pipeline reuses the same per-shard entries
    // rather than minting a second multiplexer per shard.
    assert_eq!(
        engine
            .retain_srt_fabric_runtime(second_feed.clone(), &feed, "pipeline-a")
            .await,
        Ok(true)
    );
    assert_eq!(
        ports.tracked_shards(),
        shard_count as usize,
        "shard N is shared across feeds of one pipeline, so libsrt thread count \
         tracks shard count within that pipeline"
    );

    // A feed on a DIFFERENT pipeline must mint its own multiplexers, even
    // for the same numeric shard ids -- this is the cross-tenant isolation
    // property `SrtEgressMuxerPorts` exists to provide.
    assert_eq!(
        engine
            .retain_srt_fabric_runtime(other_pipeline_feed.clone(), &feed, "pipeline-b")
            .await,
        Ok(true)
    );
    assert_eq!(
        ports.tracked_shards(),
        (shard_count * 2) as usize,
        "a different pipeline's shard N must not share pipeline-a's multiplexer"
    );

    // Distinct per shard; stable (reusable) within a shard.
    let states = (0..shard_count)
        .map(|index| ports.shard("pipeline-a", ShardId::new(index)))
        .collect::<Vec<_>>();
    for (index, state) in states.iter().enumerate() {
        assert!(Arc::ptr_eq(
            state,
            &ports.shard("pipeline-a", ShardId::new(index as u32))
        ));
        assert!(
            !Arc::ptr_eq(
                state,
                &ports.shard("pipeline-b", ShardId::new(index as u32))
            ),
            "pipeline-a and pipeline-b must never share a multiplexer for the same shard id"
        );
        for other in states.iter().skip(index + 1) {
            assert!(
                !Arc::ptr_eq(state, other),
                "shards must not share one libsrt multiplexer"
            );
        }
    }

    assert!(engine.release_srt_fabric_runtime(&first_feed).await);
    assert!(engine.release_srt_fabric_runtime(&second_feed).await);
    assert!(
        engine
            .release_srt_fabric_runtime(&other_pipeline_feed)
            .await
    );
    // Releasing a feed keeps the per-shard entries: a shard that comes back
    // reuses its previous port instead of stranding it.
    assert_eq!(ports.tracked_shards(), (shard_count * 2) as usize);
}

/// `srt_egress_muxer_port_pipeline_scoped = false` restores the
/// pre-2026-08-14 engine-wide-shared behavior: two different pipelines'
/// shard *N* collapse onto the same multiplexer entry, matching what
/// `srt_fabric_runtime_claims_one_libsrt_muxer_port_per_shard_shared_across_feeds`
/// proves for feeds *within* one pipeline. This is the explicit opt-out
/// path for operators who prefer the lower multiplexer/thread count over
/// cross-tenant isolation -- see `AppConfig::srt_egress_muxer_port_pipeline_scoped`.
#[tokio::test]
async fn srt_fabric_runtime_shares_muxer_ports_across_pipelines_when_scoping_disabled() {
    let engine = MediaEngine::new_with_config(Arc::new(crate::AppConfig {
        srt_egress_muxer_port_pipeline_scoped: false,
        ..crate::AppConfig::default()
    }));
    let shard_count = engine.config.egress_fabric.shard_count().get();
    let ts_ring = TsChunkRing::new(8, CancellationToken::new());
    let feed = TsFeed::new(&ts_ring, Arc::new(FeedEpoch::new()));
    let first_feed = FeedId::new("feed-engine-srt-muxer-scope-off-a");
    let other_pipeline_feed = FeedId::new("feed-engine-srt-muxer-scope-off-b");

    assert_eq!(
        engine
            .retain_srt_fabric_runtime(first_feed.clone(), &feed, "pipeline-a")
            .await,
        Ok(true)
    );
    let ports = engine.srt_egress_muxer_ports_handle();
    assert_eq!(ports.tracked_shards(), shard_count as usize);

    assert_eq!(
        engine
            .retain_srt_fabric_runtime(other_pipeline_feed.clone(), &feed, "pipeline-b")
            .await,
        Ok(true)
    );
    assert_eq!(
        ports.tracked_shards(),
        shard_count as usize,
        "with pipeline scoping disabled, a different pipeline's shard N \
         must share the same multiplexer entry, not mint its own"
    );
}

#[tokio::test]
async fn srt_fabric_registry_shutdown_helper_removes_and_joins_retained_runtime() {
    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-native-srt-helper");
    let ts_ring = TsChunkRing::new(8, CancellationToken::new());
    let feed = TsFeed::new(&ts_ring, Arc::new(FeedEpoch::new()));

    assert_eq!(
        engine
            .retain_srt_fabric_runtime(feed_id.clone(), &feed, "pipeline-a")
            .await,
        Ok(true)
    );
    assert_eq!(
        engine
            .shutdown_srt_fabric_runtime(&feed_id)
            .await
            .map(|snapshots| snapshots.len()),
        Some(engine.config.egress_fabric.shard_count().get() as usize)
    );
}

#[tokio::test]
async fn srt_fabric_registry_shutdown_removes_and_joins_feed_runtime() {
    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-srt-shutdown");
    let probe = Probe::default();
    engine
        .insert_srt_fabric_runtime_for_test(feed_id.clone(), runtime(probe.clone()))
        .await;

    let snapshots = engine.shutdown_srt_fabric_runtime(&feed_id).await;
    let missing = engine
        .dispatch_srt_fabric_command(&feed_id, EgressCommand::Add(output_spec("out-1", &feed_id)))
        .await;

    assert_eq!(snapshots.map(|snapshots| snapshots.len()), Some(1));
    assert_eq!(probe.state().shutdowns, 1);
    assert!(missing.is_err());
}

#[tokio::test]
async fn rtmp_fabric_registry_retains_native_runtime_once_per_feed() {
    use crate::media::egress::journal::RingFeed;

    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-native-rtmp");
    let feed = RingFeed::new(
        Arc::new(crate::media::ring_buffer::RingBuffer::new(8)),
        Arc::new(FeedEpoch::new()),
    );

    let first = engine
        .retain_rtmp_fabric_runtime(feed_id.clone(), &feed)
        .await;
    let second = engine
        .retain_rtmp_fabric_runtime(feed_id.clone(), &feed)
        .await;
    let snapshots = engine.rtmp_fabric_runtime_snapshots(&feed_id).await;

    assert_eq!(first, Ok(true));
    assert_eq!(second, Ok(false));
    assert_eq!(
        snapshots.map(|snapshots| snapshots.len()),
        Some(engine.config.egress_fabric.shard_count().get() as usize)
    );
    assert!(!engine.release_rtmp_fabric_runtime(&feed_id).await);
    assert!(
        engine
            .rtmp_fabric_runtime_snapshots(&feed_id)
            .await
            .is_some()
    );
    assert!(engine.release_rtmp_fabric_runtime(&feed_id).await);
    assert!(
        engine
            .rtmp_fabric_runtime_snapshots(&feed_id)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn rtmp_fabric_publish_startup_is_readable_by_the_shard_backend() {
    use crate::media::egress::backends::rtmp::RtmpPublishStartup;
    use crate::media::egress::backends::rtmp_shard::RtmpPublishStartupSource;
    use crate::media::egress::journal::RingFeed;

    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-rtmp-startup");
    let feed = RingFeed::new(
        Arc::new(crate::media::ring_buffer::RingBuffer::new(8)),
        Arc::new(FeedEpoch::new()),
    );

    engine
        .retain_rtmp_fabric_runtime(feed_id.clone(), &feed)
        .await
        .unwrap();

    let output_id = OutputId::new("out-1");
    let stored = engine
        .set_rtmp_publish_startup(&feed_id, output_id.clone(), RtmpPublishStartup::default())
        .await;
    assert!(stored, "startup must be stored against a live runtime");

    let mut source = {
        let registry = engine.fabric.rtmp.lock().await;
        registry.startup_sources.get(&feed_id).unwrap().clone()
    };
    assert!(
        source.take_startup(&output_id).is_some(),
        "the shard-side source must observe the startup written by the async caller"
    );

    engine.release_rtmp_fabric_runtime(&feed_id).await;
    let missing = engine
        .set_rtmp_publish_startup(&feed_id, output_id, RtmpPublishStartup::default())
        .await;
    assert!(!missing, "a released runtime must not accept new startups");
}

#[tokio::test]
async fn sink_fabric_registry_retains_runtime_once_per_feed() {
    use crate::media::egress::journal::RingFeed;

    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-sink");
    let feed = RingFeed::new(
        Arc::new(crate::media::ring_buffer::RingBuffer::new(8)),
        Arc::new(FeedEpoch::new()),
    );

    let first = engine
        .retain_sink_fabric_runtime(feed_id.clone(), &feed)
        .await;
    let second = engine
        .retain_sink_fabric_runtime(feed_id.clone(), &feed)
        .await;
    let snapshots = engine.sink_fabric_runtime_snapshots(&feed_id).await;

    assert_eq!(first, Ok(true));
    assert_eq!(second, Ok(false));
    assert_eq!(
        snapshots.map(|snapshots| snapshots.len()),
        Some(engine.config.egress_fabric.shard_count().get() as usize)
    );
    assert!(!engine.release_sink_fabric_runtime(&feed_id).await);
    assert!(
        engine
            .sink_fabric_runtime_snapshots(&feed_id)
            .await
            .is_some()
    );
    assert!(engine.release_sink_fabric_runtime(&feed_id).await);
    assert!(
        engine
            .sink_fabric_runtime_snapshots(&feed_id)
            .await
            .is_none()
    );
}

/// End-to-end proof that a `Sink` output added through the same
/// `EgressCommand::Add`/`dispatch_sink_fabric_command` path production
/// code uses actually discards real feed units on a real shard thread —
/// closing the Phase 4a gap (`docs/egress-implementation.md`): `Sink` used
/// to reach this registry's API surface without ever routing onto shard
/// OS threads.
#[tokio::test]
async fn sink_fabric_registry_dispatches_add_and_the_shard_discards_published_units() {
    use crate::media::egress::journal::RingFeed;
    use crate::media::egress::leaf::EgressProgressSink;
    use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
    use std::sync::atomic::{AtomicU64, Ordering};

    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-sink-dispatch");
    let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(8));
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));

    engine
        .retain_sink_fabric_runtime(feed_id.clone(), &feed)
        .await
        .unwrap();

    let bytes_sent = Arc::new(AtomicU64::new(0));
    let mut spec = output_spec("out-sink-1", &feed_id);
    spec.progress = EgressProgressSink {
        bytes_sent: Some(bytes_sent.clone()),
        ..Default::default()
    };
    engine
        .dispatch_sink_fabric_command(&feed_id, EgressCommand::Add(spec))
        .await
        .unwrap();

    // `retain_sink_fabric_runtime` already spawned a wake watcher for this
    // feed (mirroring `retain_srt_fabric_runtime`/`retain_rtmp_fabric_runtime`)
    // that delivers `EgressCommand::FeedWake` to every shard on publish —
    // `EgressManager::dispatch` treats a manually-dispatched `FeedWake` as a
    // no-op (`ManagerCommandOutcome::Ignored`), so the push below must go
    // through the same production path the watcher observes, not a second
    // manual dispatch.
    ring.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: bytes::Bytes::from_static(b"abcde"),
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while bytes_sent.load(Ordering::Relaxed) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "sink shard never discarded the published unit"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(bytes_sent.load(Ordering::Relaxed), 5);

    engine.release_sink_fabric_runtime(&feed_id).await;
}

#[tokio::test]
async fn pipeline_fabric_registry_retains_runtime_once_per_feed() {
    use crate::media::egress::journal::RingFeed;

    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-pipeline");
    let feed = RingFeed::new(
        Arc::new(crate::media::ring_buffer::RingBuffer::new(8)),
        Arc::new(FeedEpoch::new()),
    );

    let first = engine
        .retain_pipeline_fabric_runtime(feed_id.clone(), &feed)
        .await;
    let second = engine
        .retain_pipeline_fabric_runtime(feed_id.clone(), &feed)
        .await;
    let snapshots = engine.pipeline_fabric_runtime_snapshots(&feed_id).await;

    assert_eq!(first, Ok(true));
    assert_eq!(second, Ok(false));
    assert_eq!(
        snapshots.map(|snapshots| snapshots.len()),
        Some(engine.config.egress_fabric.shard_count().get() as usize)
    );
    assert!(!engine.release_pipeline_fabric_runtime(&feed_id).await);
    assert!(
        engine
            .pipeline_fabric_runtime_snapshots(&feed_id)
            .await
            .is_some()
    );
    assert!(engine.release_pipeline_fabric_runtime(&feed_id).await);
    assert!(
        engine
            .pipeline_fabric_runtime_snapshots(&feed_id)
            .await
            .is_none()
    );
}

/// End-to-end proof that a `Pipeline` output added through the same
/// `EgressCommand::Add`/`dispatch_pipeline_fabric_command` path production
/// code uses actually publishes real feed units into the claimed target
/// ring on a real shard thread — closing the Phase 6a gap
/// (`docs/egress-implementation.md`): recirculation used to run on a
/// plain per-output `tokio::spawn` task, never the fabric.
#[tokio::test]
async fn pipeline_fabric_registry_dispatches_add_and_the_shard_publishes_into_the_target_ring() {
    use crate::media::egress::backends::pipeline::PipelineTarget;
    use crate::media::egress::journal::RingFeed;
    use crate::media::egress::leaf::EgressProgressSink;
    use crate::media::engine::IngestRegistration;
    use crate::media::input_gate::InputPacketGate;
    use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
    use tokio_util::sync::CancellationToken;

    // Explicit config, not `MediaEngine::new()`'s `AppConfig::from_env()`.
    // Config tests temporarily overlay `RESTREAM_EGRESS_*` (including
    // `RESTREAM_EGRESS_SHARDS`) under a process-wide env lock this test
    // does not hold. Reading env here raced that window and could spawn a
    // huge shard pool, then shrink-join it on the first `Add`, so the
    // publish wait expired before the leaf ever visited. Same class of
    // flake as `srt_fabric_runtime_claims_one_libsrt_muxer_port_per_shard_shared_across_feeds`.
    // `shards: 1` also matches `target_egress_fabric_shards(OutputCount, 0, _)`,
    // so the first Add does not shrink the pool.
    let engine = MediaEngine::new_with_config(Arc::new(crate::AppConfig {
        egress_fabric: crate::config::EgressFabricConfig {
            shards: 1,
            ..crate::config::EgressFabricConfig::default()
        },
        ..crate::AppConfig::default()
    }));
    let feed_id = FeedId::new("feed-engine-pipeline-dispatch");
    let source_ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(8));
    let feed = RingFeed::new(source_ring.clone(), Arc::new(FeedEpoch::new()));
    let target_ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(8));

    engine
        .retain_pipeline_fabric_runtime(feed_id.clone(), &feed)
        .await
        .unwrap();
    // Let the current-thread runtime poll the wake watcher once so its
    // first-iteration FeedWake is delivered before we Add and publish.
    tokio::task::yield_now().await;

    let bytes_sent = Arc::new(AtomicU64::new(0));
    let mut spec = output_spec("out-pipeline-1", &feed_id);
    spec.protocol = ProtocolSpec::Pipeline {
        target_pipeline_id: "target-pipeline".to_string(),
        target_input_id: "target-input".to_string(),
    };
    spec.progress = EgressProgressSink {
        bytes_sent: Some(bytes_sent.clone()),
        ..Default::default()
    };

    let registration = IngestRegistration {
        cancel_token: CancellationToken::new(),
        attempt_id: 1,
        input_id: "target-input".to_string(),
        gate: Arc::new(InputPacketGate::active()),
        last_forwarded_dts: Arc::new(AtomicI64::new(i64::MIN)),
        preview_ring: Arc::new(arc_swap::ArcSwapOption::empty()),
    };
    let set = engine
        .set_pipeline_target(
            &feed_id,
            spec.id.clone(),
            PipelineTarget {
                target_ring: target_ring.clone(),
                input_registration: registration,
            },
        )
        .await;
    assert!(set, "target must be recorded against a live runtime");

    engine
        .dispatch_pipeline_fabric_command(&feed_id, EgressCommand::Add(spec))
        .await
        .unwrap();
    // Give the shard thread a chance to install the leaf before the
    // first unit lands. Under a loaded libtest host the publish wait
    // has expired with the leaf not yet visiting (same flake class as
    // the env-lock / shard-pool note above).
    tokio::task::yield_now().await;

    source_ring.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: bytes::Bytes::from_static(b"abcde"),
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while bytes_sent.load(Ordering::Relaxed) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "pipeline shard never published the unit into the target ring"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(bytes_sent.load(Ordering::Relaxed), 5);
    assert_eq!(target_ring.read_at(0).unwrap().dts, 0);

    engine.release_pipeline_fabric_runtime(&feed_id).await;
}
