use crate::media::egress::backends::pipeline::PipelineTarget;
use crate::media::egress::backends::pipeline_shard::{
    PipelineShardBackend, SharedPipelineTargetSource,
};
use crate::media::egress::command::{EgressCommand, FeedId, OutputId};
use crate::media::egress::factory::spawn_pipeline_fabric_shard_group;
use crate::media::egress::journal::RingFeed;
use crate::media::egress::manager::{
    EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome,
};
use crate::media::egress::runtime::{
    EgressFabricRuntime, EgressFabricRuntimeError, spawn_fabric_wake_watcher,
};
use crate::media::egress::shard::EgressShardGroupError;
#[cfg(test)]
use crate::media::egress::shard::EgressShardSnapshot;
use crate::media::engine::MediaEngine;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PipelineFabricEnsureError {
    Spawn(EgressShardGroupError),
    Runtime(EgressFabricRuntimeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PipelineFabricDispatchError {
    MissingFeed { feed_id: FeedId },
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
}

impl MediaEngine {
    pub(crate) async fn retain_pipeline_fabric_runtime(
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
                config.shard_count(),
                config.shard_config(),
                config.work_budget(),
                target_source.clone(),
                |_| feed.clone_reader(),
            )
            .map_err(PipelineFabricEnsureError::Spawn)?;
            let manager_config =
                EgressManagerConfig::new(config.shards, config.command_channel_capacity)
                    .expect("egress fabric manager config is clamped nonzero");
            let runtime = EgressFabricRuntime::new(manager_config, group)
                .map_err(PipelineFabricEnsureError::Runtime)?;

            // Pipeline leaves have no poller either (see
            // `retain_sink_fabric_runtime`'s identical comment), so this is
            // their only readiness signal too.
            let watcher = spawn_fabric_wake_watcher(
                "pipeline",
                feed_id.clone(),
                feed.clone_reader(),
                runtime.feed_wake_handles(),
            );

            tracing::info!(feed_id = %feed_id, "pipeline fabric runtime created");
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry
                .target_sources
                .insert(feed_id.clone(), target_source);
            registry.feed_watchers.insert(feed_id.clone(), watcher);
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
    pub(crate) async fn set_pipeline_target(
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

    pub(crate) async fn dispatch_pipeline_fabric_command(
        &self,
        feed_id: &FeedId,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, PipelineFabricDispatchError> {
        let mut registry = self.fabric.pipeline.lock().await;
        if let EgressCommand::Remove(output_id) = &command
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
        if matches!(command, EgressCommand::Add(_))
            && let Some(inputs) = rescale_inputs.take()
        {
            self.rescale_pipeline_fabric(feed_id, runtime, inputs);
        }

        let outcome = runtime
            .dispatch(command)
            .map_err(PipelineFabricDispatchError::Dispatch)?;

        if let Some(inputs) = rescale_inputs.take() {
            self.rescale_pipeline_fabric(feed_id, runtime, inputs);
        }

        Ok(outcome)
    }

    fn rescale_pipeline_fabric(
        &self,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        (feed, target_source): (RingFeed, SharedPipelineTargetSource),
    ) {
        let config = &self.config.egress_fabric;
        let shard_config = config.shard_config();
        let budget = config.work_budget();
        let effective_cpus = crate::system_sampling::effective_cpu_count();
        let result = runtime.rescale(
            crate::config::EgressShardProfile::OutputCount,
            effective_cpus,
            shard_config,
            |_shard_id| {
                Ok::<_, std::convert::Infallible>(PipelineShardBackend::new(
                    feed.clone_reader(),
                    budget,
                    target_source.clone(),
                ))
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

    pub(crate) async fn release_pipeline_fabric_runtime(&self, feed_id: &FeedId) -> bool {
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
            if let Some(watcher) = registry.feed_watchers.remove(feed_id) {
                watcher.abort();
            }
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
    ) -> Vec<(
        FeedId,
        Vec<crate::media::egress::shard::EgressShardHeartbeat>,
    )> {
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
            for watcher in registry.feed_watchers.drain() {
                watcher.1.abort();
            }
            std::mem::take(&mut registry.runtimes)
        };
        let count = runtimes.len();
        for runtime in runtimes.into_values() {
            let _ = runtime.shutdown();
        }
        count
    }
}
