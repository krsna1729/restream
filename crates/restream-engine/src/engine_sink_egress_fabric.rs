use crate::egress::backends::sink_shard::SinkShardBackend;
use crate::egress::command::{EgressCommand, FeedId};
use crate::egress::factory::spawn_sink_fabric_shard_group;
use crate::egress::journal::RingFeed;
use crate::egress::manager::{
    EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome, PlacementPolicy,
};
use crate::egress::policy::WorkBudgetConfig;
use crate::egress::runtime::{
    EgressFabricRuntime, EgressFabricRuntimeError, FabricWorkerError, ResizeReason,
    SharedFabricRuntime,
};
#[cfg(test)]
use crate::egress::shard::EgressShardSnapshot;
use crate::egress::shard::{EgressShardConfig, EgressShardGroupError};
use crate::engine::MediaEngine;

#[derive(Debug, PartialEq, Eq)]
pub enum SinkFabricEnsureError {
    Spawn(EgressShardGroupError),
    Runtime(EgressFabricRuntimeError),
    Worker(FabricWorkerError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkFabricDispatchError {
    MissingFeed {
        feed_id: FeedId,
    },
    /// Admission refused the new output; nothing was dispatched.
    Saturated(crate::egress::sizing::Saturated),
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
    Worker(FabricWorkerError),
}

impl SinkFabricDispatchError {
    pub fn refused(&self) -> Option<&crate::egress::sizing::Saturated> {
        match self {
            Self::Saturated(saturated) => Some(saturated),
            _ => None,
        }
    }
}

impl MediaEngine {
    pub async fn retain_sink_fabric_runtime(
        &self,
        feed_id: FeedId,
        feed: &RingFeed,
    ) -> Result<bool, SinkFabricEnsureError> {
        let generation = {
            let mut registry = self.fabric.sink.lock().await;
            if registry.runtimes.contains_key(&feed_id) {
                let outputs = registry.active_outputs.entry(feed_id).or_insert(0);
                *outputs = outputs.saturating_add(1);
                return Ok(false);
            }
            registry.generation
        };
        let engine_config = self.config.clone();
        let candidate_feed = feed.clone_reader();
        let candidate = tokio::task::spawn_blocking(move || {
            let config = &engine_config.egress_fabric;
            let group = spawn_sink_fabric_shard_group(
                std::num::NonZeroU32::MIN,
                EgressShardConfig::from_fabric(config),
                WorkBudgetConfig::from_fabric(config),
                |_| candidate_feed.clone_reader(),
            )
            .map_err(SinkFabricEnsureError::Spawn)?;
            let manager_config = EgressManagerConfig::new(1, config.command_channel_capacity)
                .expect("egress fabric manager config is clamped nonzero")
                .with_placement_policy(PlacementPolicy::LeastLoaded);
            let runtime = EgressFabricRuntime::new(manager_config, group, config.shards)
                .map_err(SinkFabricEnsureError::Runtime)?;
            Ok(SharedFabricRuntime::new(runtime))
        })
        .await
        .map_err(|error| {
            SinkFabricEnsureError::Worker(FabricWorkerError::Join(error.to_string()))
        })??;
        let runtime = candidate;
        let mut registry = self.fabric.sink.lock().await;
        if registry.generation != generation {
            return Err(SinkFabricEnsureError::Worker(FabricWorkerError::Closed));
        }
        let created = !registry.runtimes.contains_key(&feed_id);
        if created {
            let wakes = runtime.subscribe_feed_wakes(
                "sink",
                feed_id.clone(),
                feed.publication_ring().publication_subscribers(),
            );
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry.feed_wakes.insert(feed_id.clone(), wakes);
            registry.feeds.insert(feed_id.clone(), feed.clone_reader());
            tracing::info!(feed_id = %feed_id, "sink fabric runtime created");
        }
        let outputs = registry.active_outputs.entry(feed_id).or_insert(0);
        *outputs = outputs.saturating_add(1);
        Ok(created)
    }

    pub async fn dispatch_sink_fabric_command(
        &self,
        feed_id: &FeedId,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, SinkFabricDispatchError> {
        let (runtime, rescale_feed) = {
            let registry = self.fabric.sink.lock().await;
            let runtime = registry.runtimes.get(feed_id).cloned().ok_or_else(|| {
                SinkFabricDispatchError::MissingFeed {
                    feed_id: feed_id.clone(),
                }
            })?;
            (
                runtime,
                registry.feeds.get(feed_id).map(RingFeed::clone_reader),
            )
        };
        let config = self.config.clone();
        let feed_id = feed_id.clone();
        runtime
            .with(move |runtime| {
                let mut rescale_feed = rescale_feed;
                // Resize before placement/admission; do not reconnect a fresh output.
                if let EgressCommand::Add(spec) = &command
                    && let Some(feed) = rescale_feed.take()
                {
                    let reason = runtime.reason_for(&command);
                    Self::rescale_sink_fabric(&config, &feed_id, runtime, feed, reason);
                    if matches!(reason, ResizeReason::Add) {
                        runtime
                            .admit_add(
                                crate::config::EgressShardProfile::SinkOutputCount,
                                EgressShardConfig::from_fabric(&config.egress_fabric),
                                &spec.id,
                            )
                            .map_err(SinkFabricDispatchError::Saturated)?;
                    }
                }
                let outcome = runtime
                    .dispatch(command)
                    .map_err(SinkFabricDispatchError::Dispatch)?;
                if let Some(feed) = rescale_feed {
                    Self::rescale_sink_fabric(
                        &config,
                        &feed_id,
                        runtime,
                        feed,
                        ResizeReason::Remove,
                    );
                }
                Ok(outcome)
            })
            .await
            .map_err(SinkFabricDispatchError::Worker)?
    }

    /// The reconciler's periodic look: learn what this feed's sink outputs
    /// cost from a fresh delivery window (the price admission refuses by),
    /// and replace a panicked shard, releasing its outputs to retry (see
    /// `EgressFabricRuntime::rescale`), on an otherwise quiet feed.
    pub async fn resize_sink_fabrics(&self) {
        let inputs: Vec<_> = {
            let registry = self.fabric.sink.lock().await;
            registry
                .runtimes
                .iter()
                .filter_map(|(id, runtime)| {
                    Some((
                        id.clone(),
                        runtime.clone(),
                        registry.feeds.get(id)?.clone_reader(),
                    ))
                })
                .collect()
        };
        for (id, runtime, inputs) in inputs {
            let config = self.config.clone();
            runtime.try_observe(move |runtime| {
                Self::rescale_sink_fabric(&config, &id, runtime, inputs, ResizeReason::Observe);
            });
        }
    }

    fn rescale_sink_fabric(
        engine_config: &crate::config::EngineConfig,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        feed: RingFeed,
        reason: ResizeReason,
    ) {
        // Price unmeasured outputs at this feed's rate, as the other fabrics do.
        runtime.forecast_feed(&feed.publication_ring());
        let config = &engine_config.egress_fabric;
        let shard_config = EgressShardConfig::from_fabric(config);
        let budget = WorkBudgetConfig::from_fabric(config);
        let result = runtime.rescale(
            crate::config::EgressShardProfile::SinkOutputCount,
            crate::host_capacity::HostCapacity::get(),
            reason,
            shard_config,
            |_shard_id| {
                let feed = feed.clone_reader();
                let leaf_capacity = shard_config.leaf_capacity().get();
                move || {
                    Ok::<_, std::convert::Infallible>(
                        SinkShardBackend::new(feed, budget).with_leaf_capacity(leaf_capacity),
                    )
                }
            },
        );
        match result {
            Ok(touched) if !touched.is_empty() => {
                tracing::info!(feed_id = %feed_id, shards = ?touched, "sink fabric shard pool rescaled");
            }
            Ok(_) => {}
            Err(error) => match error {},
        }
    }

    pub async fn release_sink_fabric_runtime(&self, feed_id: &FeedId) -> bool {
        let runtime = {
            let mut registry = self.fabric.sink.lock().await;
            let Some(active_outputs) = registry.active_outputs.get_mut(feed_id) else {
                return false;
            };
            *active_outputs = active_outputs.saturating_sub(1);
            if *active_outputs > 0 {
                return false;
            }
            registry.active_outputs.remove(feed_id);
            registry.feeds.remove(feed_id);
            registry.feed_wakes.remove(feed_id);
            registry.runtimes.remove(feed_id)
        };

        let Some(runtime) = runtime else {
            return false;
        };
        let _ = runtime.shutdown_off_worker().await;
        true
    }

    #[cfg(test)]
    pub(crate) async fn sink_fabric_runtime_snapshots(
        &self,
        feed_id: &FeedId,
    ) -> Option<Vec<EgressShardSnapshot>> {
        let registry = self.fabric.sink.lock().await;
        registry
            .runtimes
            .get(feed_id)
            .map(SharedFabricRuntime::snapshots)
    }

    /// Per-shard health across every live sink fabric runtime, for
    /// diagnostics and alerting.
    pub(crate) async fn sink_fabric_shard_heartbeats(
        &self,
        stall_after: std::time::Duration,
    ) -> Vec<(FeedId, Vec<crate::egress::shard::EgressShardHeartbeat>)> {
        let now = std::time::Instant::now();
        let registry = self.fabric.sink.lock().await;
        registry
            .runtimes
            .iter()
            .map(|(feed_id, runtime)| (feed_id.clone(), runtime.heartbeat(now, stall_after)))
            .collect()
    }

    pub(crate) async fn shutdown_all_sink_fabric_runtimes(&self) -> usize {
        let runtimes = {
            let mut registry = self.fabric.sink.lock().await;
            registry.generation = registry.generation.wrapping_add(1);
            registry.feeds.clear();
            registry.active_outputs.clear();
            registry.feed_wakes.clear();
            std::mem::take(&mut registry.runtimes)
        };
        let count = runtimes.len();
        let mut jobs = tokio::task::JoinSet::new();
        for runtime in runtimes.into_values() {
            jobs.spawn(runtime.shutdown_off_worker());
        }
        while jobs.join_next().await.is_some() {}
        count
    }
}
