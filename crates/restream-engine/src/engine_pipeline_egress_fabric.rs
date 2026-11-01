use crate::egress::backends::pipeline::PipelineTarget;
use crate::egress::backends::pipeline_shard::{PipelineShardBackend, SharedPipelineTargetSource};
use crate::egress::command::{EgressCommand, FeedId, OutputId};
use crate::egress::factory::spawn_pipeline_fabric_shard_group;
use crate::egress::journal::RingFeed;
use crate::egress::manager::{
    EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome, PlacementPolicy,
};
use crate::egress::runtime::{
    EgressFabricRuntime, EgressFabricRuntimeError, ResizeReason, subscribe_fabric_wakes,
};
use crate::egress::shard::EgressShardGroupError;
#[cfg(test)]
use crate::egress::shard::EgressShardSnapshot;
use crate::engine::MediaEngine;

#[derive(Debug, PartialEq, Eq)]
pub enum PipelineFabricEnsureError {
    Spawn(EgressShardGroupError),
    Runtime(EgressFabricRuntimeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineFabricDispatchError {
    MissingFeed {
        feed_id: FeedId,
    },
    /// Admission refused the new output; nothing was dispatched and its
    /// pending target was dropped.
    Saturated(crate::egress::sizing::Saturated),
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
}

impl PipelineFabricDispatchError {
    pub fn refused(&self) -> Option<&crate::egress::sizing::Saturated> {
        match self {
            Self::Saturated(saturated) => Some(saturated),
            _ => None,
        }
    }
}

impl MediaEngine {
    pub async fn retain_pipeline_fabric_runtime(
        &self,
        feed_id: FeedId,
        feed: &RingFeed,
    ) -> Result<bool, PipelineFabricEnsureError> {
        let mut registry = self.fabric.pipeline.lock().await;
        let created = if registry.runtimes.contains_key(&feed_id) {
            false
        } else {
            let config = &self.config.egress_fabric;
            let target_source = SharedPipelineTargetSource::new();
            let group = spawn_pipeline_fabric_shard_group(
                std::num::NonZeroU32::MIN,
                config.shard_config(),
                config.work_budget(),
                target_source.clone(),
                |_| feed.clone_reader(),
            )
            .map_err(PipelineFabricEnsureError::Spawn)?;
            // Least-loaded, as for the other fabrics (see
            // `retain_sink_fabric_runtime`).
            let manager_config = EgressManagerConfig::new(1, config.command_channel_capacity)
                .expect("egress fabric manager config is clamped nonzero")
                .with_placement_policy(PlacementPolicy::LeastLoaded);
            let runtime = EgressFabricRuntime::new(manager_config, group, config.shards)
                .map_err(PipelineFabricEnsureError::Runtime)?;

            // Pipeline leaves have no poller either (see
            // `retain_sink_fabric_runtime`'s identical comment), so this is
            // their only readiness signal too.
            let wakes = subscribe_fabric_wakes(
                "pipeline",
                feed_id.clone(),
                feed.publication_ring().publication_subscribers(),
                runtime.feed_wake_handles(),
            );

            tracing::info!(feed_id = %feed_id, "pipeline fabric runtime created");
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry
                .target_sources
                .insert(feed_id.clone(), target_source);
            registry.feed_wakes.insert(feed_id.clone(), wakes);
            registry.feeds.insert(feed_id.clone(), feed.clone_reader());
            true
        };

        let active_outputs = registry.active_outputs.entry(feed_id).or_insert(0);
        *active_outputs = active_outputs.saturating_add(1);
        Ok(created)
    }

    /// Records the target the shard thread should publish into once it
    /// processes `EgressCommand::Add` for `output_id`. Must land before
    /// that dispatch — the shard never queries `MediaEngine` itself (see
    /// `PipelineTargetSource`'s doc comment).
    pub async fn set_pipeline_target(
        &self,
        feed_id: &FeedId,
        output_id: OutputId,
        target: PipelineTarget,
    ) -> bool {
        let registry = self.fabric.pipeline.lock().await;
        let Some(source) = registry.target_sources.get(feed_id) else {
            return false;
        };
        source.set(output_id, target);
        true
    }

    pub async fn dispatch_pipeline_fabric_command(
        &self,
        feed_id: &FeedId,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, PipelineFabricDispatchError> {
        let mut registry = self.fabric.pipeline.lock().await;
        if let EgressCommand::Remove(output_id) | EgressCommand::Abandon(output_id) = &command
            && let Some(source) = registry.target_sources.get(feed_id)
        {
            source.remove(output_id);
        }
        // Owned upfront (see `dispatch_rtmp_fabric_command`'s identical
        // comment): disjoint from the `runtimes` borrow below, and no
        // lingering reference into `registry` for `rescale` to hold.
        let rescale_inputs = registry
            .feeds
            .get(feed_id)
            .map(|feed| feed.clone_reader())
            .zip(registry.target_sources.get(feed_id).cloned());
        let Some(runtime) = registry.runtimes.get_mut(feed_id) else {
            return Err(PipelineFabricDispatchError::MissingFeed {
                feed_id: feed_id.clone(),
            });
        };

        // See `dispatch_rtmp_fabric_command`'s identical comment: size the
        // pool for an `Add` before dispatching it so it lands on its final
        // shard the first time, instead of connecting once and immediately
        // getting rehomed onto a different shard.
        let mut rescale_inputs = rescale_inputs;
        if let EgressCommand::Add(spec) = &command
            && let Some(inputs) = rescale_inputs.take()
        {
            let reason = runtime.reason_for(&command);
            let target_source = inputs.1.clone();
            self.rescale_pipeline_fabric(feed_id, runtime, inputs, reason);
            if matches!(reason, ResizeReason::Add)
                && let Err(saturated) = runtime.admit_add(
                    crate::config::EgressShardProfile::PipelineOutputCount,
                    self.config.egress_fabric.shard_config(),
                    &spec.id,
                )
            {
                // No shard will take the target: release the ring and the
                // claimed input's registration it holds.
                target_source.remove(&spec.id);
                return Err(PipelineFabricDispatchError::Saturated(saturated));
            }
        }

        let outcome = runtime
            .dispatch(command)
            .map_err(PipelineFabricDispatchError::Dispatch)?;

        if let Some(inputs) = rescale_inputs.take() {
            self.rescale_pipeline_fabric(feed_id, runtime, inputs, ResizeReason::Remove);
        }

        Ok(outcome)
    }

    /// The reconciler's periodic look: learn what this feed's pipeline
    /// outputs cost from a fresh delivery window (the price admission
    /// refuses by), and replace a panicked shard, releasing its outputs to
    /// retry (see `EgressFabricRuntime::rescale`), on an otherwise quiet feed.
    pub async fn resize_pipeline_fabrics(&self) {
        let mut registry = self.fabric.pipeline.lock().await;
        let crate::engine_registries::PipelineFabricRegistry {
            runtimes,
            feeds,
            target_sources,
            ..
        } = &mut *registry;
        for (id, runtime) in runtimes {
            if (runtime.observation_due() || runtime.has_panicked_shard())
                && let (Some(feed), Some(targets)) = (feeds.get(id), target_sources.get(id))
            {
                self.rescale_pipeline_fabric(
                    id,
                    runtime,
                    (feed.clone_reader(), targets.clone()),
                    ResizeReason::Observe,
                );
            }
        }
    }

    fn rescale_pipeline_fabric(
        &self,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        (feed, target_source): (RingFeed, SharedPipelineTargetSource),
        reason: ResizeReason,
    ) {
        // Price unmeasured outputs at this feed's rate, as the other fabrics do.
        runtime.forecast_feed(&feed.publication_ring());
        let config = &self.config.egress_fabric;
        let shard_config = config.shard_config();
        let budget = config.work_budget();
        let result = runtime.rescale(
            crate::config::EgressShardProfile::PipelineOutputCount,
            crate::host_capacity::HostCapacity::get(),
            reason,
            shard_config,
            |_shard_id| {
                let feed = feed.clone_reader();
                let target_source = target_source.clone();
                let leaf_capacity = shard_config.leaf_capacity().get();
                move || {
                    Ok::<_, std::convert::Infallible>(
                        PipelineShardBackend::new(feed, budget, target_source)
                            .with_leaf_capacity(leaf_capacity),
                    )
                }
            },
        );
        match result {
            Ok(touched) if !touched.is_empty() => {
                tracing::info!(feed_id = %feed_id, shards = ?touched, "pipeline fabric shard pool rescaled");
            }
            Ok(_) => {}
            Err(error) => match error {},
        }
    }

    pub async fn release_pipeline_fabric_runtime(&self, feed_id: &FeedId) -> bool {
        let runtime = {
            let mut registry = self.fabric.pipeline.lock().await;
            let Some(active_outputs) = registry.active_outputs.get_mut(feed_id) else {
                return false;
            };
            *active_outputs = active_outputs.saturating_sub(1);
            if *active_outputs > 0 {
                return false;
            }
            registry.active_outputs.remove(feed_id);
            registry.target_sources.remove(feed_id);
            registry.feeds.remove(feed_id);
            registry.feed_wakes.remove(feed_id);
            registry.runtimes.remove(feed_id)
        };

        let Some(runtime) = runtime else {
            return false;
        };
        let _ = runtime.shutdown();
        true
    }

    #[cfg(test)]
    pub(crate) async fn pipeline_fabric_runtime_snapshots(
        &self,
        feed_id: &FeedId,
    ) -> Option<Vec<EgressShardSnapshot>> {
        let registry = self.fabric.pipeline.lock().await;
        registry
            .runtimes
            .get(feed_id)
            .map(EgressFabricRuntime::snapshots)
    }

    /// Per-shard health across every live pipeline-recirculation fabric
    /// runtime, for diagnostics and alerting.
    pub(crate) async fn pipeline_fabric_shard_heartbeats(
        &self,
        stall_after: std::time::Duration,
    ) -> Vec<(FeedId, Vec<crate::egress::shard::EgressShardHeartbeat>)> {
        let now = std::time::Instant::now();
        let registry = self.fabric.pipeline.lock().await;
        registry
            .runtimes
            .iter()
            .map(|(feed_id, runtime)| (feed_id.clone(), runtime.heartbeat(now, stall_after)))
            .collect()
    }

    pub(crate) async fn shutdown_all_pipeline_fabric_runtimes(&self) -> usize {
        let runtimes = {
            let mut registry = self.fabric.pipeline.lock().await;
            registry.active_outputs.clear();
            registry.target_sources.clear();
            registry.feed_wakes.clear();
            std::mem::take(&mut registry.runtimes)
        };
        let count = runtimes.len();
        for runtime in runtimes.into_values() {
            let _ = runtime.shutdown();
        }
        count
    }
}
