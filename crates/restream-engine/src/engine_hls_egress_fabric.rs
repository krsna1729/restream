//! HLS PUT egress fabric: one shard runtime per HLS store (one per pipeline
//! segmenter), its shards woken by the store's publishes. Mirrors the sink
//! and RTMP fabrics (`engine_sink_egress_fabric.rs`,
//! `engine_rtmp_egress_fabric.rs`).
use std::sync::Arc;

use crate::egress::backends::compio_tcp::CompioTcpPoller;
use crate::egress::backends::hls_put_shard::HlsPutShardBackend;
use crate::egress::command::{EgressCommand, FeedId};
use crate::egress::manager::{
    EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome, PlacementPolicy,
};
use crate::egress::runtime::{
    EgressFabricRuntime, EgressFabricRuntimeError, FabricWorkerError, ResizeReason,
    SharedFabricRuntime,
};
use crate::egress::shard::{
    EgressShardConfig, EgressShardGroup, EgressShardGroupError, EgressShardGroupSpawnError,
};
use crate::engine::MediaEngine;
use crate::hls::HlsStore;

#[derive(Debug, PartialEq, Eq)]
pub enum HlsPutFabricEnsureError {
    Spawn(String),
    Group(EgressShardGroupError),
    Runtime(EgressFabricRuntimeError),
    TrustRoots(String),
    Worker(FabricWorkerError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HlsPutFabricDispatchError {
    MissingFeed {
        feed_id: FeedId,
    },
    /// Admission refused the new output; nothing was dispatched.
    Saturated(crate::egress::sizing::Saturated),
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
    Worker(FabricWorkerError),
}

impl HlsPutFabricDispatchError {
    pub fn refused(&self) -> Option<&crate::egress::sizing::Saturated> {
        match self {
            Self::Saturated(saturated) => Some(saturated),
            _ => None,
        }
    }
}

/// The fabric feed of a pipeline's HLS PUT outputs.
pub fn hls_put_feed_id(pipeline_id: &str) -> FeedId {
    FeedId::new(format!("hls:{pipeline_id}"))
}

type Shared = (Arc<HlsStore>, Arc<tokio_rustls::rustls::ClientConfig>);

/// A factory building one shard's backend on the shard's own thread (the
/// Compio runtime is thread-affine).
fn backend_factory(
    (store, client_config): Shared,
    max_events: usize,
    leaf_capacity: usize,
) -> impl FnOnce() -> Result<HlsPutShardBackend<CompioTcpPoller>, String> + Send + 'static {
    move || {
        let poller = CompioTcpPoller::new(max_events).map_err(|error| error.message)?;
        Ok(HlsPutShardBackend::new(
            poller,
            store,
            client_config,
            leaf_capacity,
        ))
    }
}

impl MediaEngine {
    pub async fn retain_hls_put_fabric_runtime(
        &self,
        feed_id: FeedId,
        store: &Arc<HlsStore>,
    ) -> Result<bool, HlsPutFabricEnsureError> {
        let generation = {
            let mut registry = self.fabric.hls_put.lock().await;
            if registry.runtimes.contains_key(&feed_id) {
                let outputs = registry.active_outputs.entry(feed_id).or_insert(0);
                *outputs = outputs.saturating_add(1);
                return Ok(false);
            }
            registry.generation
        };
        let engine_config = self.config.clone();
        let candidate_store = store.clone();
        let candidate = tokio::task::spawn_blocking(move || {
            let config = &engine_config.egress_fabric;
            let client_config = crate::egress::tls::resolve_client_config(
                engine_config.rtmps_extra_trust_roots_pem_path.as_deref(),
            )
            .map_err(HlsPutFabricEnsureError::TrustRoots)?;
            let shard_config = EgressShardConfig::from_fabric(config);
            let shared: Shared = (candidate_store, client_config);
            let max_events = config.tcp_poller_max_events;
            let leaf_capacity = shard_config.leaf_capacity().get();
            let group =
                EgressShardGroup::try_spawn_with(std::num::NonZeroU32::MIN, shard_config, |_| {
                    backend_factory(shared.clone(), max_events, leaf_capacity)
                })
                .map_err(|error| match error {
                    EgressShardGroupSpawnError::Backend(error) => {
                        HlsPutFabricEnsureError::Spawn(error)
                    }
                    EgressShardGroupSpawnError::Group(error) => {
                        HlsPutFabricEnsureError::Group(error)
                    }
                })?;
            let manager_config = EgressManagerConfig::new(1, config.command_channel_capacity)
                .expect("egress fabric manager config is clamped nonzero")
                .with_placement_policy(PlacementPolicy::LeastLoaded);
            let runtime = EgressFabricRuntime::new(manager_config, group, config.shards)
                .map_err(HlsPutFabricEnsureError::Runtime)?;
            Ok((SharedFabricRuntime::new(runtime), shared))
        })
        .await
        .map_err(|error| {
            HlsPutFabricEnsureError::Worker(FabricWorkerError::Join(error.to_string()))
        })??;
        let (runtime, shared) = candidate;
        let mut registry = self.fabric.hls_put.lock().await;
        if registry.generation != generation {
            return Err(HlsPutFabricEnsureError::Worker(FabricWorkerError::Closed));
        }
        let created = !registry.runtimes.contains_key(&feed_id);
        if created {
            let wakes = runtime.subscribe_feed_wakes(
                "hls-put",
                feed_id.clone(),
                store.publication_subscribers(),
            );
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry.feed_wakes.insert(feed_id.clone(), wakes);
            registry.shared.insert(feed_id.clone(), shared);
            tracing::info!(feed_id = %feed_id, "hls put fabric runtime created");
        }
        let outputs = registry.active_outputs.entry(feed_id).or_insert(0);
        *outputs = outputs.saturating_add(1);
        Ok(created)
    }

    pub async fn dispatch_hls_put_fabric_command(
        &self,
        feed_id: &FeedId,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, HlsPutFabricDispatchError> {
        let (runtime, shared) = {
            let registry = self.fabric.hls_put.lock().await;
            let runtime = registry.runtimes.get(feed_id).cloned().ok_or_else(|| {
                HlsPutFabricDispatchError::MissingFeed {
                    feed_id: feed_id.clone(),
                }
            })?;
            (runtime, registry.shared.get(feed_id).cloned())
        };
        let config = self.config.clone();
        let feed_id = feed_id.clone();
        runtime
            .with(move |runtime| {
                let is_add = matches!(command, EgressCommand::Add(_));
                // Resize before placement/admission; reconnecting a fresh output is wasteful.
                if let EgressCommand::Add(spec) = &command
                    && let Some(shared) = shared.clone()
                {
                    let reason = runtime.reason_for(&command);
                    Self::rescale_hls_put_fabric(&config, &feed_id, runtime, shared, reason);
                    if matches!(reason, ResizeReason::Add) {
                        runtime
                            .admit_add(
                                crate::config::EgressShardProfile::HlsPutOutputCount,
                                EgressShardConfig::from_fabric(&config.egress_fabric),
                                &spec.id,
                            )
                            .map_err(HlsPutFabricDispatchError::Saturated)?;
                    }
                }
                let outcome = runtime
                    .dispatch(command)
                    .map_err(HlsPutFabricDispatchError::Dispatch)?;
                if !is_add && let Some(shared) = shared {
                    Self::rescale_hls_put_fabric(
                        &config,
                        &feed_id,
                        runtime,
                        shared,
                        ResizeReason::Remove,
                    );
                }
                Ok(outcome)
            })
            .await
            .map_err(HlsPutFabricDispatchError::Worker)?
    }

    /// The reconciler's periodic look: learn what this feed's HLS PUT
    /// outputs cost from fresh segment-delivery windows (the price admission
    /// refuses by), and replace a panicked shard, releasing its outputs to
    /// retry (see `EgressFabricRuntime::rescale`), on an otherwise quiet feed.
    pub async fn resize_hls_put_fabrics(&self) {
        let inputs: Vec<_> = {
            let registry = self.fabric.hls_put.lock().await;
            registry
                .runtimes
                .iter()
                .filter_map(|(id, runtime)| {
                    Some((
                        id.clone(),
                        runtime.clone(),
                        registry.shared.get(id)?.clone(),
                    ))
                })
                .collect()
        };
        for (id, runtime, inputs) in inputs {
            let config = self.config.clone();
            runtime.try_observe(move |runtime| {
                Self::rescale_hls_put_fabric(&config, &id, runtime, inputs, ResizeReason::Observe);
            });
        }
    }

    fn rescale_hls_put_fabric(
        engine_config: &crate::config::EngineConfig,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        shared: Shared,
        reason: ResizeReason,
    ) {
        let config = &engine_config.egress_fabric;
        let shard_config = EgressShardConfig::from_fabric(config);
        let max_events = config.tcp_poller_max_events;
        let leaf_capacity = shard_config.leaf_capacity().get();
        let result = runtime.rescale(
            crate::config::EgressShardProfile::HlsPutOutputCount,
            crate::host_capacity::HostCapacity::get(),
            reason,
            shard_config,
            |_| backend_factory(shared.clone(), max_events, leaf_capacity),
        );
        match result {
            Ok(touched) if !touched.is_empty() => {
                tracing::info!(feed_id = %feed_id, shards = ?touched, "hls put fabric shard pool rescaled");
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(feed_id = %feed_id, error = %error, "hls put fabric rescale failed to grow a shard");
            }
        }
    }

    pub async fn release_hls_put_fabric_runtime(&self, feed_id: &FeedId) -> bool {
        let runtime = {
            let mut registry = self.fabric.hls_put.lock().await;
            let Some(active_outputs) = registry.active_outputs.get_mut(feed_id) else {
                return false;
            };
            *active_outputs = active_outputs.saturating_sub(1);
            if *active_outputs > 0 {
                return false;
            }
            registry.active_outputs.remove(feed_id);
            registry.shared.remove(feed_id);
            registry.feed_wakes.remove(feed_id);
            registry.runtimes.remove(feed_id)
        };
        let Some(runtime) = runtime else {
            return false;
        };
        let _ = runtime.shutdown_off_worker().await;
        true
    }

    /// Per-shard health across every live HLS PUT fabric runtime.
    pub(crate) async fn hls_put_fabric_shard_heartbeats(
        &self,
        stall_after: std::time::Duration,
    ) -> Vec<(FeedId, Vec<crate::egress::shard::EgressShardHeartbeat>)> {
        let now = std::time::Instant::now();
        let registry = self.fabric.hls_put.lock().await;
        registry
            .runtimes
            .iter()
            .map(|(feed_id, runtime)| (feed_id.clone(), runtime.heartbeat(now, stall_after)))
            .collect()
    }

    pub(crate) async fn shutdown_all_hls_put_fabric_runtimes(&self) -> usize {
        let runtimes = {
            let mut registry = self.fabric.hls_put.lock().await;
            registry.generation = registry.generation.wrapping_add(1);
            registry.active_outputs.clear();
            registry.shared.clear();
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
