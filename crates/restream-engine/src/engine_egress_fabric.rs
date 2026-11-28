use crate::egress::backends::srt::SrtOwnerSettings;
use crate::egress::command::{EgressCommand, FeedId};
use crate::egress::factory::{SrtFabricShardGroupError, spawn_srt_fabric_shard_group};
use crate::egress::journal::TsFeed;
use crate::egress::manager::{
    EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome, PlacementPolicy,
};
use crate::egress::policy::WorkBudgetConfig;
use crate::egress::runtime::{
    EgressFabricRuntime, EgressFabricRuntimeError, FabricWorkerError, SharedFabricRuntime,
};
#[cfg(test)]
use crate::egress::shard::EgressShardSnapshot;
use crate::egress::shard::{EgressShardConfig, EgressShardGroupError};
use crate::engine::MediaEngine;

#[derive(Debug, PartialEq, Eq)]
pub enum SrtFabricEnsureError {
    Spawn(SrtFabricShardGroupError<String>),
    Runtime(EgressFabricRuntimeError),
    Worker(FabricWorkerError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrtFabricDispatchError {
    MissingFeed {
        feed_id: FeedId,
    },
    /// Admission refused the new output; nothing was dispatched.
    Saturated(crate::egress::sizing::Saturated),
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
    Worker(FabricWorkerError),
}

impl SrtFabricDispatchError {
    pub fn refused(&self) -> Option<&crate::egress::sizing::Saturated> {
        match self {
            Self::Saturated(saturated) => Some(saturated),
            _ => None,
        }
    }
}

impl MediaEngine {
    /// Per-shard SRT Owner settings: the caller pool's `max_in_flight`
    /// (`RESTREAM_SRT_EGRESS_CONNECT_CONCURRENCY`), the TX window
    /// (`RESTREAM_SRT_EGRESS_TX_CAPACITY`) and UDP GSO coalescing
    /// (`RESTREAM_SRT_EGRESS_GSO`). The connect TIMEOUT is not an Owner
    /// setting: it is each output's `LeafPolicy.connect_timeout`
    /// (`RESTREAM_SRT_CONNECT_TIMEOUT_MS`), carried on that output's own
    /// `CallerConfig` and counted from pool admission.
    fn srt_owner_settings(config: &crate::config::EngineConfig) -> SrtOwnerSettings {
        SrtOwnerSettings::new(config.srt_egress_connect_concurrency)
            .with_tx_capacity(config.srt_egress_tx_capacity)
            .with_tx_gso(config.srt_egress_gso)
    }

    /// `source` is the ring the feed's TS muxer reads. The muxer may have
    /// started with this runtime, so its ring has no rate history yet; the
    /// source's retained window prices the first Adds. That is elementary
    /// payload, a few percent under the TS bytes sent, until the TS ring has
    /// 250 ms of its own history.
    pub async fn retain_srt_fabric_runtime(
        &self,
        feed_id: FeedId,
        feed: &TsFeed,
        source: &std::sync::Arc<restream_sync::RingBuffer>,
    ) -> Result<bool, SrtFabricEnsureError> {
        let generation = {
            let mut registry = self.fabric.srt.lock().await;
            if registry.runtimes.contains_key(&feed_id) {
                let outputs = registry.active_outputs.entry(feed_id).or_insert(0);
                *outputs = outputs.saturating_add(1);
                return Ok(false);
            }
            registry.generation
        };
        let config = self.config.clone();
        let candidate_feed = feed.clone_reader();
        let source = source.clone();
        let candidate = tokio::task::spawn_blocking(move || {
            let owner_settings = Self::srt_owner_settings(&config);
            let config = &config.egress_fabric;
            let group = spawn_srt_fabric_shard_group(
                config.srt_shard_count(),
                EgressShardConfig::from_fabric(config),
                WorkBudgetConfig::from_fabric(config),
                |_| candidate_feed.clone_reader(),
                owner_settings,
            )
            .map_err(SrtFabricEnsureError::Spawn)?;
            let manager_config = EgressManagerConfig::new(
                config.srt_shard_count().get(),
                config.command_channel_capacity,
            )
            .expect("egress fabric manager config is clamped nonzero")
            .with_placement_policy(PlacementPolicy::LeastLoaded);
            let mut runtime = EgressFabricRuntime::new(manager_config, group, config.shards)
                .map_err(SrtFabricEnsureError::Runtime)?;
            runtime.forecast_feed(&source);
            Ok(SharedFabricRuntime::new(runtime))
        })
        .await
        .map_err(|error| {
            SrtFabricEnsureError::Worker(FabricWorkerError::Join(error.to_string()))
        })??;
        let mut registry = self.fabric.srt.lock().await;
        if registry.generation != generation {
            return Err(SrtFabricEnsureError::Worker(FabricWorkerError::Closed));
        }
        let created = !registry.runtimes.contains_key(&feed_id);
        if created {
            let wakes = candidate.subscribe_feed_wakes(
                "srt",
                feed_id.clone(),
                feed.publication_ring().publication_subscribers(),
            );
            registry.runtimes.insert(feed_id.clone(), candidate);
            registry.feed_wakes.insert(feed_id.clone(), wakes);
            registry.feeds.insert(feed_id.clone(), feed.clone_reader());
            tracing::info!(feed_id = %feed_id, "srt fabric runtime created");
        }
        let outputs = registry.active_outputs.entry(feed_id).or_insert(0);
        *outputs = outputs.saturating_add(1);
        Ok(created)
    }

    pub async fn dispatch_srt_fabric_command(
        &self,
        feed_id: &FeedId,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, SrtFabricDispatchError> {
        let (runtime, rescale_inputs) = {
            let registry = self.fabric.srt.lock().await;
            let runtime = registry.runtimes.get(feed_id).cloned().ok_or_else(|| {
                SrtFabricDispatchError::MissingFeed {
                    feed_id: feed_id.clone(),
                }
            })?;
            (
                runtime,
                registry.feeds.get(feed_id).map(TsFeed::clone_reader),
            )
        };
        let config = self.config.clone();
        let feed_id = feed_id.clone();
        runtime
            .with(move |runtime| {
                let mut rescale_inputs = rescale_inputs;
                // Resize before placement/admission so a fresh output connects only once.
                if let EgressCommand::Add(spec) = &command
                    && let Some(inputs) = rescale_inputs.take()
                {
                    let reason = runtime.reason_for(&command);
                    Self::rescale_srt_fabric(&config, &feed_id, runtime, inputs, reason);
                    if matches!(reason, crate::egress::runtime::ResizeReason::Add) {
                        runtime
                            .admit_add(
                                crate::config::EgressShardProfile::SrtOutputCount,
                                EgressShardConfig::from_fabric(&config.egress_fabric),
                                &spec.id,
                            )
                            .map_err(SrtFabricDispatchError::Saturated)?;
                    }
                }
                let outcome = runtime
                    .dispatch(command)
                    .map_err(SrtFabricDispatchError::Dispatch)?;
                if let Some(inputs) = rescale_inputs {
                    Self::rescale_srt_fabric(
                        &config,
                        &feed_id,
                        runtime,
                        inputs,
                        crate::egress::runtime::ResizeReason::Remove,
                    );
                }
                Ok(outcome)
            })
            .await
            .map_err(SrtFabricDispatchError::Worker)?
    }

    pub async fn resize_srt_fabrics(&self) {
        let inputs: Vec<_> = {
            let registry = self.fabric.srt.lock().await;
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
        for (id, runtime, feed) in inputs {
            let config = self.config.clone();
            runtime.try_observe(move |runtime| {
                Self::rescale_srt_fabric(
                    &config,
                    &id,
                    runtime,
                    feed,
                    crate::egress::runtime::ResizeReason::Observe,
                );
            });
        }
    }

    fn rescale_srt_fabric(
        engine_config: &crate::config::EngineConfig,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        feed: TsFeed,
        reason: crate::egress::runtime::ResizeReason,
    ) {
        let config = &engine_config.egress_fabric;
        let shard_config = EgressShardConfig::from_fabric(config);
        let budget = WorkBudgetConfig::from_fabric(config);
        let owner_settings = Self::srt_owner_settings(engine_config);
        runtime.forecast_feed(&feed.publication_ring());
        let result = runtime.rescale(
            crate::config::EgressShardProfile::SrtOutputCount,
            crate::host_capacity::HostCapacity::get(),
            reason,
            shard_config,
            |_shard_id| {
                let feed = feed.clone_reader();
                let drain_timeout = shard_config.drain_timeout();
                let leaf_capacity = shard_config.leaf_capacity().get();
                // Built on the new shard's own thread, like the initial spawn;
                // a runtime that cannot be built fails this grow attempt.
                move || {
                    crate::egress::backends::srt::srt_shard_backend(
                        feed,
                        budget,
                        drain_timeout,
                        leaf_capacity,
                        owner_settings,
                        crate::resolver::shared(),
                    )
                }
            },
        );
        match result {
            Ok(touched) if !touched.is_empty() => {
                tracing::info!(feed_id = %feed_id, shards = ?touched, "srt fabric shard pool rescaled");
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(feed_id = %feed_id, error = ?error, "srt fabric rescale failed to grow a shard");
            }
        }
    }

    pub async fn release_srt_fabric_runtime(&self, feed_id: &FeedId) -> bool {
        let runtime = {
            let mut registry = self.fabric.srt.lock().await;
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
    pub(crate) async fn srt_fabric_runtime_snapshots(
        &self,
        feed_id: &FeedId,
    ) -> Option<Vec<EgressShardSnapshot>> {
        let registry = self.fabric.srt.lock().await;
        registry
            .runtimes
            .get(feed_id)
            .map(SharedFabricRuntime::snapshots)
    }

    /// Per-shard health across every live SRT fabric runtime, for
    /// diagnostics and alerting — unlike `srt_fabric_runtime_snapshots`
    /// above, this has real production callers (resource map, alerts) and
    /// is not test-only.
    pub(crate) async fn srt_fabric_shard_heartbeats(
        &self,
        stall_after: std::time::Duration,
    ) -> Vec<(FeedId, Vec<crate::egress::shard::EgressShardHeartbeat>)> {
        let now = std::time::Instant::now();
        let registry = self.fabric.srt.lock().await;
        registry
            .runtimes
            .iter()
            .map(|(feed_id, runtime)| (feed_id.clone(), runtime.heartbeat(now, stall_after)))
            .collect()
    }

    #[cfg(test)]
    pub(crate) async fn shutdown_srt_fabric_runtime(
        &self,
        feed_id: &FeedId,
    ) -> Option<Vec<EgressShardSnapshot>> {
        let runtime = {
            let mut registry = self.fabric.srt.lock().await;
            registry.active_outputs.remove(feed_id);
            registry.feeds.remove(feed_id);
            registry.feed_wakes.remove(feed_id);
            registry.runtimes.remove(feed_id)?
        };
        Some(runtime.shutdown_off_worker().await)
    }

    pub(crate) async fn shutdown_all_srt_fabric_runtimes(&self) -> usize {
        let runtimes = {
            let mut registry = self.fabric.srt.lock().await;
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

    #[cfg(test)]
    pub(crate) async fn insert_srt_fabric_runtime_for_test(
        &self,
        feed_id: FeedId,
        runtime: EgressFabricRuntime,
    ) {
        let runtime = tokio::task::spawn_blocking(move || SharedFabricRuntime::new(runtime))
            .await
            .expect("test runtime wrapper worker failed");
        self.fabric
            .srt
            .lock()
            .await
            .runtimes
            .insert(feed_id, runtime);
    }
}
