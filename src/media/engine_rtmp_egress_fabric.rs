use std::sync::Arc;

use crate::media::egress::backends::rtmp::RtmpPublishStartup;
use crate::media::egress::backends::rtmp_shard::SharedRtmpPublishStartupSource;
use crate::media::egress::backends::rtmp_shard_resolve_runtime::resolving_rtmp_shard_backend;
use crate::media::egress::backends::tcp::{TcpEgressPollError, TcpEgressPoller};
use crate::media::egress::command::{EgressCommand, FeedId, OutputId};
use crate::media::egress::factory::{RtmpFabricShardGroupError, spawn_rtmp_fabric_shard_group};
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
pub(crate) enum RtmpFabricEnsureError {
    Spawn(RtmpFabricShardGroupError<TcpEgressPollError>),
    Runtime(EgressFabricRuntimeError),
    TrustRoots(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RtmpFabricDispatchError {
    MissingFeed { feed_id: FeedId },
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
}

impl MediaEngine {
    pub(crate) async fn retain_rtmp_fabric_runtime(
        &self,
        feed_id: FeedId,
        feed: &RingFeed,
    ) -> Result<bool, RtmpFabricEnsureError> {
        let mut registry = self.fabric.rtmp.lock().await;
        let created = if registry.runtimes.contains_key(&feed_id) {
            false
        } else {
            let config = &self.config.egress_fabric;
            let startup_source = SharedRtmpPublishStartupSource::new();
            let rtmps_client_config = crate::media::rtmp::resolve_rtmps_client_config(
                self.config.rtmps_extra_trust_roots_pem_path.as_deref(),
            )
            .map_err(RtmpFabricEnsureError::TrustRoots)?;
            let group = spawn_rtmp_fabric_shard_group(
                config.shard_count(),
                config.shard_config(),
                config.srt_poller_max_events,
                config.work_budget(),
                self.config.rtmp_egress_chunk_size,
                rtmps_client_config.clone(),
                startup_source.clone(),
                |_| feed.clone_reader(),
            )
            .map_err(RtmpFabricEnsureError::Spawn)?;
            let manager_config =
                EgressManagerConfig::new(config.shards, config.command_channel_capacity)
                    .expect("egress fabric manager config is clamped nonzero");
            let runtime = EgressFabricRuntime::new(manager_config, group)
                .map_err(RtmpFabricEnsureError::Runtime)?;

            // Bridge feed publications into coalesced shard wakes; same
            // check-register-recheck-then-await pattern as the SRT fabric
            // watcher (`retain_srt_fabric_runtime`) and `Reader::wait_for_data`,
            // closing the lost-wakeup window a bare `notified().await` loop
            // would have.
            // Shared with `EgressFabricRuntime::rescale`: a shard the
            // manager grows after this watcher starts appends its handle
            // here, so the watcher (which reads through the lock every
            // wake rather than a one-time snapshot) picks it up without
            // needing to know shards can resize at all.
            let wake_handles = runtime.feed_wake_handles();
            let watcher_feed = feed.clone_reader();
            let notify = watcher_feed.notify_handle();
            let watcher_feed_id = feed_id.clone();
            let watcher = tokio::spawn(async move {
                tracing::info!(feed_id = %watcher_feed_id, "rtmp fabric wake watcher started");
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

            tracing::info!(feed_id = %feed_id, "rtmp fabric runtime created");
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry
                .startup_sources
                .insert(feed_id.clone(), startup_source);
            registry.feed_watchers.insert(feed_id.clone(), watcher);
            registry.feeds.insert(feed_id.clone(), feed.clone_reader());
            registry
                .rtmps_client_configs
                .insert(feed_id.clone(), rtmps_client_config);
            true
        };

        let active_outputs = registry.active_outputs.entry(feed_id).or_insert(0);
        *active_outputs = active_outputs.saturating_add(1);
        Ok(created)
    }

    /// Write the immutable publish-startup snapshot for `output_id` before
    /// dispatching `EgressCommand::Add`/`Update` for it — the shard thread
    /// only ever reads this via `RtmpPublishStartupSource::take_startup`,
    /// so it must already be present by the time the command lands.
    pub(crate) async fn set_rtmp_publish_startup(
        &self,
        feed_id: &FeedId,
        output_id: OutputId,
        startup: RtmpPublishStartup,
    ) -> bool {
        let registry = self.fabric.rtmp.lock().await;
        let Some(source) = registry.startup_sources.get(feed_id) else {
            return false;
        };
        source.set(output_id, startup);
        true
    }

    pub(crate) async fn dispatch_rtmp_fabric_command(
        &self,
        feed_id: &FeedId,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, RtmpFabricDispatchError> {
        let mut registry = self.fabric.rtmp.lock().await;
        if let EgressCommand::Remove(output_id) = &command
            && let Some(source) = registry.startup_sources.get(feed_id)
        {
            source.remove(output_id);
        }
        // Snapshot what a freshly grown shard would need before taking
        // `runtime` mutably below -- these are separate `registry` fields,
        // so this is disjoint from the `runtimes` borrow, not a conflict.
        let rescale_inputs = registry.startup_sources.get(feed_id).cloned().zip(
            registry
                .feeds
                .get(feed_id)
                .map(|feed| feed.clone_reader())
                .zip(registry.rtmps_client_configs.get(feed_id).cloned()),
        );
        let Some(runtime) = registry.runtimes.get_mut(feed_id) else {
            return Err(RtmpFabricDispatchError::MissingFeed {
                feed_id: feed_id.clone(),
            });
        };

        // `assign_output_to_shard` places an `Add` under whatever shard
        // count is live right now. If rescale below is about to resize the
        // pool anyway, dispatching first would place the output, then
        // immediately rehome it -- for a connection-oriented protocol like
        // RTMP that means tearing down and reconnecting a socket that was
        // just established (surfaced by the live concurrency harness as a
        // spurious extra reconnect right after a fresh output starts).
        // Size the pool for an `Add` *before* dispatching it so it lands on
        // its final shard the first time. `Remove` (and everything else)
        // still rescales after: shrinking once an output count drops
        // doesn't disturb anything live.
        let mut rescale_inputs = rescale_inputs;
        if matches!(command, EgressCommand::Add(_))
            && let Some(inputs) = rescale_inputs.take()
        {
            self.rescale_rtmp_fabric(feed_id, runtime, inputs);
        }

        let outcome = runtime
            .dispatch(command)
            .map_err(RtmpFabricDispatchError::Dispatch)?;

        if let Some(inputs) = rescale_inputs.take() {
            self.rescale_rtmp_fabric(feed_id, runtime, inputs);
        }

        Ok(outcome)
    }

    fn rescale_rtmp_fabric(
        &self,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        (startup_source, (feed, rtmps_client_config)): (
            SharedRtmpPublishStartupSource,
            (RingFeed, Arc<tokio_rustls::rustls::ClientConfig>),
        ),
    ) {
        let config = &self.config.egress_fabric;
        let shard_config = config.shard_config();
        let budget = config.work_budget();
        let chunk_size = self.config.rtmp_egress_chunk_size;
        let poller_max_events = config.srt_poller_max_events;
        let effective_cpus = crate::system_sampling::effective_cpu_count();
        let result = runtime.rescale(effective_cpus, shard_config, |_shard_id| {
            let poller = TcpEgressPoller::new(poller_max_events)?;
            Ok::<_, TcpEgressPollError>(resolving_rtmp_shard_backend(
                poller,
                feed.clone_reader(),
                budget,
                chunk_size,
                rtmps_client_config.clone(),
                startup_source.clone(),
                shard_config.drain_timeout(),
            ))
        });
        match result {
            Ok(touched) if !touched.is_empty() => {
                tracing::info!(feed_id = %feed_id, shards = ?touched, "rtmp fabric shard pool rescaled");
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(feed_id = %feed_id, error = ?error, "rtmp fabric rescale failed to grow a shard");
            }
        }
    }

    pub(crate) async fn release_rtmp_fabric_runtime(&self, feed_id: &FeedId) -> bool {
        let runtime = {
            let mut registry = self.fabric.rtmp.lock().await;
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
            registry.rtmps_client_configs.remove(feed_id);
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
    pub(crate) async fn rtmp_fabric_runtime_snapshots(
        &self,
        feed_id: &FeedId,
    ) -> Option<Vec<EgressShardSnapshot>> {
        let registry = self.fabric.rtmp.lock().await;
        registry
            .runtimes
            .get(feed_id)
            .map(EgressFabricRuntime::snapshots)
    }

    /// Per-shard health across every live RTMP fabric runtime, for
    /// diagnostics and alerting.
    pub(crate) async fn rtmp_fabric_shard_heartbeats(
        &self,
        stall_after: std::time::Duration,
    ) -> Vec<(
        FeedId,
        Vec<crate::media::egress::shard::EgressShardHeartbeat>,
    )> {
        let now = std::time::Instant::now();
        let registry = self.fabric.rtmp.lock().await;
        registry
            .runtimes
            .iter()
            .map(|(feed_id, runtime)| (feed_id.clone(), runtime.heartbeat(now, stall_after)))
            .collect()
    }

    pub(crate) async fn shutdown_all_rtmp_fabric_runtimes(&self) -> usize {
        let runtimes = {
            let mut registry = self.fabric.rtmp.lock().await;
            registry.active_outputs.clear();
            registry.startup_sources.clear();
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
