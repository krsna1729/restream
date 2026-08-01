use crate::media::egress::command::{EgressCommand, FeedId};
use crate::media::egress::factory::{SrtFabricShardGroupError, spawn_srt_fabric_shard_group};
use crate::media::egress::feed::EgressFeed;
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
            // Share the same local-UDP-port reuse state the legacy SRT
            // egress path uses (`MediaEngine::srt_egress_muxer_port_handle`)
            // so a socket connected by either path can be reused by the
            // other, restoring the libsrt egress-multiplexer sharing the
            // fabric path lost by always passing `None` here — see
            // `docs/egress-implementation.md` Phase 4 status.
            let srt_egress_muxer_port_reuse = self
                .config
                .srt_egress_reuse_local_port
                .then(|| self.srt_egress_muxer_port_handle());
            let group = spawn_srt_fabric_shard_group(
                config.shard_count(),
                config.shard_config(),
                config.srt_poller_max_events,
                config.work_budget(),
                |_| feed.clone_reader(),
                srt_egress_muxer_port_reuse.clone(),
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
            // Shared with `EgressFabricRuntime::rescale` -- see
            // `RtmpFabricRegistry`'s identical comment in
            // engine_rtmp_egress_fabric.rs for why a fixed snapshot here
            // would leave a later-grown shard's leaves without the fast
            // feed-wake path.
            let wake_handles = runtime.feed_wake_handles();
            // Clone the reader side (shared ring + epoch) rather than only
            // the Notify handle: `notify_waiters()` only wakes waiters that
            // were already polling `.notified()` at the moment it fires, so
            // a bare `notify.notified().await` loop can miss the publish
            // that happens between loop iterations. Following the same
            // check-register-recheck-then-await pattern as
            // `Reader::wait_for_data` (src/media/ring_buffer/reader.rs)
            // closes that window: the head is read again after registering
            // interest, so a publish landing in the gap is still observed
            // this iteration instead of being lost until some later push.
            let watcher_feed = feed.clone_reader();
            let notify = watcher_feed.notify_handle();
            let watcher_feed_id = feed_id.clone();
            let watcher = tokio::spawn(async move {
                tracing::info!(feed_id = %watcher_feed_id, "srt fabric wake watcher started");
                let mut last_head = watcher_feed.head_sequence();
                // `last_head`'s pre-loop snapshot can already reflect data
                // published before this task's first poll (e.g. scheduler
                // delay from `EgressFabricRuntime::rescale`'s synchronous
                // shard shutdowns) -- treating that as "already seen" would
                // await a `notify_waiters()` wake that already fired with no
                // registered waiter, hanging forever. `first_iteration`
                // forces the first pass to always fall through to deliver
                // instead of awaiting, regardless of what `last_head` reads.
                let mut first_iteration = true;
                loop {
                    let notified = notify.notified();
                    let current_head = watcher_feed.head_sequence();
                    if current_head == last_head && !first_iteration {
                        notified.await;
                    }
                    first_iteration = false;
                    last_head = watcher_feed.head_sequence();
                    let handles = wake_handles.lock().unwrap().clone();
                    for handle in &handles {
                        let _ = handle.deliver();
                    }
                }
            });

            tracing::info!(feed_id = %feed_id, "srt fabric runtime created");
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry.feed_watchers.insert(feed_id.clone(), watcher);
            registry.feeds.insert(feed_id.clone(), feed.clone_reader());
            registry
                .srt_egress_muxer_port_reuse
                .insert(feed_id.clone(), srt_egress_muxer_port_reuse);
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
        let rescale_inputs = registry
            .feeds
            .get(feed_id)
            .map(|feed| feed.clone_reader())
            .zip(registry.srt_egress_muxer_port_reuse.get(feed_id).cloned());
        let Some(runtime) = registry.runtimes.get_mut(feed_id) else {
            return Err(SrtFabricDispatchError::MissingFeed {
                feed_id: feed_id.clone(),
            });
        };
        let outcome = runtime
            .dispatch(command)
            .map_err(SrtFabricDispatchError::Dispatch)?;

        if let Some((feed, srt_egress_muxer_port_reuse)) = rescale_inputs {
            let config = &self.config.egress_fabric;
            let shard_config = config.shard_config();
            let budget = config.work_budget();
            let poller_max_events = config.srt_poller_max_events;
            let effective_cpus = crate::system_sampling::effective_cpu_count();
            let result = runtime.rescale(effective_cpus, shard_config, |_shard_id| {
                let poller = crate::media::srt::SrtFabricPoller::new(poller_max_events)?;
                Ok::<_, crate::media::srt::SrtEgressPollError>(
                    crate::media::egress::backends::srt::resolve_runtime::resolving_srt_shard_backend(
                        poller,
                        feed.clone_reader(),
                        budget,
                        srt_egress_muxer_port_reuse.clone(),
                        shard_config.drain_timeout(),
                    ),
                )
            });
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

        Ok(outcome)
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
            registry.srt_egress_muxer_port_reuse.remove(feed_id);
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
