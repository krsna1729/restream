//! MoQ fabric runtimes on the engine: one per feed, retained by each running
//! `moqt://` output and shut down with the last one — the RTMP fabric's
//! lifecycle (`engine_rtmp_egress_fabric.rs`), with the MoQ shard backend.

use crate::media::egress::command::{EgressCommand, FeedId, OutputId};
use crate::media::egress::factory::{MoqFabricShardGroupError, spawn_moq_fabric_shard_group};
use crate::media::egress::journal::RingFeed;
use crate::media::egress::manager::{
    EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome, PlacementPolicy,
};
use crate::media::egress::runtime::{
    EgressFabricRuntime, EgressFabricRuntimeError, ResizeReason, subscribe_fabric_wakes,
};
use crate::media::egress::shard::EgressShardGroupError;
use crate::media::engine::MediaEngine;
use crate::media::moq::egress::{MoqPublishStartup, SharedMoqStartupSource, moq_shard_backend};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MoqFabricEnsureError {
    Spawn(MoqFabricShardGroupError),
    Runtime(EgressFabricRuntimeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MoqFabricDispatchError {
    MissingFeed {
        feed_id: FeedId,
    },
    /// Admission refused the new output; nothing was dispatched.
    Saturated(crate::media::egress::sizing::Saturated),
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
}

impl MoqFabricDispatchError {
    pub(crate) fn refused(&self) -> Option<&crate::media::egress::sizing::Saturated> {
        match self {
            Self::Saturated(saturated) => Some(saturated),
            _ => None,
        }
    }
}

impl MediaEngine {
    pub(crate) async fn retain_moq_fabric_runtime(
        &self,
        feed_id: FeedId,
        feed: &RingFeed,
    ) -> Result<bool, MoqFabricEnsureError> {
        let mut registry = self.fabric.moq.lock().await;
        let created = if registry.runtimes.contains_key(&feed_id) {
            false
        } else {
            let config = &self.config.egress_fabric;
            let startup_source = SharedMoqStartupSource::default();
            let group = spawn_moq_fabric_shard_group(
                std::num::NonZeroU32::MIN,
                config.shard_config(),
                config.work_budget(),
                startup_source.clone(),
                |_| feed.clone_reader(),
            )
            .map_err(MoqFabricEnsureError::Spawn)?;
            // Least-loaded placement: see the SRT fabric.
            let manager_config = EgressManagerConfig::new(1, config.command_channel_capacity)
                .expect("egress fabric manager config is clamped nonzero")
                .with_placement_policy(PlacementPolicy::LeastLoaded);
            let runtime = EgressFabricRuntime::new(manager_config, group)
                .map_err(MoqFabricEnsureError::Runtime)?
                .adaptive(config.shards);
            let wakes = subscribe_fabric_wakes(
                "moq",
                feed_id.clone(),
                feed.publication_ring().publication_subscribers(),
                runtime.feed_wake_handles(),
            );

            tracing::info!(feed_id = %feed_id, "moq fabric runtime created");
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry
                .startup_sources
                .insert(feed_id.clone(), startup_source);
            registry.feed_wakes.insert(feed_id.clone(), wakes);
            registry.feeds.insert(feed_id.clone(), feed.clone_reader());
            true
        };

        let active_outputs = registry.active_outputs.entry(feed_id).or_insert(0);
        *active_outputs = active_outputs.saturating_add(1);
        Ok(created)
    }

    /// Write the publish startup for `output_id` before dispatching its
    /// `Add`: the shard thread only reads it, never queries the engine.
    pub(crate) async fn set_moq_publish_startup(
        &self,
        feed_id: &FeedId,
        output_id: OutputId,
        startup: MoqPublishStartup,
    ) -> bool {
        let registry = self.fabric.moq.lock().await;
        let Some(source) = registry.startup_sources.get(feed_id) else {
            return false;
        };
        source.set(output_id, startup);
        true
    }

    pub(crate) async fn dispatch_moq_fabric_command(
        &self,
        feed_id: &FeedId,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, MoqFabricDispatchError> {
        let mut registry = self.fabric.moq.lock().await;
        if let EgressCommand::Remove(output_id) | EgressCommand::Abandon(output_id) = &command
            && let Some(source) = registry.startup_sources.get(feed_id)
        {
            source.remove(output_id);
        }
        let mut rescale_inputs = registry
            .startup_sources
            .get(feed_id)
            .cloned()
            .zip(registry.feeds.get(feed_id).map(RingFeed::clone_reader));
        let Some(runtime) = registry.runtimes.get_mut(feed_id) else {
            return Err(MoqFabricDispatchError::MissingFeed {
                feed_id: feed_id.clone(),
            });
        };

        // Size the pool for an `Add` before dispatching it, so a fresh QUIC
        // connection lands on its final shard the first time (see
        // `dispatch_rtmp_fabric_command`).
        let mut startup = None;
        if let EgressCommand::Add(spec) = &command
            && let Some(inputs) = rescale_inputs.take()
        {
            let reason = runtime.reason_for(&command);
            let source = inputs.0.clone();
            self.rescale_moq_fabric(feed_id, runtime, inputs, reason);
            if matches!(reason, ResizeReason::Add)
                && let Err(saturated) =
                    runtime.admit_add(crate::config::EgressShardProfile::OutputCount, &spec.id)
            {
                source.remove(&spec.id);
                return Err(MoqFabricDispatchError::Saturated(saturated));
            }
            startup = Some((source, spec.id.clone()));
        }

        let outcome = runtime.dispatch(command).map_err(|error| {
            if let Some((source, output_id)) = &startup {
                source.remove(output_id);
            }
            MoqFabricDispatchError::Dispatch(error)
        })?;

        if let Some(inputs) = rescale_inputs.take() {
            self.rescale_moq_fabric(feed_id, runtime, inputs, ResizeReason::Remove);
        }
        Ok(outcome)
    }

    pub(crate) async fn resize_moq_fabrics(&self) {
        let mut registry = self.fabric.moq.lock().await;
        let crate::media::engine_registries::MoqFabricRegistry {
            runtimes,
            feeds,
            startup_sources,
            ..
        } = &mut *registry;
        for (id, runtime) in runtimes {
            if !runtime.observation_due() {
                continue;
            }
            if let (Some(feed), Some(startup)) = (feeds.get(id), startup_sources.get(id)) {
                self.rescale_moq_fabric(
                    id,
                    runtime,
                    (startup.clone(), feed.clone_reader()),
                    ResizeReason::Observe,
                );
            }
        }
    }

    fn rescale_moq_fabric(
        &self,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        (startup_source, feed): (SharedMoqStartupSource, RingFeed),
        reason: ResizeReason,
    ) {
        let config = &self.config.egress_fabric;
        let shard_config = config.shard_config();
        let budget = config.work_budget();
        runtime.forecast_feed(&feed.publication_ring());
        let result = runtime.rescale(
            crate::config::EgressShardProfile::OutputCount,
            crate::capacity::HostCapacity::get(),
            reason,
            shard_config,
            |_shard_id| {
                let feed = feed.clone_reader();
                let startup_source = startup_source.clone();
                let drain_timeout = shard_config.drain_timeout();
                let leaf_capacity = shard_config.leaf_capacity().get();
                move || {
                    moq_shard_backend(feed, budget, startup_source, drain_timeout, leaf_capacity)
                }
            },
        );
        match result {
            Ok(touched) if !touched.is_empty() => {
                tracing::info!(feed_id = %feed_id, shards = ?touched, "moq fabric shard pool rescaled");
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(feed_id = %feed_id, error = ?error, "moq fabric rescale failed to grow a shard");
            }
        }
    }

    pub(crate) async fn release_moq_fabric_runtime(&self, feed_id: &FeedId) -> bool {
        let runtime = {
            let mut registry = self.fabric.moq.lock().await;
            let Some(active_outputs) = registry.active_outputs.get_mut(feed_id) else {
                return false;
            };
            *active_outputs = active_outputs.saturating_sub(1);
            if *active_outputs > 0 {
                return false;
            }
            registry.active_outputs.remove(feed_id);
            registry.startup_sources.remove(feed_id);
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

    /// Per-shard health across every live MoQ fabric runtime, for
    /// diagnostics and alerting.
    pub(crate) async fn moq_fabric_shard_heartbeats(
        &self,
        stall_after: std::time::Duration,
    ) -> Vec<(
        FeedId,
        Vec<crate::media::egress::shard::EgressShardHeartbeat>,
    )> {
        let now = std::time::Instant::now();
        let registry = self.fabric.moq.lock().await;
        registry
            .runtimes
            .iter()
            .map(|(feed_id, runtime)| (feed_id.clone(), runtime.heartbeat(now, stall_after)))
            .collect()
    }

    pub(crate) async fn shutdown_all_moq_fabric_runtimes(&self) -> usize {
        let runtimes = {
            let mut registry = self.fabric.moq.lock().await;
            registry.active_outputs.clear();
            registry.startup_sources.clear();
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
