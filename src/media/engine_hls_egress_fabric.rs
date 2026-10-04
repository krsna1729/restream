//! HLS PUT egress fabric: one shard runtime per HLS store (one per pipeline
//! segmenter), its shards woken by the store's publishes. Mirrors the sink
//! and RTMP fabrics (`engine_sink_egress_fabric.rs`,
//! `engine_rtmp_egress_fabric.rs`).
use std::sync::Arc;

use crate::media::egress::backends::compio_tcp::CompioTcpPoller;
use crate::media::egress::backends::hls_put_shard::HlsPutShardBackend;
use crate::media::egress::command::{EgressCommand, FeedId};
use crate::media::egress::manager::{
    EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome,
};
use crate::media::egress::runtime::{
    EgressFabricRuntime, EgressFabricRuntimeError, ResizeReason, subscribe_fabric_wakes,
};
use crate::media::egress::shard::{
    EgressShardGroup, EgressShardGroupError, EgressShardGroupSpawnError,
};
use crate::media::engine::MediaEngine;
use crate::media::hls::HlsStore;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HlsPutFabricEnsureError {
    Spawn(String),
    Group(EgressShardGroupError),
    Runtime(EgressFabricRuntimeError),
    TrustRoots(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HlsPutFabricDispatchError {
    MissingFeed { feed_id: FeedId },
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
}

/// The fabric feed of a pipeline's HLS PUT outputs.
pub(crate) fn hls_put_feed_id(pipeline_id: &str) -> FeedId {
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
        HlsPutShardBackend::new(poller, store, client_config, leaf_capacity)
            .map_err(|error| error.to_string())
    }
}

impl MediaEngine {
    pub(crate) async fn retain_hls_put_fabric_runtime(
        &self,
        feed_id: FeedId,
        store: &Arc<HlsStore>,
    ) -> Result<bool, HlsPutFabricEnsureError> {
        let mut registry = self.fabric.hls_put.lock().await;
        let created = if registry.runtimes.contains_key(&feed_id) {
            false
        } else {
            let config = &self.config.egress_fabric;
            let client_config = crate::media::egress::tls::resolve_client_config(
                self.config.rtmps_extra_trust_roots_pem_path.as_deref(),
            )
            .map_err(HlsPutFabricEnsureError::TrustRoots)?;
            let shard_config = config.shard_config();
            let shared: Shared = (Arc::clone(store), client_config.clone());
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
                .expect("egress fabric manager config is clamped nonzero");
            let runtime = EgressFabricRuntime::new(manager_config, group)
                .map_err(HlsPutFabricEnsureError::Runtime)?
                .adaptive(config.shards);
            // The store wakes the shards from the segmenter on every publish.
            let wakes = subscribe_fabric_wakes(
                "hls-put",
                feed_id.clone(),
                store.publication_subscribers(),
                runtime.feed_wake_handles(),
            );
            tracing::info!(feed_id = %feed_id, "hls put fabric runtime created");
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry.feed_wakes.insert(feed_id.clone(), wakes);
            registry.shared.insert(feed_id.clone(), shared);
            true
        };
        let active_outputs = registry.active_outputs.entry(feed_id).or_insert(0);
        *active_outputs = active_outputs.saturating_add(1);
        Ok(created)
    }

    pub(crate) async fn dispatch_hls_put_fabric_command(
        &self,
        feed_id: &FeedId,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, HlsPutFabricDispatchError> {
        let mut registry = self.fabric.hls_put.lock().await;
        let shared = registry.shared.get(feed_id).cloned();
        let Some(runtime) = registry.runtimes.get_mut(feed_id) else {
            return Err(HlsPutFabricDispatchError::MissingFeed {
                feed_id: feed_id.clone(),
            });
        };
        let is_add = matches!(command, EgressCommand::Add(_));
        // Size the pool before an Add so it lands on its final shard (see
        // `dispatch_rtmp_fabric_command`).
        if is_add && let Some(shared) = shared.clone() {
            self.rescale_hls_put_fabric(feed_id, runtime, shared, ResizeReason::Add);
        }
        let outcome = runtime
            .dispatch(command)
            .map_err(HlsPutFabricDispatchError::Dispatch)?;
        if !is_add && let Some(shared) = shared {
            self.rescale_hls_put_fabric(feed_id, runtime, shared, ResizeReason::Remove);
        }
        Ok(outcome)
    }

    fn rescale_hls_put_fabric(
        &self,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        shared: Shared,
        reason: ResizeReason,
    ) {
        let config = &self.config.egress_fabric;
        let shard_config = config.shard_config();
        let max_events = config.tcp_poller_max_events;
        let leaf_capacity = shard_config.leaf_capacity().get();
        let result = runtime.rescale(
            crate::config::EgressShardProfile::OutputCount,
            crate::system_sampling::effective_cpu_count(),
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

    pub(crate) async fn release_hls_put_fabric_runtime(&self, feed_id: &FeedId) -> bool {
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
        let _ = runtime.shutdown();
        true
    }

    /// Per-shard health across every live HLS PUT fabric runtime.
    pub(crate) async fn hls_put_fabric_shard_heartbeats(
        &self,
        stall_after: std::time::Duration,
    ) -> Vec<(
        FeedId,
        Vec<crate::media::egress::shard::EgressShardHeartbeat>,
    )> {
        let now = std::time::Instant::now();
        let registry = self.fabric.hls_put.lock().await;
        registry
            .runtimes
            .iter()
            .map(|(feed_id, runtime)| (feed_id.clone(), runtime.heartbeat(now, stall_after)))
            .collect()
    }

    pub(crate) async fn shutdown_all_hls_put_fabric_runtimes(&self) -> usize {
        let runtimes: Vec<_> = {
            let mut registry = self.fabric.hls_put.lock().await;
            registry.active_outputs.clear();
            registry.shared.clear();
            registry.feed_wakes.clear();
            registry
                .runtimes
                .drain()
                .map(|(_, runtime)| runtime)
                .collect()
        };
        let count = runtimes.len();
        for runtime in runtimes {
            let _ = runtime.shutdown();
        }
        count
    }
}
