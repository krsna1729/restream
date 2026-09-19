use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use crate::media::egress::command::{EgressCommand, ShardId};
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardConfig, EgressShardHandle, EgressShardSendError,
    EgressShardSnapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressShardGroupError {
    ShardCountTooLarge,
    BackendCountMismatch {
        expected: usize,
        actual: usize,
    },
    UnknownShard {
        shard_id: ShardId,
    },
    SendFailed {
        shard_id: ShardId,
        source: EgressShardSendError,
    },
}

/// Failure of [`EgressShardGroup::try_spawn_with`]: either the group itself
/// could not be assembled or a shard's backend factory failed.
#[derive(Debug, PartialEq, Eq)]
pub enum EgressShardGroupSpawnError<E> {
    Backend(E),
    Group(EgressShardGroupError),
}

#[derive(Debug)]
pub struct EgressShardGroup {
    handles: Vec<EgressShardHandle>,
}

impl EgressShardGroup {
    pub fn spawn<B: EgressShardBackend + Send>(
        shard_count: NonZeroU32,
        config: EgressShardConfig,
        backends: Vec<B>,
    ) -> Result<Self, EgressShardGroupError> {
        let expected = usize::try_from(shard_count.get())
            .map_err(|_| EgressShardGroupError::ShardCountTooLarge)?;
        if backends.len() != expected {
            return Err(EgressShardGroupError::BackendCountMismatch {
                expected,
                actual: backends.len(),
            });
        }
        let mut handles = Vec::with_capacity(expected);
        for (index, backend) in backends.into_iter().enumerate() {
            let shard_index =
                u32::try_from(index).map_err(|_| EgressShardGroupError::ShardCountTooLarge)?;
            handles.push(EgressShardHandle::spawn(
                ShardId::new(shard_index),
                config,
                backend,
            ));
        }
        Ok(Self { handles })
    }

    /// Spawn `shard_count` shards, each building its backend on its own
    /// shard thread from the factory `factory_for(shard_id)` returns. The
    /// backends need not be `Send`; only the factories cross threads.
    pub fn spawn_with<B, F, G>(
        shard_count: NonZeroU32,
        config: EgressShardConfig,
        mut factory_for: G,
    ) -> Result<Self, EgressShardGroupError>
    where
        B: EgressShardBackend,
        F: FnOnce() -> B + Send + 'static,
        G: FnMut(ShardId) -> F,
    {
        match Self::try_spawn_with(shard_count, config, |shard_id| {
            let factory = factory_for(shard_id);
            move || Ok::<B, std::convert::Infallible>(factory())
        }) {
            Ok(group) => Ok(group),
            Err(EgressShardGroupSpawnError::Group(error)) => Err(error),
            Err(EgressShardGroupSpawnError::Backend(never)) => match never {},
        }
    }

    /// [`Self::spawn_with`] for fallible factories. A construction error
    /// shuts down the shards already started and is returned as-is.
    pub fn try_spawn_with<B, E, F, G>(
        shard_count: NonZeroU32,
        config: EgressShardConfig,
        mut factory_for: G,
    ) -> Result<Self, EgressShardGroupSpawnError<E>>
    where
        B: EgressShardBackend,
        E: Send + 'static,
        F: FnOnce() -> Result<B, E> + Send + 'static,
        G: FnMut(ShardId) -> F,
    {
        let expected = usize::try_from(shard_count.get()).map_err(|_| {
            EgressShardGroupSpawnError::Group(EgressShardGroupError::ShardCountTooLarge)
        })?;
        let mut handles = Vec::with_capacity(expected);
        for index in 0..shard_count.get() {
            let shard_id = ShardId::new(index);
            match EgressShardHandle::try_spawn_with(shard_id, config, factory_for(shard_id)) {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    for handle in handles {
                        let _ = handle.shutdown_and_join();
                    }
                    return Err(EgressShardGroupSpawnError::Backend(error));
                }
            }
        }
        Ok(Self { handles })
    }

    pub fn feed_wake_handles(&self) -> Vec<super::FeedWakeHandle> {
        self.handles
            .iter()
            .map(|handle| handle.feed_wake_handle())
            .collect()
    }

    pub fn shard_count(&self) -> usize {
        self.handles.len()
    }

    pub fn try_send_to(
        &self,
        shard_id: ShardId,
        command: EgressCommand,
    ) -> Result<(), EgressShardGroupError> {
        let Ok(index) = usize::try_from(shard_id.index()) else {
            return Err(EgressShardGroupError::UnknownShard { shard_id });
        };
        let Some(handle) = self.handles.get(index) else {
            return Err(EgressShardGroupError::UnknownShard { shard_id });
        };
        handle
            .try_send(command)
            .map_err(|source| EgressShardGroupError::SendFailed { shard_id, source })
    }

    pub fn snapshots(&self) -> Vec<EgressShardSnapshot> {
        self.handles
            .iter()
            .map(EgressShardHandle::snapshot)
            .collect()
    }

    pub fn heartbeat(&self, now: Instant, stall_after: Duration) -> Vec<EgressShardHeartbeat> {
        self.snapshots()
            .into_iter()
            .map(|snapshot| EgressShardHeartbeat::from_snapshot(snapshot, now, stall_after))
            .collect()
    }

    pub fn replace_panicked<B, F, G>(
        &mut self,
        config: EgressShardConfig,
        mut factory_for: G,
    ) -> Vec<ShardId>
    where
        B: EgressShardBackend,
        F: FnOnce() -> B + Send + 'static,
        G: FnMut(ShardId) -> F,
    {
        let mut replaced = Vec::new();
        for handle in &mut self.handles {
            let snapshot = handle.snapshot();
            if !snapshot.panicked {
                continue;
            }
            let shard_id = snapshot.shard_id;
            let replacement =
                EgressShardHandle::spawn_with(shard_id, config, factory_for(shard_id));
            let old = std::mem::replace(handle, replacement);
            let _ = old.shutdown_and_join();
            replaced.push(shard_id);
        }
        replaced
    }

    /// Add one shard at the next index, building its backend on the new
    /// shard thread with `factory`. Used for output-count-driven scale-out
    /// (`EgressFabricRuntime::rescale`) — mirrors `replace_panicked`'s spawn
    /// shape, but appends a new handle instead of replacing one in place.
    /// A construction error leaves the group unchanged.
    pub fn grow_with<B, E, F>(
        &mut self,
        config: EgressShardConfig,
        factory: F,
    ) -> Result<ShardId, E>
    where
        B: EgressShardBackend,
        E: Send + 'static,
        F: FnOnce() -> Result<B, E> + Send + 'static,
    {
        let shard_id = ShardId::new(u32::try_from(self.handles.len()).unwrap_or(u32::MAX));
        self.handles.push(EgressShardHandle::try_spawn_with(
            shard_id, config, factory,
        )?);
        Ok(shard_id)
    }

    /// Remove and gracefully shut down the highest-index shard, if any.
    /// Used for output-count-driven scale-in. The caller is responsible
    /// for rehoming whatever outputs were assigned to this shard
    /// (`EgressManager::rehome`) — this only tears the shard thread down,
    /// draining any leaves it still owned per its own `Shutdown` handling
    /// (`EgressShardRuntime::run`'s drain window), it does not reassign
    /// them anywhere.
    ///
    /// Detaches rather than joins the shard thread (see
    /// `EgressShardHandle::shutdown_detached`'s doc comment): the shard is
    /// routable-unreachable immediately (removed from `handles` before
    /// this returns), but its graceful drain continues in the background
    /// rather than blocking this call for up to `drain_timeout`.
    pub fn shrink(&mut self) -> Option<ShardId> {
        let handle = self.handles.pop()?;
        let shard_id = handle.shard_id();
        handle.shutdown_detached();
        Some(shard_id)
    }

    pub fn shutdown_and_join(self) -> Vec<EgressShardSnapshot> {
        self.handles
            .into_iter()
            .map(EgressShardHandle::shutdown_and_join)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressShardHeartbeat {
    pub shard_id: ShardId,
    pub state: EgressShardHealth,
    pub loop_iterations: u64,
    pub media_ticks: u64,
    pub progress_age: Option<Duration>,
    pub command_depth: u32,
    pub command_capacity: u32,
    pub resync_count: u64,
    pub ready_depth: u32,
    pub ready_depth_hwm: u32,
    pub ready_visits: u64,
    pub budget_exhaustions: u64,
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub cqes: u64,
    pub sqes: u64,
    pub stale_completions: u64,
    pub rx_pool_empty: u64,
    pub tx_pool_empty: u64,
    pub send_zc_attempts: u64,
    pub send_zc_fallbacks: u64,
    pub cq_overflows: u64,
    pub ready_overflows: u64,
    pub queue_overflows: u64,
}

impl EgressShardHeartbeat {
    pub fn from_snapshot(
        snapshot: EgressShardSnapshot,
        now: Instant,
        stall_after: Duration,
    ) -> Self {
        Self::from_snapshot_with_capacity(snapshot, now, stall_after, 0)
    }

    /// Same as [`Self::from_snapshot`], but also records the shard's
    /// command-channel capacity so callers can derive a saturation ratio
    /// (`command_depth as f64 / command_capacity as f64`) without a second
    /// lookup. `command_capacity` of `0` means "unknown" (no capacity was
    /// supplied), not "zero-capacity channel".
    pub fn from_snapshot_with_capacity(
        snapshot: EgressShardSnapshot,
        now: Instant,
        stall_after: Duration,
        command_capacity: u32,
    ) -> Self {
        let progress_age = snapshot
            .last_progress_at
            .map(|progress_at| now.saturating_duration_since(progress_at));
        let state = if snapshot.panicked {
            EgressShardHealth::Panicked
        } else if snapshot.stopped {
            EgressShardHealth::Stopped
        } else if progress_age.is_none_or(|age| age >= stall_after) {
            EgressShardHealth::Stalled
        } else {
            EgressShardHealth::Healthy
        };
        Self {
            shard_id: snapshot.shard_id,
            state,
            loop_iterations: snapshot.loop_iterations,
            media_ticks: snapshot.media_ticks,
            progress_age,
            command_depth: snapshot.metrics.command_depth,
            command_capacity,
            resync_count: snapshot.metrics.feed_resyncs,
            ready_depth: snapshot.metrics.ready_depth,
            ready_depth_hwm: snapshot.metrics.ready_depth_hwm,
            ready_visits: snapshot.metrics.ready_visits,
            budget_exhaustions: snapshot.metrics.budget_exhaustions,
            rx_packets: snapshot.metrics.rx_packets,
            rx_bytes: snapshot.metrics.rx_bytes,
            tx_packets: snapshot.metrics.tx_packets,
            tx_bytes: snapshot.metrics.tx_bytes,
            cqes: snapshot.metrics.cqes,
            sqes: snapshot.metrics.sqes,
            stale_completions: snapshot.metrics.stale_completions,
            rx_pool_empty: snapshot.metrics.rx_pool_empty,
            tx_pool_empty: snapshot.metrics.tx_pool_empty,
            send_zc_attempts: snapshot.metrics.send_zc_attempts,
            send_zc_fallbacks: snapshot.metrics.send_zc_fallbacks,
            cq_overflows: snapshot.metrics.cq_overflows,
            ready_overflows: snapshot.metrics.ready_overflows,
            queue_overflows: snapshot.metrics.queue_overflows,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressShardHealth {
    Healthy,
    Stalled,
    Stopped,
    Panicked,
}
