use crate::media::egress::backends::srt::muxer_ports::SrtEgressMuxerPorts;
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
    /// The key used to scope `SrtEgressMuxerPorts` reuse: the real pipeline
    /// id when `srt_egress_muxer_port_pipeline_scoped` is enabled (the
    /// default), or one fixed key shared by every pipeline when disabled
    /// (the pre-2026-08-14 engine-wide-shared behavior). See the field doc
    /// on `AppConfig::srt_egress_muxer_port_pipeline_scoped`.
    fn srt_egress_muxer_scope_key<'a>(&self, pipeline_id: &'a str) -> &'a str {
        if self.config.srt_egress_muxer_port_pipeline_scoped {
            pipeline_id
        } else {
            ""
        }
    }

    pub(crate) async fn retain_srt_fabric_runtime(
        &self,
        feed_id: FeedId,
        feed: &TsFeed,
        pipeline_id: &str,
    ) -> Result<bool, SrtFabricEnsureError> {
        let mut registry = self.fabric.srt.lock().await;
        let created = if registry.runtimes.contains_key(&feed_id) {
            false
        } else {
            let config = &self.config.egress_fabric;
            // Engine-wide, per-(pipeline, shard) local-UDP-port reuse:
            // leaves on one shard of one pipeline share that shard's libsrt
            // egress multiplexer (and its one `CSndQueue` worker thread),
            // while different shards -- and, when pipeline-scoped, the same
            // shard number on a different pipeline -- get different
            // multiplexers — see `muxer_ports.rs` and
            // `srt_egress_muxer_scope_key`.
            let srt_egress_muxer_port_reuse = self
                .config
                .srt_egress_reuse_local_port
                .then(|| self.srt_egress_muxer_ports_handle());
            let group = spawn_srt_fabric_shard_group(
                self.srt_egress_muxer_scope_key(pipeline_id),
                config.shard_count(),
                config.shard_config(),
                config.srt_poller_max_events,
                config.work_budget(),
                |_| feed.clone_reader(),
                srt_egress_muxer_port_reuse.clone(),
                Some(self.srt_egress_connect_admission_handle()),
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
            registry
                .srt_egress_muxer_port_reuse
                .insert(feed_id.clone(), srt_egress_muxer_port_reuse);
            registry
                .pipeline_ids
                .insert(feed_id.clone(), pipeline_id.to_string());
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
            .zip(registry.srt_egress_muxer_port_reuse.get(feed_id).cloned())
            .zip(registry.pipeline_ids.get(feed_id).cloned())
            .map(|((feed, muxer_reuse), pipeline_id)| (feed, muxer_reuse, pipeline_id));
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
        (feed, srt_egress_muxer_port_reuse, pipeline_id): (
            TsFeed,
            Option<SrtEgressMuxerPorts>,
            String,
        ),
    ) {
        let config = &self.config.egress_fabric;
        let shard_config = config.shard_config();
        let budget = config.work_budget();
        let poller_max_events = config.srt_poller_max_events;
        let effective_cpus = crate::system_sampling::effective_cpu_count();
        let scope_key = self.srt_egress_muxer_scope_key(&pipeline_id).to_string();
        let connect_admission = self.srt_egress_connect_admission_handle();
        let result = runtime.rescale(
            crate::config::EgressShardProfile::SrtCpuParallel,
            effective_cpus,
            shard_config,
            |shard_id| {
                let poller =
                    crate::media::egress::backends::srt::SrtRuntimePoller::new(poller_max_events)?;
                Ok::<_, crate::media::srt::SrtEgressPollError>(
                    crate::media::egress::backends::srt::resolve_runtime::resolving_srt_shard_backend(
                        poller,
                        feed.clone_reader(),
                        budget,
                        // Same per-(pipeline, shard) scoping the initial
                        // `spawn_srt_fabric_shard_group` call uses: a shard
                        // grown by a live rescale claims its own libsrt
                        // multiplexer instead of inheriting another
                        // shard's or another pipeline's.
                        srt_egress_muxer_port_reuse
                            .as_ref()
                            .map(|ports| ports.shard(&scope_key, shard_id)),
                        shard_config.drain_timeout(),
                        // Same shared engine-wide admission handle the
                        // initial spawn uses.
                        Some(connect_admission.clone()),
                    ),
                )
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
            registry.srt_egress_muxer_port_reuse.remove(feed_id);
            registry.pipeline_ids.remove(feed_id);
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
