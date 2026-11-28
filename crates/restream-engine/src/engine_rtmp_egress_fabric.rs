use std::sync::Arc;

use crate::egress::backends::compio_tcp::CompioTcpPoller;
use crate::egress::backends::rtmp::RtmpPublishStartup;
use crate::egress::backends::rtmp_shard::RtmpShardBackend;
use crate::egress::backends::rtmp_shard::SharedRtmpPublishStartupSource;
use crate::egress::backends::tcp::TcpEgressPollError;
use crate::egress::command::{EgressCommand, FeedId, OutputId};
use crate::egress::factory::{RtmpFabricShardGroupError, spawn_rtmp_fabric_shard_group};
use crate::egress::journal::RingFeed;
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
pub enum RtmpFabricEnsureError {
    Spawn(RtmpFabricShardGroupError<TcpEgressPollError>),
    Runtime(EgressFabricRuntimeError),
    TrustRoots(String),
    Worker(FabricWorkerError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtmpFabricDispatchError {
    MissingFeed {
        feed_id: FeedId,
    },
    /// Admission refused the new output; nothing was dispatched.
    Saturated(crate::egress::sizing::Saturated),
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
    Worker(FabricWorkerError),
}

impl RtmpFabricDispatchError {
    pub fn refused(&self) -> Option<&crate::egress::sizing::Saturated> {
        match self {
            Self::Saturated(saturated) => Some(saturated),
            _ => None,
        }
    }
}

impl MediaEngine {
    pub async fn retain_rtmp_fabric_runtime(
        &self,
        feed_id: FeedId,
        feed: &RingFeed,
    ) -> Result<bool, RtmpFabricEnsureError> {
        let generation = {
            let mut registry = self.fabric.rtmp.lock().await;
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
            let startup_source = SharedRtmpPublishStartupSource::new();
            let rtmps_client_config = crate::egress::tls::resolve_client_config(
                engine_config.rtmps_extra_trust_roots_pem_path.as_deref(),
            )
            .map_err(RtmpFabricEnsureError::TrustRoots)?;
            let group = spawn_rtmp_fabric_shard_group(
                std::num::NonZeroU32::MIN,
                EgressShardConfig::from_fabric(config),
                config.tcp_poller_max_events,
                WorkBudgetConfig::from_fabric(config),
                engine_config.rtmp_egress_chunk_size,
                rtmps_client_config.clone(),
                startup_source.clone(),
                |_| candidate_feed.clone_reader(),
            )
            .map_err(RtmpFabricEnsureError::Spawn)?;
            let manager_config = EgressManagerConfig::new(1, config.command_channel_capacity)
                .expect("egress fabric manager config is clamped nonzero")
                .with_placement_policy(PlacementPolicy::LeastLoaded);
            let runtime = EgressFabricRuntime::new(manager_config, group, config.shards)
                .map_err(RtmpFabricEnsureError::Runtime)?;
            Ok((
                SharedFabricRuntime::new(runtime),
                startup_source,
                rtmps_client_config,
            ))
        })
        .await
        .map_err(|error| {
            RtmpFabricEnsureError::Worker(FabricWorkerError::Join(error.to_string()))
        })??;
        let (runtime, startup_source, rtmps_client_config) = candidate;
        let mut registry = self.fabric.rtmp.lock().await;
        if registry.generation != generation {
            return Err(RtmpFabricEnsureError::Worker(FabricWorkerError::Closed));
        }
        let created = !registry.runtimes.contains_key(&feed_id);
        if created {
            let wakes = runtime.subscribe_feed_wakes(
                "rtmp",
                feed_id.clone(),
                feed.publication_ring().publication_subscribers(),
            );
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry.feed_wakes.insert(feed_id.clone(), wakes);
            registry
                .startup_sources
                .insert(feed_id.clone(), startup_source);
            registry.feeds.insert(feed_id.clone(), feed.clone_reader());
            registry
                .rtmps_client_configs
                .insert(feed_id.clone(), rtmps_client_config);
            tracing::info!(feed_id = %feed_id, "rtmp fabric runtime created");
        }
        let outputs = registry.active_outputs.entry(feed_id).or_insert(0);
        *outputs = outputs.saturating_add(1);
        Ok(created)
    }

    /// Write the immutable publish-startup snapshot for `output_id` before
    /// dispatching `EgressCommand::Add`/`Update` for it — the shard thread
    /// only ever reads this via `RtmpPublishStartupSource::take_startup`,
    /// so it must already be present by the time the command lands.
    pub async fn set_rtmp_publish_startup(
        &self,
        feed_id: &FeedId,
        output_id: OutputId,
        startup: RtmpPublishStartup,
    ) -> bool {
        let (runtime, source) = {
            let registry = self.fabric.rtmp.lock().await;
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

    pub async fn dispatch_rtmp_fabric_command(
        &self,
        feed_id: &FeedId,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, RtmpFabricDispatchError> {
        let (runtime, source, rescale_inputs) = {
            let registry = self.fabric.rtmp.lock().await;
            let runtime = registry.runtimes.get(feed_id).cloned().ok_or_else(|| {
                RtmpFabricDispatchError::MissingFeed {
                    feed_id: feed_id.clone(),
                }
            })?;
            let source = registry.startup_sources.get(feed_id).cloned();
            let inputs = source.clone().zip(
                registry
                    .feeds
                    .get(feed_id)
                    .map(RingFeed::clone_reader)
                    .zip(registry.rtmps_client_configs.get(feed_id).cloned()),
            );
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
                // Resize before placement; a fresh TCP connection must not be rehomed.
                if let EgressCommand::Add(spec) = &command
                    && let Some(inputs) = rescale_inputs.take()
                {
                    let reason = runtime.reason_for(&command);
                    let source = inputs.0.clone();
                    Self::rescale_rtmp_fabric(&config, &feed_id, runtime, inputs, reason);
                    if matches!(reason, crate::egress::runtime::ResizeReason::Add)
                        && let Err(saturated) = runtime.admit_add(
                            crate::config::EgressShardProfile::OutputCount,
                            EgressShardConfig::from_fabric(&config.egress_fabric),
                            &spec.id,
                        )
                    {
                        source.remove(&spec.id);
                        return Err(RtmpFabricDispatchError::Saturated(saturated));
                    }
                    startup = Some((source, spec.id.clone()));
                }
                let outcome = runtime.dispatch(command).map_err(|error| {
                    if let Some((source, output_id)) = &startup {
                        source.remove(output_id);
                    }
                    RtmpFabricDispatchError::Dispatch(error)
                })?;
                if let Some(inputs) = rescale_inputs {
                    Self::rescale_rtmp_fabric(
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
            .map_err(RtmpFabricDispatchError::Worker)?
    }

    pub async fn resize_rtmp_fabrics(&self) {
        let inputs: Vec<_> = {
            let registry = self.fabric.rtmp.lock().await;
            registry
                .runtimes
                .iter()
                .filter_map(|(id, runtime)| {
                    Some((
                        id.clone(),
                        runtime.clone(),
                        (
                            registry.startup_sources.get(id)?.clone(),
                            (
                                registry.feeds.get(id)?.clone_reader(),
                                registry.rtmps_client_configs.get(id)?.clone(),
                            ),
                        ),
                    ))
                })
                .collect()
        };
        for (id, runtime, inputs) in inputs {
            let config = self.config.clone();
            runtime.try_observe(move |runtime| {
                Self::rescale_rtmp_fabric(
                    &config,
                    &id,
                    runtime,
                    inputs,
                    crate::egress::runtime::ResizeReason::Observe,
                );
            });
        }
    }

    fn rescale_rtmp_fabric(
        engine_config: &crate::config::EngineConfig,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        (startup_source, (feed, rtmps_client_config)): (
            SharedRtmpPublishStartupSource,
            (RingFeed, Arc<tokio_rustls::rustls::ClientConfig>),
        ),
        reason: crate::egress::runtime::ResizeReason,
    ) {
        let config = &engine_config.egress_fabric;
        let shard_config = EgressShardConfig::from_fabric(config);
        let budget = WorkBudgetConfig::from_fabric(config);
        let chunk_size = engine_config.rtmp_egress_chunk_size;
        let poller_max_events = config.tcp_poller_max_events;
        runtime.forecast_feed(&feed.publication_ring());
        let result = runtime.rescale(
            crate::config::EgressShardProfile::OutputCount,
            crate::host_capacity::HostCapacity::get(),
            reason,
            shard_config,
            |_shard_id| {
                let feed = feed.clone_reader();
                let rtmps_client_config = rtmps_client_config.clone();
                let startup_source = startup_source.clone();
                let drain_timeout = shard_config.drain_timeout();
                let leaf_capacity = shard_config.leaf_capacity().get();
                move || {
                    let poller = CompioTcpPoller::new(poller_max_events)?;
                    Ok::<_, TcpEgressPollError>(
                        RtmpShardBackend::with_runtime_components(
                            poller,
                            feed,
                            budget,
                            chunk_size,
                            rtmps_client_config,
                            Some(crate::resolver::shared()),
                            startup_source,
                        )
                        .with_leaf_capacity(leaf_capacity)
                        .with_drain_timeout(drain_timeout),
                    )
                }
            },
        );
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

    pub async fn release_rtmp_fabric_runtime(&self, feed_id: &FeedId) -> bool {
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
    pub(crate) async fn rtmp_fabric_runtime_snapshots(
        &self,
        feed_id: &FeedId,
    ) -> Option<Vec<EgressShardSnapshot>> {
        let registry = self.fabric.rtmp.lock().await;
        registry
            .runtimes
            .get(feed_id)
            .map(SharedFabricRuntime::snapshots)
    }

    /// Per-shard health across every live RTMP fabric runtime, for
    /// diagnostics and alerting.
    pub(crate) async fn rtmp_fabric_shard_heartbeats(
        &self,
        stall_after: std::time::Duration,
    ) -> Vec<(FeedId, Vec<crate::egress::shard::EgressShardHeartbeat>)> {
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
            registry.generation = registry.generation.wrapping_add(1);
            registry.feeds.clear();
            registry.rtmps_client_configs.clear();
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
