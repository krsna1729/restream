//! Control jobs serialize one feed before entering the blocking pool. Shard
//! media and read-only health snapshots never wait for that control mutex.
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use tokio::runtime::Handle;
use tokio::sync::Mutex;

use super::{EgressFabricRuntime, FeedWakeSubscription, subscribe_fabric_wakes};
use crate::egress::command::FeedId;
use crate::egress::shard::{
    EgressShardGroup, EgressShardHeartbeat, EgressShardSnapshot, FeedWakeHandle, ShardSnapshotWatch,
};
use restream_sync::PublishSubscribers;

#[derive(Clone, Debug)]
pub(super) struct FabricObserver {
    watch: Arc<ArcSwap<ShardSnapshotWatch>>,
    command_capacity: u32,
}

impl FabricObserver {
    pub(super) fn new(group: &EgressShardGroup, command_capacity: u32) -> Self {
        Self {
            watch: Arc::new(ArcSwap::from_pointee(group.snapshot_watch())),
            command_capacity,
        }
    }

    pub(super) fn update(&self, group: &EgressShardGroup) {
        self.watch.store(Arc::new(group.snapshot_watch()));
    }

    fn snapshots(&self) -> Vec<EgressShardSnapshot> {
        self.watch.load().snapshots()
    }

    pub(super) fn heartbeat(
        &self,
        now: Instant,
        stall_after: Duration,
    ) -> Vec<EgressShardHeartbeat> {
        self.snapshots()
            .into_iter()
            .map(|snapshot| {
                EgressShardHeartbeat::from_snapshot_with_capacity(
                    snapshot,
                    now,
                    stall_after,
                    self.command_capacity,
                )
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FabricWorkerError {
    Closed,
    Join(String),
}

#[derive(Debug)]
struct RuntimeOwner {
    runtime: Option<EgressFabricRuntime>,
    cleanup: Handle,
}

impl Drop for RuntimeOwner {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            // An unpublished candidate, losing constructor, or cancelled last
            // user must still stop and join its shards. Drop never waits on an
            // async worker, nor relies on command-channel disconnection: wake
            // subscriptions can own additional channel senders.
            drop(self.cleanup.spawn_blocking(move || runtime.shutdown()));
        }
    }
}

/// A feed's control-plane ownership. Clones observe the same live runtime;
/// closing it is permanent for all clones, even jobs queued before closure.
#[derive(Clone, Debug)]
pub struct SharedFabricRuntime {
    owner: Arc<Mutex<RuntimeOwner>>,
    observer: FabricObserver,
    wakes: Arc<ArcSwap<Vec<FeedWakeHandle>>>,
}

impl SharedFabricRuntime {
    pub fn new(runtime: EgressFabricRuntime) -> Self {
        let observer = runtime.observer.clone();
        let wakes = runtime.wake_handles.clone();
        Self {
            owner: Arc::new(Mutex::new(RuntimeOwner {
                runtime: Some(runtime),
                cleanup: Handle::current(),
            })),
            observer,
            wakes,
        }
    }

    pub async fn with<R: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut EgressFabricRuntime) -> R + Send + 'static,
    ) -> Result<R, FabricWorkerError> {
        // Wait asynchronously BEFORE offloading. A stalled feed must not fill
        // the blocking pool with workers waiting on that feed's mutex.
        let owner = Arc::clone(&self.owner).lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let mut owner = owner;
            owner
                .runtime
                .as_mut()
                .map(operation)
                .ok_or(FabricWorkerError::Closed)
        })
        .await
        .map_err(|error| FabricWorkerError::Join(error.to_string()))?
    }

    /// Periodic sizing never queues behind a busy feed, nor waits for backend
    /// readiness. The owned guard makes this one maintenance job per feed.
    pub(crate) fn try_observe(
        &self,
        operation: impl FnOnce(&mut EgressFabricRuntime) + Send + 'static,
    ) -> bool {
        let Ok(mut owner) = Arc::clone(&self.owner).try_lock_owned() else {
            return false;
        };
        let Some(runtime) = owner.runtime.as_ref() else {
            return false;
        };
        if !runtime.observation_due() && !runtime.has_panicked_shard() {
            return false;
        }
        drop(tokio::task::spawn_blocking(move || {
            if let Some(runtime) = owner.runtime.as_mut() {
                operation(runtime);
            }
        }));
        true
    }

    /// Subscribe this fabric to producer wakes without exposing the shard list.
    pub(crate) fn subscribe_feed_wakes(
        &self,
        kind: &'static str,
        feed_id: FeedId,
        subscribers: Arc<PublishSubscribers>,
    ) -> FeedWakeSubscription {
        subscribe_fabric_wakes(kind, feed_id, subscribers, self.wakes.clone())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn snapshots(&self) -> Vec<EgressShardSnapshot> {
        self.observer.snapshots()
    }

    pub(crate) fn heartbeat(
        &self,
        now: Instant,
        stall_after: Duration,
    ) -> Vec<EgressShardHeartbeat> {
        self.observer.heartbeat(now, stall_after)
    }

    pub async fn shutdown_off_worker(self) -> Vec<EgressShardSnapshot> {
        let owner = Arc::clone(&self.owner).lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let mut owner = owner;
            owner
                .runtime
                .take()
                .map(EgressFabricRuntime::shutdown)
                .unwrap_or_default()
        })
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "egress fabric shutdown did not finish cleanly");
            Vec::new()
        })
    }
}

#[cfg(test)]
mod tests;
