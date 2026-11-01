use crate::egress::backends::sink_shard::SinkShardBackend;
use crate::egress::command::{EgressCommand, FeedId};
use crate::egress::factory::spawn_sink_fabric_shard_group;
use crate::egress::journal::RingFeed;
use crate::egress::manager::{
    EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome, PlacementPolicy,
};
use crate::egress::runtime::{
    EgressFabricRuntime, EgressFabricRuntimeError, ResizeReason, subscribe_fabric_wakes,
};
use crate::egress::shard::EgressShardGroupError;
#[cfg(test)]
use crate::egress::shard::EgressShardSnapshot;
use crate::engine::MediaEngine;

#[derive(Debug, PartialEq, Eq)]
pub enum SinkFabricEnsureError {
    Spawn(EgressShardGroupError),
    Runtime(EgressFabricRuntimeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkFabricDispatchError {
    MissingFeed {
        feed_id: FeedId,
    },
    /// Admission refused the new output; nothing was dispatched.
    Saturated(crate::egress::sizing::Saturated),
    Dispatch(EgressManagerDispatchError<EgressShardGroupError>),
}

impl SinkFabricDispatchError {
    pub fn refused(&self) -> Option<&crate::egress::sizing::Saturated> {
        match self {
            Self::Saturated(saturated) => Some(saturated),
            _ => None,
        }
    }
}

impl MediaEngine {
    pub async fn retain_sink_fabric_runtime(
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
                std::num::NonZeroU32::MIN,
                config.shard_config(),
                config.work_budget(),
                |_| feed.clone_reader(),
            )
            .map_err(SinkFabricEnsureError::Spawn)?;
            // Least-loaded placement, as for the other fabrics: a shard grown
            // for new outputs takes them, and admission's per-shard ceiling
            // can only refuse when every shard is full.
            let manager_config = EgressManagerConfig::new(1, config.command_channel_capacity)
                .expect("egress fabric manager config is clamped nonzero")
                .with_placement_policy(PlacementPolicy::LeastLoaded);
            let runtime = EgressFabricRuntime::new(manager_config, group)
                .map_err(SinkFabricEnsureError::Runtime)?
                .adaptive(config.shards);

            // Sink leaves have no poller at all (see `sink_shard.rs`'s
            // module doc), so this subscription's `FeedWake` delivery is their
            // *only* readiness signal, not just an interest-widening hint
            // the way it is for RTMP/SRT.
            let wakes = subscribe_fabric_wakes(
                "sink",
                feed_id.clone(),
                feed.publication_ring().publication_subscribers(),
                runtime.feed_wake_handles(),
            );

            tracing::info!(feed_id = %feed_id, "sink fabric runtime created");
            registry.runtimes.insert(feed_id.clone(), runtime);
            registry.feed_wakes.insert(feed_id.clone(), wakes);
            registry.feeds.insert(feed_id.clone(), feed.clone_reader());
            true
        };

        let active_outputs = registry.active_outputs.entry(feed_id).or_insert(0);
        *active_outputs = active_outputs.saturating_add(1);
        Ok(created)
    }

    pub async fn dispatch_sink_fabric_command(
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
        if let EgressCommand::Add(spec) = &command
            && let Some(feed) = rescale_feed.take()
        {
            let reason = runtime.reason_for(&command);
            self.rescale_sink_fabric(feed_id, runtime, feed, reason);
            if matches!(reason, ResizeReason::Add) {
                runtime
                    .admit_add(
                        crate::config::EgressShardProfile::SinkOutputCount,
                        self.config.egress_fabric.shard_config(),
                        &spec.id,
                    )
                    .map_err(SinkFabricDispatchError::Saturated)?;
            }
        }

        let outcome = runtime
            .dispatch(command)
            .map_err(SinkFabricDispatchError::Dispatch)?;

        if let Some(feed) = rescale_feed.take() {
            self.rescale_sink_fabric(feed_id, runtime, feed, ResizeReason::Remove);
        }

        Ok(outcome)
    }

    /// The reconciler's periodic look: learn what this feed's sink outputs
    /// cost from a fresh delivery window (the price admission refuses by),
    /// and replace a panicked shard, releasing its outputs to retry (see
    /// `EgressFabricRuntime::rescale`), on an otherwise quiet feed.
    pub async fn resize_sink_fabrics(&self) {
        let mut registry = self.fabric.sink.lock().await;
        let crate::engine_registries::SinkFabricRegistry {
            runtimes, feeds, ..
        } = &mut *registry;
        for (id, runtime) in runtimes {
            if (runtime.observation_due() || runtime.has_panicked_shard())
                && let Some(feed) = feeds.get(id)
            {
                self.rescale_sink_fabric(id, runtime, feed.clone_reader(), ResizeReason::Observe);
            }
        }
    }

    fn rescale_sink_fabric(
        &self,
        feed_id: &FeedId,
        runtime: &mut EgressFabricRuntime,
        feed: RingFeed,
        reason: ResizeReason,
    ) {
        // Price unmeasured outputs at this feed's rate, as the other fabrics do.
        runtime.forecast_feed(&feed.publication_ring());
        let config = &self.config.egress_fabric;
        let shard_config = config.shard_config();
        let budget = config.work_budget();
        let result = runtime.rescale(
            crate::config::EgressShardProfile::SinkOutputCount,
            crate::host_capacity::HostCapacity::get(),
            reason,
            shard_config,
            |_shard_id| {
                let feed = feed.clone_reader();
                let leaf_capacity = shard_config.leaf_capacity().get();
                move || {
                    Ok::<_, std::convert::Infallible>(
                        SinkShardBackend::new(feed, budget).with_leaf_capacity(leaf_capacity),
                    )
                }
            },
        );
        match result {
            Ok(touched) if !touched.is_empty() => {
                tracing::info!(feed_id = %feed_id, shards = ?touched, "sink fabric shard pool rescaled");
            }
            Ok(_) => {}
            Err(error) => match error {},
        }
    }

    pub async fn release_sink_fabric_runtime(&self, feed_id: &FeedId) -> bool {
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
            registry.feed_wakes.remove(feed_id);
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
    ) -> Vec<(FeedId, Vec<crate::egress::shard::EgressShardHeartbeat>)> {
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
            registry.feed_wakes.clear();
            std::mem::take(&mut registry.runtimes)
        };
        let count = runtimes.len();
        for runtime in runtimes.into_values() {
            let _ = runtime.shutdown();
        }
        count
    }
}
