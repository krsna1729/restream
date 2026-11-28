use super::*;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::egress::backends::sink_shard::SinkShardBackend;
use crate::egress::command::{EgressCommand, FeedId};
use crate::egress::journal::{FeedEpoch, RingFeed};
use crate::egress::manager::{EgressManagerConfig, PlacementPolicy};
use crate::egress::runtime::ResizeReason;
use crate::egress::shard::EgressShardHealth;
use crate::test_support::{output_spec, shard_config, test_video_packet};
use restream_sync::RingBuffer;

struct Fixture {
    runtime: SharedFabricRuntime,
    feed: RingFeed,
    id: FeedId,
    _wakes: FeedWakeSubscription,
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let id = FeedId::new(name);
        let feed = RingFeed::new(Arc::new(RingBuffer::new(16)), Arc::new(FeedEpoch::new()));
        let source = feed.clone_reader();
        let runtime = tokio::task::spawn_blocking(move || {
            let config = shard_config().with_leaf_capacity(1);
            let budget = crate::egress::policy::WorkBudgetConfig::from_fabric(
                &crate::config::EgressFabricConfig::default(),
            );
            let group = EgressShardGroup::spawn_with(NonZeroU32::MIN, config, |_| {
                let source = source.clone_reader();
                move || SinkShardBackend::new(source, budget).with_leaf_capacity(1)
            })
            .unwrap();
            SharedFabricRuntime::new(
                EgressFabricRuntime::new(
                    EgressManagerConfig::new(1, 16)
                        .unwrap()
                        .with_placement_policy(PlacementPolicy::LeastLoaded),
                    group,
                    2,
                )
                .unwrap(),
            )
        })
        .await
        .unwrap();
        let wakes = runtime.subscribe_feed_wakes(
            "owned-test",
            id.clone(),
            feed.publication_ring().publication_subscribers(),
        );
        Self {
            runtime,
            feed,
            id,
            _wakes: wakes,
        }
    }

    async fn add(&self) -> Arc<AtomicU64> {
        let bytes = Arc::new(AtomicU64::new(0));
        let mut spec = output_spec("consumer", &self.id);
        spec.progress.bytes_sent = Some(Arc::clone(&bytes));
        self.runtime
            .with(move |runtime| runtime.dispatch(EgressCommand::Add(spec)))
            .await
            .unwrap()
            .unwrap();
        bytes
    }

    async fn deliver(&self, bytes: &AtomicU64, sequence: i64) {
        self.feed
            .publication_ring()
            .push(test_video_packet(sequence, sequence, true));
        tokio::time::timeout(Duration::from_secs(2), async {
            while bytes.load(Ordering::Acquire) != (sequence as u64 + 1) * 5 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("the real sink must consume every published payload");
    }
}

struct Release(Option<std::sync::mpsc::Sender<()>>);

impl Drop for Release {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

/// Two blocking workers suffice even with many jobs queued behind a held feed.
/// The factory and backend below are the actual shard construction/media path.
#[test]
fn held_growth_keeps_media_health_and_other_feeds_live_after_caller_cancel() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .unwrap();
    executor.block_on(async {
        let growing = Fixture::new("growing").await;
        let healthy = Fixture::new("healthy").await;
        let growing_bytes = growing.add().await;
        growing.deliver(&growing_bytes, 0).await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = Release(Some(release_tx));
        let runtime = growing.runtime.clone();
        let feed = growing.feed.clone_reader();
        let job = tokio::spawn(async move {
            runtime
                .with(move |runtime| {
                    let mut entered_tx = Some(entered_tx);
                    let mut release_rx = Some(release_rx);
                    runtime.rescale(
                        crate::config::EgressShardProfile::SinkOutputCount,
                        crate::host_capacity::HostCapacity { cpus: 8 },
                        ResizeReason::Add,
                        shard_config().with_leaf_capacity(1),
                        |_| {
                            let entered_tx = entered_tx.take().unwrap();
                            let release_rx = release_rx.take().unwrap();
                            let feed = feed.clone_reader();
                            move || {
                                let _ = entered_tx.send(());
                                release_rx
                                    .recv_timeout(Duration::from_secs(5))
                                    .map_err(|error| error.to_string())?;
                                Ok::<_, String>(
                                    SinkShardBackend::new(
                                        feed,
                                        crate::egress::policy::WorkBudgetConfig::from_fabric(
                                            &crate::config::EgressFabricConfig::default(),
                                        ),
                                    )
                                    .with_leaf_capacity(1),
                                )
                            }
                        },
                    )
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), entered_rx)
            .await
            .expect("grow begins")
            .unwrap();

        let mut waiters = Vec::new();
        let (queued, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
        for _ in 0..16 {
            let runtime = growing.runtime.clone();
            let queued = queued.clone();
            waiters.push(tokio::spawn(async move {
                let _ = queued.send(());
                runtime.with(|_| ()).await
            }));
        }
        // A sender cannot yield between reporting arrival and polling `with`
        // to its first pending await. Every same-feed waiter is now queued.
        for _ in 0..16 {
            arrivals.recv().await.unwrap();
        }
        let result = tokio::time::timeout(Duration::from_secs(1), healthy.add()).await;
        let heartbeat = growing
            .runtime
            .heartbeat(Instant::now(), Duration::from_secs(10));
        let held = !job.is_finished();
        if let Ok(bytes) = &result {
            healthy.deliver(bytes, 0).await;
        }
        growing.deliver(&growing_bytes, 1).await;
        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());
        for waiter in waiters {
            waiter.abort();
            let _ = waiter.await;
        }
        drop(release);
        let ready = tokio::time::timeout(
            Duration::from_secs(2),
            growing.runtime.with(|runtime| runtime.snapshots()),
        )
        .await
        .expect("cancelled caller does not lose the running job's owner")
        .unwrap();
        assert!(
            held,
            "the factory watchdog must not release the fault first"
        );
        assert!(
            result.is_ok(),
            "another feed is not starved of blocking workers"
        );
        assert_eq!(heartbeat.len(), 1);
        assert_eq!(heartbeat[0].state, EgressShardHealth::Healthy);
        assert_eq!(
            ready.len(),
            2,
            "the owned construction finishes after cancellation"
        );
        assert_eq!(growing.runtime.snapshots().len(), 2);
        growing.deliver(&growing_bytes, 2).await;
        growing.runtime.shutdown_off_worker().await;
        healthy.runtime.shutdown_off_worker().await;
    });
}

#[tokio::test]
async fn dropping_unpublished_runtime_stops_shards_despite_retained_wake_handles() {
    let Fixture {
        runtime,
        feed,
        _wakes: retained_wakes,
        ..
    } = Fixture::new("unpublished").await;
    let observer = runtime.observer.clone();
    drop((runtime, feed));
    tokio::time::timeout(Duration::from_secs(2), async {
        while !observer.snapshots()[0].stopped {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("last-owner cleanup explicitly stops the unpublished shard");
    assert!(!observer.snapshots()[0].panicked);
    drop(retained_wakes);
}

#[tokio::test]
async fn a_control_job_panic_does_not_poison_the_feed_or_crash_the_executor() {
    let fixture = Fixture::new("control-panic").await;
    let bytes = fixture.add().await;
    fixture.deliver(&bytes, 0).await;
    let error = fixture
        .runtime
        .with(|_| {
            crate::test_support::with_expected_panic_suppressed(|| {
                panic!("scripted control job fault")
            })
        })
        .await
        .unwrap_err();
    assert!(matches!(error, FabricWorkerError::Join(_)));
    fixture.deliver(&bytes, 1).await;
    fixture.runtime.shutdown_off_worker().await;
}

#[tokio::test]
async fn closing_a_runtime_rejects_stale_control_handles() {
    let fixture = Fixture::new("closed").await;
    let bytes = fixture.add().await;
    fixture.deliver(&bytes, 0).await;
    let stale = fixture.runtime.clone();
    let snapshots = fixture.runtime.shutdown_off_worker().await;
    assert!(snapshots.iter().all(|snapshot| snapshot.stopped));
    let spec = output_spec("late", &fixture.id);
    assert_eq!(
        stale
            .with(move |runtime| runtime.dispatch(EgressCommand::Add(spec)))
            .await,
        Err(FabricWorkerError::Closed),
    );
}
