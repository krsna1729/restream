use crate::media::egress::command::{EgressCommand, FeedId};
use crate::media::egress::factory::{SrtFabricShardGroupError, spawn_srt_fabric_shard_group};
use crate::media::egress::journal::TsFeed;
use crate::media::egress::manager::{
    EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome,
};
use crate::media::egress::runtime::{EgressFabricRuntime, EgressFabricRuntimeError};
use crate::media::egress::shard::EgressShardGroupError;
#[cfg(test)]
use crate::media::egress::shard::EgressShardSnapshot;
use crate::media::engine::MediaEngine;
use crate::media::srt::SrtEgressPollError;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SrtFabricEnsureError {
    Spawn(SrtFabricShardGroupError<SrtEgressPollError>),
    Runtime(EgressFabricRuntimeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SrtFabricDispatchError {
    MissingFeed { feed_id: FeedId },
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
}

impl MediaEngine {
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
                config.srt_poller_max_events,
                config.work_budget(),
                |_| feed.clone_reader(),
            )
            .map_err(SrtFabricEnsureError::Spawn)?;
            let manager_config =
                EgressManagerConfig::new(config.shards, config.command_channel_capacity)
                    .expect("egress fabric manager config is clamped nonzero");
            let runtime = EgressFabricRuntime::new(manager_config, group)
                .map_err(SrtFabricEnsureError::Runtime)?;

            // Bridge feed publications into coalesced shard wakes: one
            // watcher per feed runtime, one gate per shard, at most one
            // outstanding wake per (feed, shard).
            let wake_handles = runtime.feed_wake_handles();
            let notify = feed.notify_handle();
            let watcher = tokio::spawn(async move {
                loop {
                    notify.notified().await;
                    for handle in &wake_handles {
                        let _ = handle.deliver();
                    }
                }
            });

            registry.runtimes.insert(feed_id.clone(), runtime);
            registry.feed_watchers.insert(feed_id.clone(), watcher);
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
        let Some(runtime) = registry.runtimes.get_mut(feed_id) else {
            return Err(SrtFabricDispatchError::MissingFeed {
                feed_id: feed_id.clone(),
            });
        };
        runtime
            .dispatch(command)
            .map_err(SrtFabricDispatchError::Dispatch)
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
