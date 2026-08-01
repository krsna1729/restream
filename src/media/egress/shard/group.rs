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

#[derive(Debug)]
pub struct EgressShardGroup {
    handles: Vec<EgressShardHandle>,
}

impl EgressShardGroup {
    pub fn spawn<B: EgressShardBackend>(
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

    pub fn replace_panicked<B, F>(
        &mut self,
        config: EgressShardConfig,
        mut backend_for: F,
    ) -> Vec<ShardId>
    where
        B: EgressShardBackend,
        F: FnMut(ShardId) -> B,
    {
        let mut replaced = Vec::new();
        for handle in &mut self.handles {
            let snapshot = handle.snapshot();
            if !snapshot.panicked {
                continue;
            }
            let shard_id = snapshot.shard_id;
            let replacement = EgressShardHandle::spawn(shard_id, config, backend_for(shard_id));
            let old = std::mem::replace(handle, replacement);
            let _ = old.shutdown_and_join();
            replaced.push(shard_id);
        }
        replaced
    }

    /// Add one shard at the next index, running `backend`. Used for
    /// output-count-driven scale-out (`EgressFabricRuntime::rescale`) —
    /// mirrors `replace_panicked`'s spawn shape, but appends a new handle
    /// instead of replacing one in place.
    pub fn grow<B: EgressShardBackend>(
        &mut self,
        config: EgressShardConfig,
        backend: B,
    ) -> ShardId {
        let shard_id = ShardId::new(u32::try_from(self.handles.len()).unwrap_or(u32::MAX));
        self.handles
            .push(EgressShardHandle::spawn(shard_id, config, backend));
        shard_id
    }

    /// Remove and gracefully shut down the highest-index shard, if any.
    /// Used for output-count-driven scale-in. The caller is responsible
    /// for rehoming whatever outputs were assigned to this shard
    /// (`EgressManager::rehome`) — this only tears the shard thread down,
    /// draining any leaves it still owned per its own `Shutdown` handling
    /// (`EgressShardRuntime::run`'s drain window), it does not reassign
    /// them anywhere.
    pub fn shrink(&mut self) -> Option<(ShardId, EgressShardSnapshot)> {
        let handle = self.handles.pop()?;
        let shard_id = handle.shard_id();
        Some((shard_id, handle.shutdown_and_join()))
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
