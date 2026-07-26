use crate::media::egress::command::{EgressCommand, FeedId};
use crate::media::egress::factory::spawn_sink_fabric_shard_group;
use crate::media::egress::feed::EgressFeed;
use crate::media::egress::journal::RingFeed;
use crate::media::egress::manager::{
    EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome,
};
use crate::media::egress::runtime::{EgressFabricRuntime, EgressFabricRuntimeError};
use crate::media::egress::shard::EgressShardGroupError;
#[cfg(test)]
use crate::media::egress::shard::EgressShardSnapshot;
use crate::media::engine::MediaEngine;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SinkFabricEnsureError {
    Spawn(EgressShardGroupError),
    Runtime(EgressFabricRuntimeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SinkFabricDispatchError {
    MissingFeed { feed_id: FeedId },
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
}

impl MediaEngine {
    pub(crate) async fn retain_sink_fabric_runtime(
        &self,
        feed_id: FeedId,
        feed: &RingFeed,
    ) -> Result<bool, SinkFabricEnsureError> {
        let mut registry = self.fabric.sink.lock().await;
        let created = if registry.runtimes.contains_key(&feed_id) {
            false
        } else {
            let config = &self.config.egress_fabric;
            let group = spawn_sink_fabric_shard_group(
                config.shard_count(),
                config.shard_config(),
                config.work_budget(),
                |_| feed.clone_reader(),
            )
            .map_err(SinkFabricEnsureError::Spawn)?;
            let manager_config =
                EgressManagerConfig::new(config.shards, config.command_channel_capacity)
                    .expect("egress fabric manager config is clamped nonzero");
            let runtime = EgressFabricRuntime::new(manager_config, group)
                .map_err(SinkFabricEnsureError::Runtime)?;

            // Bridge feed publications into coalesced shard wakes; same
            // check-register-recheck-then-await pattern as the SRT/RTMP
            // fabric watchers (`retain_srt_fabric_runtime`,
            // `retain_rtmp_fabric_runtime`) and `Reader::wait_for_data`,
            // closing the lost-wakeup window a bare `notified().await` loop
            // would have. Sink leaves have no poller at all (see
            // `sink_shard.rs`'s module doc), so this `FeedWake` delivery is
            // their *only* readiness signal, not just an interest-widening
            // hint the way it is for RTMP.
            let wake_handles = runtime.feed_wake_handles();
            let watcher_feed = feed.clone_reader();
            let notify = watcher_feed.notify_handle();
            let watcher_feed_id = feed_id.clone();
            let watcher = tokio::spawn(async move {
                tracing::info!(feed_id = %watcher_feed_id, shard_count = wake_handles.len(), "sink fabric wake watcher started");
                let mut last_head = watcher_feed.head_sequence();
                loop {
                    let notified = notify.notified();
                    let current_head = watcher_feed.head_sequence();
                    if current_head == last_head {
                        notified.await;
                    }
                    last_head = watcher_feed.head_sequence();
                    for handle in &wake_handles {
                        let _ = handle.deliver();
                    }
                }
            });

            tracing::info!(feed_id = %feed_id, "sink fabric runtime created");
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry.feed_watchers.insert(feed_id.clone(), watcher);
            true
        };

        let active_outputs = registry.active_outputs.entry(feed_id).or_insert(0);
        *active_outputs = active_outputs.saturating_add(1);
        Ok(created)
    }

    pub(crate) async fn dispatch_sink_fabric_command(
        &self,
        feed_id: &FeedId,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, SinkFabricDispatchError> {
        let mut registry = self.fabric.sink.lock().await;
        let Some(runtime) = registry.runtimes.get_mut(feed_id) else {
            return Err(SinkFabricDispatchError::MissingFeed {
                feed_id: feed_id.clone(),
            });
        };
        runtime
            .dispatch(command)
            .map_err(SinkFabricDispatchError::Dispatch)
    }

    pub(crate) async fn release_sink_fabric_runtime(&self, feed_id: &FeedId) -> bool {
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
    pub(crate) async fn sink_fabric_runtime_snapshots(
        &self,
        feed_id: &FeedId,
    ) -> Option<Vec<EgressShardSnapshot>> {
        let registry = self.fabric.sink.lock().await;
        registry
            .runtimes
            .get(feed_id)
            .map(EgressFabricRuntime::snapshots)
    }

    pub(crate) async fn shutdown_all_sink_fabric_runtimes(&self) -> usize {
        let runtimes = {
            let mut registry = self.fabric.sink.lock().await;
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
}
