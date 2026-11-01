use super::*;
use std::num::NonZeroU32;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::egress::command::{EgressCommand, FeedId, OutputId, ProtocolSpec, ShardId};
use crate::egress::journal::{FeedEpoch, TsFeed};
use crate::egress::manager::{EgressManagerConfig, ManagerCommandOutcome};
use crate::egress::runtime::EgressFabricRuntime;
use crate::egress::shard::{EgressShardBackend, EgressShardCommandEffect, EgressShardGroup};
use crate::test_support::{output_spec, shard_config};
use crate::ts_chunk_ring::TsChunkRing;
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
            EgressCommand::Abandon(output_id) => format!("abandon:{output_id}"),
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
        .retain_srt_fabric_runtime(feed_id.clone(), &feed, &ts_ring.ring)
        .await;
    let second = engine
        .retain_srt_fabric_runtime(feed_id.clone(), &feed, &ts_ring.ring)
        .await;

    assert_eq!(first, Ok(true));
    assert_eq!(second, Ok(false));
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

#[tokio::test]
async fn srt_fabric_registry_shutdown_helper_removes_and_joins_retained_runtime() {
    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-native-srt-helper");
    let ts_ring = TsChunkRing::new(8, CancellationToken::new());
    let feed = TsFeed::new(&ts_ring, Arc::new(FeedEpoch::new()));

    assert_eq!(
        engine
            .retain_srt_fabric_runtime(feed_id.clone(), &feed, &ts_ring.ring)
            .await,
        Ok(true)
    );
    let snapshots = engine.shutdown_srt_fabric_runtime(&feed_id).await.unwrap();
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.stopped && !snapshot.panicked)
    );
    assert!(
        engine
            .srt_fabric_runtime_snapshots(&feed_id)
            .await
            .is_none()
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
    use crate::egress::journal::RingFeed;

    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-native-rtmp");
    let feed = RingFeed::new(
        Arc::new(restream_sync::RingBuffer::new(8)),
        Arc::new(FeedEpoch::new()),
    );

    let first = engine
        .retain_rtmp_fabric_runtime(feed_id.clone(), &feed)
        .await;
    let second = engine
        .retain_rtmp_fabric_runtime(feed_id.clone(), &feed)
        .await;

    assert_eq!(first, Ok(true));
    assert_eq!(second, Ok(false));
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
    use crate::egress::backends::rtmp::RtmpPublishStartup;
    use crate::egress::backends::rtmp_shard::RtmpPublishStartupSource;
    use crate::egress::journal::RingFeed;

    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-rtmp-startup");
    let feed = RingFeed::new(
        Arc::new(restream_sync::RingBuffer::new(8)),
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
    use crate::egress::journal::RingFeed;

    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-sink");
    let feed = RingFeed::new(
        Arc::new(restream_sync::RingBuffer::new(8)),
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
    assert_eq!(snapshots.map(|snapshots| snapshots.len()), Some(1));
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
/// code uses actually routes onto a shard OS thread and discards real feed
/// units there.
#[tokio::test]
async fn sink_fabric_registry_dispatches_add_and_the_shard_discards_published_units() {
    use crate::egress::journal::RingFeed;
    use crate::egress::leaf::EgressProgressSink;
    use restream_core::{MediaPacket, MediaType, PayloadFormat};
    use std::sync::atomic::{AtomicU64, Ordering};

    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-sink-dispatch");
    let ring = Arc::new(restream_sync::RingBuffer::new(8));
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

/// A sink pool at its shard ceiling whose leaf tables are full refuses the
/// next Add with an operator-visible reason, before anything is dispatched,
/// instead of the shard dropping the leaf as an unexpected termination.
#[tokio::test]
async fn a_sink_add_past_a_full_leaf_table_is_refused() {
    use crate::egress::journal::RingFeed;
    use crate::egress::sizing::Saturated;

    let engine = MediaEngine::new_with_config(Arc::new(crate::config::EngineConfig {
        egress_fabric: crate::config::EgressFabricConfig {
            shards: 1,
            max_leaves_per_shard: 4,
            ..crate::config::EgressFabricConfig::default()
        },
        ..crate::config::EngineConfig::default()
    }));
    let feed_id = FeedId::new("feed-engine-sink-admission");
    let ring = Arc::new(restream_sync::RingBuffer::new(8));
    let feed = RingFeed::new(ring, Arc::new(FeedEpoch::new()));
    engine
        .retain_sink_fabric_runtime(feed_id.clone(), &feed)
        .await
        .unwrap();
    for i in 0..4 {
        let spec = output_spec(&format!("out-sink-{i}"), &feed_id);
        if let Err(error) = engine
            .dispatch_sink_fabric_command(&feed_id, EgressCommand::Add(spec))
            .await
        {
            panic!("output {i} refused with a free leaf: {error:?}");
        }
    }
    let refused = engine
        .dispatch_sink_fabric_command(
            &feed_id,
            EgressCommand::Add(output_spec("out-sink-4", &feed_id)),
        )
        .await
        .unwrap_err();
    assert_eq!(
        refused.refused(),
        Some(&Saturated::Full {
            leaves: 4,
            shards: 1
        })
    );

    engine.release_sink_fabric_runtime(&feed_id).await;
}

/// On a quiet feed the reconciler's `resize_sink_fabrics` must still replace
/// a panicked sink shard, end the outputs it held for their application
/// tasks, and run the retried output on the replacement.
#[tokio::test]
async fn reconciler_recovery_replaces_a_panicked_sink_shard() {
    use crate::egress::backends::sink_shard::INJECTED_SHARD_PANIC_OUTPUT;
    use crate::egress::journal::RingFeed;
    use crate::egress::leaf::EgressProgressSink;
    use restream_core::{MediaPacket, MediaType, PayloadFormat};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-sink-recover");
    let ring = Arc::new(restream_sync::RingBuffer::new(8));
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
    engine
        .retain_sink_fabric_runtime(feed_id.clone(), &feed)
        .await
        .unwrap();
    let ended = Arc::new(AtomicBool::new(false));
    let bytes_sent = Arc::new(AtomicU64::new(0));
    let mut spec = output_spec("out-sink-recover", &feed_id);
    spec.progress = EgressProgressSink {
        bytes_sent: Some(bytes_sent.clone()),
        terminated_unexpectedly: Some(ended.clone()),
        ..Default::default()
    };
    engine
        .dispatch_sink_fabric_command(&feed_id, EgressCommand::Add(spec.clone()))
        .await
        .unwrap();
    engine
        .dispatch_sink_fabric_command(
            &feed_id,
            EgressCommand::Add(output_spec(INJECTED_SHARD_PANIC_OUTPUT, &feed_id)),
        )
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !engine
        .sink_fabric_runtime_snapshots(&feed_id)
        .await
        .unwrap()
        .iter()
        .any(|snapshot| snapshot.panicked)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the shard never panicked"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    engine.resize_sink_fabrics().await;
    assert!(
        ended.load(Ordering::Acquire),
        "the output's task must see it end, or it never retries"
    );
    assert!(
        engine
            .sink_fabric_runtime_snapshots(&feed_id)
            .await
            .unwrap()
            .iter()
            .all(|snapshot| !snapshot.panicked && !snapshot.stopped)
    );

    // The application task's retry runs on the replacement.
    spec.generation = 2;
    engine
        .dispatch_sink_fabric_command(&feed_id, EgressCommand::Add(spec))
        .await
        .unwrap();
    ring.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: bytes::Bytes::from_static(b"abcde"),
    });
    while bytes_sent.load(Ordering::Relaxed) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the retried output never ran on the replacement"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    engine.release_sink_fabric_runtime(&feed_id).await;
}

#[tokio::test]
async fn pipeline_fabric_registry_retains_runtime_once_per_feed() {
    use crate::egress::journal::RingFeed;

    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-pipeline");
    let feed = RingFeed::new(
        Arc::new(restream_sync::RingBuffer::new(8)),
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
    assert_eq!(snapshots.map(|snapshots| snapshots.len()), Some(1));
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
/// ring on a real shard thread.
#[tokio::test]
async fn pipeline_fabric_registry_dispatches_add_and_the_shard_publishes_into_the_target_ring() {
    use crate::egress::backends::pipeline::PipelineTarget;
    use crate::egress::journal::RingFeed;
    use crate::egress::leaf::EgressProgressSink;
    use crate::engine::IngestRegistration;
    use crate::input_gate::InputPacketGate;
    use restream_core::{MediaPacket, MediaType, PayloadFormat};
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
    use tokio_util::sync::CancellationToken;

    // Explicit config, not `MediaEngine::new()`'s `AppConfig::from_env()`.
    // Config tests temporarily overlay `RESTREAM_EGRESS_*` (including
    // `RESTREAM_EGRESS_SHARDS`) under a process-wide env lock this test
    // does not hold. Reading env here raced that window and could spawn a
    // huge shard pool, then shrink-join it on the first `Add`, so the
    // publish wait expired before the leaf ever visited. Same class of
    // flake as the earlier SRT multi-feed shard-pool regression.
    // `shards: 1` also matches `target_egress_fabric_shards(OutputCount, 0, _)`,
    // so the first Add does not shrink the pool.
    let engine = MediaEngine::new_with_config(Arc::new(crate::config::EngineConfig {
        egress_fabric: crate::config::EgressFabricConfig {
            shards: 1,
            ..crate::config::EgressFabricConfig::default()
        },
        ..crate::config::EngineConfig::default()
    }));
    let feed_id = FeedId::new("feed-engine-pipeline-dispatch");
    let source_ring = Arc::new(restream_sync::RingBuffer::new(8));
    let feed = RingFeed::new(source_ring.clone(), Arc::new(FeedEpoch::new()));
    let target_ring = Arc::new(restream_sync::RingBuffer::new(8));

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

    let commands_before = engine
        .pipeline_fabric_runtime_snapshots(&feed_id)
        .await
        .map(|snapshots| snapshots.iter().map(|s| s.commands_processed).sum::<u64>())
        .unwrap_or(0);
    engine
        .dispatch_pipeline_fabric_command(&feed_id, EgressCommand::Add(spec))
        .await
        .unwrap();
    // The retain-time FeedWake is consumed before Add. A unit pushed
    // before the shard has processed Add is a lost wake: the leaf is
    // not there yet, and nothing will republish. Wait for this Add to
    // increment `commands_processed`, then push so the watcher notifies
    // an installed leaf.
    let add_deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let processed = engine
            .pipeline_fabric_runtime_snapshots(&feed_id)
            .await
            .map(|snapshots| snapshots.iter().map(|s| s.commands_processed).sum::<u64>())
            .unwrap_or(0);
        if processed > commands_before {
            break;
        }
        assert!(
            std::time::Instant::now() < add_deadline,
            "pipeline shard never processed Add"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

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

/// A pipeline pool whose leaf tables are full refuses the next Add before
/// dispatching it, and releases the target the application claimed for it:
/// nothing keeps the target ring (or its input registration) alive.
#[tokio::test]
async fn a_pipeline_add_past_a_full_leaf_table_is_refused_and_releases_its_target() {
    use crate::egress::backends::pipeline::PipelineTarget;
    use crate::egress::command::ProtocolSpec;
    use crate::egress::journal::RingFeed;
    use crate::egress::sizing::Saturated;
    use crate::engine::IngestRegistration;
    use crate::input_gate::InputPacketGate;
    use std::sync::atomic::AtomicI64;
    use tokio_util::sync::CancellationToken;

    let engine = MediaEngine::new_with_config(Arc::new(crate::config::EngineConfig {
        egress_fabric: crate::config::EgressFabricConfig {
            shards: 1,
            max_leaves_per_shard: 1,
            ..crate::config::EgressFabricConfig::default()
        },
        ..crate::config::EngineConfig::default()
    }));
    let feed_id = FeedId::new("feed-engine-pipeline-admission");
    let ring = Arc::new(restream_sync::RingBuffer::new(8));
    let feed = RingFeed::new(ring, Arc::new(FeedEpoch::new()));
    engine
        .retain_pipeline_fabric_runtime(feed_id.clone(), &feed)
        .await
        .unwrap();
    let claim = |id: &str| {
        let mut spec = output_spec(id, &feed_id);
        spec.protocol = ProtocolSpec::Pipeline {
            target_pipeline_id: format!("target-{id}"),
            target_input_id: "input".to_string(),
        };
        let target_ring = Arc::new(restream_sync::RingBuffer::new(8));
        let target = PipelineTarget {
            target_ring: target_ring.clone(),
            input_registration: IngestRegistration {
                cancel_token: CancellationToken::new(),
                attempt_id: 1,
                input_id: "input".to_string(),
                gate: Arc::new(InputPacketGate::active()),
                last_forwarded_dts: Arc::new(AtomicI64::new(i64::MIN)),
                preview_ring: Arc::new(arc_swap::ArcSwapOption::empty()),
            },
        };
        (spec, target, target_ring)
    };
    let (first, first_target, _first_ring) = claim("out-pipeline-0");
    assert!(
        engine
            .set_pipeline_target(&feed_id, first.id.clone(), first_target)
            .await
    );
    engine
        .dispatch_pipeline_fabric_command(&feed_id, EgressCommand::Add(first))
        .await
        .unwrap();

    let (second, second_target, second_ring) = claim("out-pipeline-1");
    assert!(
        engine
            .set_pipeline_target(&feed_id, second.id.clone(), second_target)
            .await
    );
    let refused = engine
        .dispatch_pipeline_fabric_command(&feed_id, EgressCommand::Add(second))
        .await
        .unwrap_err();
    assert_eq!(
        refused.refused(),
        Some(&Saturated::Full {
            leaves: 1,
            shards: 1
        })
    );
    assert_eq!(
        Arc::strong_count(&second_ring),
        1,
        "the refused output's target is released"
    );

    engine.release_pipeline_fabric_runtime(&feed_id).await;
}

/// An HLS PUT pool whose leaf tables are full refuses the next Add before
/// dispatching it, with a reason an operator can act on, instead of the
/// shard dropping the leaf as an unexpected termination.
#[tokio::test]
async fn an_hls_put_add_past_a_full_leaf_table_is_refused() {
    use crate::egress::sizing::Saturated;

    let engine = MediaEngine::new_with_config(Arc::new(crate::config::EngineConfig {
        egress_fabric: crate::config::EgressFabricConfig {
            shards: 1,
            max_leaves_per_shard: 1,
            ..crate::config::EgressFabricConfig::default()
        },
        ..crate::config::EngineConfig::default()
    }));
    let feed_id = crate::engine_hls_egress_fabric::hls_put_feed_id("pipe-hls-admission");
    let store = Arc::new(crate::hls::HlsStore::new());
    engine
        .retain_hls_put_fabric_runtime(feed_id.clone(), &store)
        .await
        .unwrap();
    let hls = |id: &str| {
        let mut spec = output_spec(id, &feed_id);
        spec.protocol = ProtocolSpec::HlsPut {
            url: format!("http://127.0.0.1:9/{id}/out.m3u8"),
        };
        spec
    };
    engine
        .dispatch_hls_put_fabric_command(&feed_id, EgressCommand::Add(hls("out-hls-0")))
        .await
        .unwrap();
    let refused = engine
        .dispatch_hls_put_fabric_command(&feed_id, EgressCommand::Add(hls("out-hls-1")))
        .await
        .unwrap_err();
    assert_eq!(
        refused.refused(),
        Some(&Saturated::Full {
            leaves: 1,
            shards: 1
        })
    );

    engine.release_hls_put_fabric_runtime(&feed_id).await;
}
