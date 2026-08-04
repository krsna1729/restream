use crate::media::egress::backends::sink_shard::SinkShardBackend;
use crate::media::egress::command::{EgressCommand, FeedId};
use crate::media::egress::factory::spawn_sink_fabric_shard_group;
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

            // Sink leaves have no poller at all (see `sink_shard.rs`'s
            // module doc), so this watcher's `FeedWake` delivery is their
            // *only* readiness signal, not just an interest-widening hint
            // the way it is for RTMP/SRT.
            let watcher = spawn_fabric_wake_watcher(
                "sink",
                feed_id.clone(),
                feed.clone_reader(),
                runtime.feed_wake_handles(),
            );

            tracing::info!(feed_id = %feed_id, "sink fabric runtime created");
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry.feed_watchers.insert(feed_id.clone(), watcher);
            registry.feeds.insert(feed_id.clone(), feed.clone_reader());
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
        // Owned upfront (see `dispatch_rtmp_fabric_command`'s identical
        // comment): disjoint from the `runtimes` borrow below, and no
        // lingering reference into `registry` for `rescale` to hold.
        let rescale_feed = registry.feeds.get(feed_id).map(|feed| feed.clone_reader());
        let Some(runtime) = registry.runtimes.get_mut(feed_id) else {
            return Err(SinkFabricDispatchError::MissingFeed {
                feed_id: feed_id.clone(),
            });
        };

        // See `dispatch_rtmp_fabric_command`'s identical comment: size the
        // pool for an `Add` before dispatching it so it lands on its final
        // shard the first time, instead of connecting once and immediately
        // getting rehomed onto a different shard.
        let mut rescale_feed = rescale_feed;
        if matches!(command, EgressCommand::Add(_))
            && let Some(feed) = rescale_feed.take()
        {
            self.rescale_sink_fabric(feed_id, runtime, feed);
        }

        let outcome = runtime
            .dispatch(command)
            .map_err(SinkFabricDispatchError::Dispatch)?;

        if let Some(feed) = rescale_feed.take() {
            self.rescale_sink_fabric(feed_id, runtime, feed);
        }

        Ok(outcome)
    }

    fn rescale_sink_fabric(
        &self,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        feed: RingFeed,
    ) {
        let config = &self.config.egress_fabric;
        let shard_config = config.shard_config();
        let budget = config.work_budget();
        let effective_cpus = crate::system_sampling::effective_cpu_count();
        let result = runtime.rescale(effective_cpus, shard_config, |_shard_id| {
            Ok::<_, std::convert::Infallible>(SinkShardBackend::new(feed.clone_reader(), budget))
        });
        match result {
            Ok(touched) if !touched.is_empty() => {
                tracing::info!(feed_id = %feed_id, shards = ?touched, "sink fabric shard pool rescaled");
            }
            Ok(_) => {}
            Err(error) => match error {},
        }
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

    /// Per-shard health across every live sink fabric runtime, for
    /// diagnostics and alerting.
    pub(crate) async fn sink_fabric_shard_heartbeats(
        &self,
        stall_after: std::time::Duration,
    ) -> Vec<(
        FeedId,
        Vec<crate::media::egress::shard::EgressShardHeartbeat>,
    )> {
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
