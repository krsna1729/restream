//! MoQ fabric runtimes on the engine: one per feed, retained by each running
//! `moqt://` output and shut down with the last one — the RTMP fabric's
//! lifecycle (`engine_rtmp_egress_fabric.rs`), with the MoQ shard backend.

use crate::egress::backends::moq::{MoqPublishStartup, SharedMoqStartupSource, moq_shard_backend};
use crate::egress::command::{EgressCommand, FeedId, OutputId};
use crate::egress::factory::{MoqFabricShardGroupError, spawn_moq_fabric_shard_group};
use crate::egress::journal::RingFeed;
use crate::egress::manager::{
    EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome, PlacementPolicy,
};
use crate::egress::policy::WorkBudgetConfig;
use crate::egress::runtime::{
    EgressFabricRuntime, EgressFabricRuntimeError, FabricWorkerError, ResizeReason,
    SharedFabricRuntime,
};
use crate::egress::shard::{EgressShardConfig, EgressShardGroupError};
use crate::engine::MediaEngine;

#[derive(Debug, PartialEq, Eq)]
pub enum MoqFabricEnsureError {
    Spawn(MoqFabricShardGroupError),
    Runtime(EgressFabricRuntimeError),
    Worker(FabricWorkerError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoqFabricDispatchError {
    MissingFeed {
        feed_id: FeedId,
    },
    /// Admission refused the new output; nothing was dispatched.
    Saturated(crate::egress::sizing::Saturated),
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
    Worker(FabricWorkerError),
}

impl MoqFabricDispatchError {
    pub fn refused(&self) -> Option<&crate::egress::sizing::Saturated> {
        match self {
            Self::Saturated(saturated) => Some(saturated),
            _ => None,
        }
    }
}

impl MediaEngine {
    pub async fn retain_moq_fabric_runtime(
        &self,
        feed_id: FeedId,
        feed: &RingFeed,
    ) -> Result<bool, MoqFabricEnsureError> {
        let generation = {
            let mut registry = self.fabric.moq.lock().await;
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
            let startup_source = SharedMoqStartupSource::default();
            let group = spawn_moq_fabric_shard_group(
                std::num::NonZeroU32::MIN,
                EgressShardConfig::from_fabric(config),
                WorkBudgetConfig::from_fabric(config),
                startup_source.clone(),
                |_| candidate_feed.clone_reader(),
            )
            .map_err(MoqFabricEnsureError::Spawn)?;
            let manager_config = EgressManagerConfig::new(1, config.command_channel_capacity)
                .expect("egress fabric manager config is clamped nonzero")
                .with_placement_policy(PlacementPolicy::LeastLoaded);
            let runtime = EgressFabricRuntime::new(manager_config, group, config.shards)
                .map_err(MoqFabricEnsureError::Runtime)?;
            Ok((SharedFabricRuntime::new(runtime), startup_source))
        })
        .await
        .map_err(|error| {
            MoqFabricEnsureError::Worker(FabricWorkerError::Join(error.to_string()))
        })??;
        let (runtime, startup_source) = candidate;
        let mut registry = self.fabric.moq.lock().await;
        if registry.generation != generation {
            return Err(MoqFabricEnsureError::Worker(FabricWorkerError::Closed));
        }
        let created = !registry.runtimes.contains_key(&feed_id);
        if created {
            let wakes = runtime.subscribe_feed_wakes(
                "moq",
                feed_id.clone(),
                feed.publication_ring().publication_subscribers(),
            );
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry.feed_wakes.insert(feed_id.clone(), wakes);
            registry
                .startup_sources
                .insert(feed_id.clone(), startup_source);
            registry.feeds.insert(feed_id.clone(), feed.clone_reader());
            tracing::info!(feed_id = %feed_id, "moq fabric runtime created");
        }
        let outputs = registry.active_outputs.entry(feed_id).or_insert(0);
        *outputs = outputs.saturating_add(1);
        Ok(created)
    }

    /// Write the publish startup for `output_id` before dispatching its
    /// `Add`: the shard thread only reads it, never queries the engine.
    pub async fn set_moq_publish_startup(
        &self,
        feed_id: &FeedId,
        output_id: OutputId,
        startup: MoqPublishStartup,
    ) -> bool {
        let (runtime, source) = {
            let registry = self.fabric.moq.lock().await;
            let (Some(runtime), Some(source)) = (
                registry.runtimes.get(feed_id),
                registry.startup_sources.get(feed_id),
            ) else {
                return false;
            };
            (runtime.clone(), source.clone())
        };
        runtime
            .with(move |_| source.set(output_id, startup))
            .await
            .is_ok()
    }

    pub async fn dispatch_moq_fabric_command(
        &self,
        feed_id: &FeedId,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, MoqFabricDispatchError> {
        let (runtime, source, rescale_inputs) = {
            let registry = self.fabric.moq.lock().await;
            let runtime = registry.runtimes.get(feed_id).cloned().ok_or_else(|| {
                MoqFabricDispatchError::MissingFeed {
                    feed_id: feed_id.clone(),
                }
            })?;
            let source = registry.startup_sources.get(feed_id).cloned();
            let inputs = source
                .clone()
                .zip(registry.feeds.get(feed_id).map(RingFeed::clone_reader));
            (runtime, source, inputs)
        };
        let config = self.config.clone();
        let feed_id = feed_id.clone();
        runtime
            .with(move |runtime| {
                if let EgressCommand::Remove(output_id) | EgressCommand::Abandon(output_id) =
                    &command
                    && let Some(source) = &source
                {
                    source.remove(output_id);
                }
                let mut rescale_inputs = rescale_inputs;
                let mut startup = None;
                // Resize before placement; a fresh QUIC connection must not be rehomed.
                if let EgressCommand::Add(spec) = &command
                    && let Some(inputs) = rescale_inputs.take()
                {
                    let reason = runtime.reason_for(&command);
                    let source = inputs.0.clone();
                    Self::rescale_moq_fabric(&config, &feed_id, runtime, inputs, reason);
                    if matches!(reason, ResizeReason::Add)
                        && let Err(saturated) = runtime.admit_add(
                            crate::config::EgressShardProfile::OutputCount,
                            EgressShardConfig::from_fabric(&config.egress_fabric),
                            &spec.id,
                        )
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
                if let Some(inputs) = rescale_inputs {
                    Self::rescale_moq_fabric(
                        &config,
                        &feed_id,
                        runtime,
                        inputs,
                        ResizeReason::Remove,
                    );
                }
                Ok(outcome)
            })
            .await
            .map_err(MoqFabricDispatchError::Worker)?
    }

    pub async fn resize_moq_fabrics(&self) {
        let inputs: Vec<_> = {
            let registry = self.fabric.moq.lock().await;
            registry
                .runtimes
                .iter()
                .filter_map(|(id, runtime)| {
                    Some((
                        id.clone(),
                        runtime.clone(),
                        (
                            registry.startup_sources.get(id)?.clone(),
                            registry.feeds.get(id)?.clone_reader(),
                        ),
                    ))
                })
                .collect()
        };
        for (id, runtime, inputs) in inputs {
            let config = self.config.clone();
            runtime.try_observe(move |runtime| {
                Self::rescale_moq_fabric(&config, &id, runtime, inputs, ResizeReason::Observe);
            });
        }
    }

    fn rescale_moq_fabric(
        engine_config: &crate::config::EngineConfig,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        (startup_source, feed): (SharedMoqStartupSource, RingFeed),
        reason: ResizeReason,
    ) {
        let config = &engine_config.egress_fabric;
        let shard_config = EgressShardConfig::from_fabric(config);
        let budget = WorkBudgetConfig::from_fabric(config);
        runtime.forecast_feed(&feed.publication_ring());
        let result = runtime.rescale(
            crate::config::EgressShardProfile::OutputCount,
            crate::host_capacity::HostCapacity::get(),
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

    pub async fn release_moq_fabric_runtime(&self, feed_id: &FeedId) -> bool {
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
        let _ = runtime.shutdown_off_worker().await;
        true
    }

    /// Per-shard health across every live MoQ fabric runtime, for
    /// diagnostics and alerting.
    pub(crate) async fn moq_fabric_shard_heartbeats(
        &self,
        stall_after: std::time::Duration,
    ) -> Vec<(FeedId, Vec<crate::egress::shard::EgressShardHeartbeat>)> {
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
            registry.generation = registry.generation.wrapping_add(1);
            registry.feeds.clear();
            registry.active_outputs.clear();
            registry.startup_sources.clear();
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
