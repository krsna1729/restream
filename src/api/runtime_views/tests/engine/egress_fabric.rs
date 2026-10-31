use super::*;
use restream_engine::egress::command::{EgressCommand, FeedId};
use restream_engine::egress::journal::{FeedEpoch, TsFeed};
use restream_engine::egress::manager::EgressManagerConfig;
use restream_engine::egress::runtime::EgressFabricRuntime;
use restream_engine::egress::shard::{
    EgressShardBackend, EgressShardCommandEffect, EgressShardGroup,
};
use restream_engine::test_support::{output_spec, shard_config};
use restream_engine::ts_chunk_ring::TsChunkRing;
use std::num::NonZeroU32;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Exercise every production recovery entry point with a one-shard ceiling.
/// Periodic observation is still in its cooldown: panic recovery must not
/// depend on pool growth, a later sample, or another output command.
#[tokio::test]
async fn every_fabric_recovers_panics_without_adaptive_observation() {
    use restream_engine::egress::journal::RingFeed;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct PanicBackend;
    impl EgressShardBackend for PanicBackend {
        fn on_command(&mut self, _: EgressCommand) -> EgressShardCommandEffect {
            restream_engine::test_support::with_expected_panic_suppressed(|| {
                panic!("scripted shard panic")
            })
        }
    }

    let engine = MediaEngine::new();
    let id = FeedId::new("fixed-pool-recovery");
    let feed = RingFeed::new(
        Arc::new(restream_sync::RingBuffer::new(8)),
        Arc::new(FeedEpoch::new()),
    );
    let ts_ring = TsChunkRing::new(8, CancellationToken::new());
    let ts_feed = TsFeed::new(&ts_ring, Arc::new(FeedEpoch::new()));
    let store = Arc::new(restream_engine::hls::HlsStore::new());
    engine
        .retain_srt_fabric_runtime(id.clone(), &ts_feed, &ts_ring.ring)
        .await
        .unwrap();
    engine
        .retain_rtmp_fabric_runtime(id.clone(), &feed)
        .await
        .unwrap();
    engine
        .retain_moq_fabric_runtime(id.clone(), &feed)
        .await
        .unwrap();
    engine
        .retain_sink_fabric_runtime(id.clone(), &feed)
        .await
        .unwrap();
    engine
        .retain_pipeline_fabric_runtime(id.clone(), &feed)
        .await
        .unwrap();
    engine
        .retain_hls_put_fabric_runtime(id.clone(), &store)
        .await
        .unwrap();

    macro_rules! recover {
        ($registry:ident, $recover:ident) => {{
            let ended = Arc::new(AtomicBool::new(false));
            let mut spec = output_spec(stringify!($registry), &id);
            spec.progress.terminated_unexpectedly = Some(ended.clone());
            let group =
                EgressShardGroup::spawn(NonZeroU32::MIN, shard_config(), vec![PanicBackend])
                    .unwrap();
            let mut failed =
                EgressFabricRuntime::new(EgressManagerConfig::new(1, 16).unwrap(), group)
                    .unwrap()
                    .adaptive(1);
            failed
                .rescale(
                    restream_engine::config::EgressShardProfile::OutputCount,
                    restream_engine::host_capacity::HostCapacity { cpus: 1 },
                    restream_engine::egress::runtime::ResizeReason::Observe,
                    shard_config(),
                    |_| || Ok::<_, String>(PanicBackend),
                )
                .unwrap();
            assert!(!failed.observation_due());
            failed.dispatch(EgressCommand::Add(spec)).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !failed.has_panicked_shard() {
                assert!(std::time::Instant::now() < deadline);
                tokio::task::yield_now().await;
            }
            let old = engine
                .fabric
                .$registry
                .lock()
                .await
                .runtimes
                .insert(id.clone(), failed)
                .unwrap();
            old.shutdown();
            let before = test_health_snapshot(&engine, &[], &HashMap::new()).await;
            assert!(
                restream_health::alerts::derive_alerts(&before)
                    .iter()
                    .any(|alert| alert.id.ends_with(":panicked")),
                "{} panic must alert",
                stringify!($registry)
            );
            engine.$recover().await;
            let registry = engine.fabric.$registry.lock().await;
            let recovered = &registry.runtimes[&id];
            assert!(
                ended.load(Ordering::Acquire),
                "{} must release its outputs",
                stringify!($registry)
            );
            assert!(
                recovered
                    .snapshots()
                    .iter()
                    .all(|s| !s.panicked && !s.stopped)
            );
            drop(registry);
            let after = test_health_snapshot(&engine, &[], &HashMap::new()).await;
            assert!(
                !restream_health::alerts::derive_alerts(&after)
                    .iter()
                    .any(|alert| alert.id.ends_with(":panicked")),
                "{} recovery must clear its alert",
                stringify!($registry)
            );
        }};
    }
    recover!(srt, resize_srt_fabrics);
    recover!(rtmp, resize_rtmp_fabrics);
    recover!(moq, resize_moq_fabrics);
    recover!(sink, recover_sink_fabrics);
    recover!(pipeline, recover_pipeline_fabrics);
    recover!(hls_put, recover_hls_put_fabrics);
    engine.release_srt_fabric_runtime(&id).await;
    engine.release_rtmp_fabric_runtime(&id).await;
    engine.release_moq_fabric_runtime(&id).await;
    engine.release_sink_fabric_runtime(&id).await;
    engine.release_pipeline_fabric_runtime(&id).await;
    engine.release_hls_put_fabric_runtime(&id).await;
}
