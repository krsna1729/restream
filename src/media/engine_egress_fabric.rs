use crate::media::egress::backends::srt::SrtOwnerSettings;
use crate::media::egress::command::{EgressCommand, FeedId};
use crate::media::egress::factory::{SrtFabricShardGroupError, spawn_srt_fabric_shard_group};
use crate::media::egress::journal::TsFeed;
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
pub(crate) enum SrtFabricEnsureError {
    Spawn(SrtFabricShardGroupError<String>),
    Runtime(EgressFabricRuntimeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SrtFabricDispatchError {
    MissingFeed { feed_id: FeedId },
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
}

impl MediaEngine {
    /// Per-shard SRT Owner settings: only the caller pool's `max_in_flight`
    /// (`RESTREAM_SRT_EGRESS_CONNECT_CONCURRENCY`), the transport's connect
    /// admission. The connect TIMEOUT is not an Owner setting: it is each
    /// output's `LeafPolicy.connect_timeout`
    /// (`RESTREAM_SRT_CONNECT_TIMEOUT_MS`), carried on that output's own
    /// `CallerConfig` and counted from pool admission.
    fn srt_owner_settings(&self) -> SrtOwnerSettings {
        SrtOwnerSettings::new(self.config.srt_egress_connect_concurrency)
    }

    pub(crate) async fn retain_srt_fabric_runtime(
        &self,
        feed_id: FeedId,
        feed: &TsFeed,
    ) -> Result<bool, SrtFabricEnsureError> {
        let mut registry = self.fabric.srt.lock().await;
        let created = if registry.runtimes.contains_key(&feed_id) {
            false
        } else {
            let config = &self.config.egress_fabric;
            let group = spawn_srt_fabric_shard_group(
                config.shard_count(),
                config.shard_config(),
                config.work_budget(),
                |_| feed.clone_reader(),
                self.srt_owner_settings(),
            )
            .map_err(SrtFabricEnsureError::Spawn)?;
            let manager_config =
                EgressManagerConfig::new(config.shards, config.command_channel_capacity)
                    .expect("egress fabric manager config is clamped nonzero");
            let runtime = EgressFabricRuntime::new(manager_config, group)
                .map_err(SrtFabricEnsureError::Runtime)?;

            let watcher = spawn_fabric_wake_watcher(
                "srt",
                feed_id.clone(),
                feed.clone_reader(),
                runtime.feed_wake_handles(),
            );

            tracing::info!(feed_id = %feed_id, "srt fabric runtime created");
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry.feed_watchers.insert(feed_id.clone(), watcher);
            registry.feeds.insert(feed_id.clone(), feed.clone_reader());
            true
        };

        let active_outputs = registry.active_outputs.entry(feed_id).or_insert(0);
        *active_outputs = active_outputs.saturating_add(1);
        Ok(created)
    }

    pub(crate) async fn dispatch_srt_fabric_command(
        &self,
        feed_id: &FeedId,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, SrtFabricDispatchError> {
        let mut registry = self.fabric.srt.lock().await;
        // Owned upfront (see `dispatch_rtmp_fabric_command`'s identical
        // comment): disjoint from the `runtimes` borrow below, and no
        // lingering reference into `registry` for `rescale` to hold.
        let rescale_inputs = registry.feeds.get(feed_id).map(|feed| feed.clone_reader());
        let Some(runtime) = registry.runtimes.get_mut(feed_id) else {
            return Err(SrtFabricDispatchError::MissingFeed {
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
            self.rescale_srt_fabric(feed_id, runtime, inputs);
        }

        let outcome = runtime
            .dispatch(command)
            .map_err(SrtFabricDispatchError::Dispatch)?;

        if let Some(inputs) = rescale_inputs.take() {
            self.rescale_srt_fabric(feed_id, runtime, inputs);
        }

        Ok(outcome)
    }

    fn rescale_srt_fabric(
        &self,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        feed: TsFeed,
    ) {
        let config = &self.config.egress_fabric;
        let shard_config = config.shard_config();
        let budget = config.work_budget();
        let effective_cpus = crate::system_sampling::effective_cpu_count();
        let owner_settings = self.srt_owner_settings();
        let result = runtime.rescale(
            crate::config::EgressShardProfile::SrtCpuParallel,
            effective_cpus,
            shard_config,
            |_shard_id| {
                let feed = feed.clone_reader();
                let drain_timeout = shard_config.drain_timeout();
                let leaf_capacity = shard_config.leaf_capacity().get();
                // Built on the new shard's own thread, like the initial spawn;
                // a runtime that cannot be built fails this grow attempt.
                move || {
                    crate::media::egress::backends::srt::resolve_runtime::resolving_srt_shard_backend(
                        feed,
                        budget,
                        drain_timeout,
                        leaf_capacity,
                        owner_settings,
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

    pub(crate) async fn release_srt_fabric_runtime(&self, feed_id: &FeedId) -> bool {
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
    pub(crate) async fn srt_fabric_runtime_snapshots(
        &self,
        feed_id: &FeedId,
    ) -> Option<Vec<EgressShardSnapshot>> {
        let registry = self.fabric.srt.lock().await;
        registry
            .runtimes
            .get(feed_id)
            .map(EgressFabricRuntime::snapshots)
    }

    /// Per-shard health across every live SRT fabric runtime, for
    /// diagnostics and alerting — unlike `srt_fabric_runtime_snapshots`
    /// above, this has real production callers (resource map, alerts) and
    /// is not test-only.
    pub(crate) async fn srt_fabric_shard_heartbeats(
        &self,
        stall_after: std::time::Duration,
    ) -> Vec<(
        FeedId,
        Vec<crate::media::egress::shard::EgressShardHeartbeat>,
    )> {
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
            registry.runtimes.remove(feed_id)?
        };
        Some(runtime.shutdown())
    }

    pub(crate) async fn shutdown_all_srt_fabric_runtimes(&self) -> usize {
        let runtimes = {
            let mut registry = self.fabric.srt.lock().await;
            registry.active_outputs.clear();
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

    #[cfg(test)]
    pub(crate) async fn insert_srt_fabric_runtime_for_test(
        &self,
        feed_id: FeedId,
        runtime: EgressFabricRuntime,
    ) {
        self.fabric
            .srt
            .lock()
            .await
            .runtimes
            .insert(feed_id, runtime);
    }
}
