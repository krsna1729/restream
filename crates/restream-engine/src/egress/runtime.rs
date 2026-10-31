use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::egress::command::{EgressCommand, FeedId, ShardId};
use crate::egress::manager::{
    EgressManager, EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome,
};
use crate::egress::shard::{
    EgressShardBackend, EgressShardConfig, EgressShardGroup, EgressShardGroupError,
    EgressShardHeartbeat, EgressShardSnapshot, FeedWakeHandle,
};
use crate::egress::sizing::{Saturated, ServiceSample, ShardSizer};
use restream_sync::{PublishSubscribers, PublishWake, RingBuffer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressFabricRuntimeError {
    ShardCountMismatch { expected: usize, actual: usize },
}

#[derive(Clone, Copy)]
pub enum ResizeReason {
    Add,
    Remove,
    Observe,
}

#[derive(Debug)]
pub struct EgressFabricRuntime {
    manager: EgressManager,
    group: EgressShardGroup,
    /// Shared with every feed's publication subscription
    /// (`subscribe_fabric_wakes`), which the feed's producer reads through on
    /// every publication. `rescale` stores a fresh list, so a shard grown
    /// later gets the fast feed-wake path on feeds subscribed before it.
    wake_handles: Arc<ArcSwap<Vec<FeedWakeHandle>>>,
    sizer: ShardSizer,
    adaptive: bool,
    previous_cpu: HashMap<ShardId, (u64, Instant)>,
    previous_service: HashMap<ShardId, Instant>,
    last_observation: Option<Instant>,
    feed_mark: Option<(usize, u64, Instant)>,
    shard_ceiling: u32,
}

impl EgressFabricRuntime {
    pub fn new(
        manager_config: EgressManagerConfig,
        group: EgressShardGroup,
    ) -> Result<Self, EgressFabricRuntimeError> {
        let expected = manager_config.shard_count().get() as usize;
        let actual = group.shard_count();
        if actual != expected {
            return Err(EgressFabricRuntimeError::ShardCountMismatch { expected, actual });
        }
        let wake_handles = Arc::new(ArcSwap::from_pointee(group.feed_wake_handles()));
        Ok(Self {
            manager: EgressManager::new(manager_config),
            group,
            wake_handles,
            sizer: ShardSizer::default(),
            adaptive: false,
            previous_cpu: HashMap::new(),
            previous_service: HashMap::new(),
            last_observation: None,
            feed_mark: None,
            shard_ceiling: u32::MAX,
        })
    }

    pub fn adaptive(mut self, shard_ceiling: u32) -> Self {
        self.adaptive = true;
        self.shard_ceiling = shard_ceiling.max(1);
        self.sizer.resized(Instant::now());
        self
    }

    pub(crate) fn reason_for(&self, command: &EgressCommand) -> ResizeReason {
        match command {
            EgressCommand::Add(spec) if self.manager.desired_output(&spec.id).is_none() => {
                ResizeReason::Add
            }
            _ => ResizeReason::Remove,
        }
    }

    /// Admission for a new output (`reason_for` gave `Add`), after the
    /// Add's `rescale`: refused when the shard it would be placed on would
    /// run above `ADMIT_CEILING` with it. Every shard stays under the
    /// ceiling, whatever the pool's average.
    pub(crate) fn admit_add(
        &self,
        profile: crate::config::EgressShardProfile,
        output_id: &crate::egress::command::OutputId,
    ) -> Result<(), Saturated> {
        let shard = self.manager.assign_output(output_id);
        let shard_outputs = self.manager.desired_count_for_shard(shard) + 1;
        self.sizer
            .admit(shard_outputs, profile.outputs_per_shard())
            .map_err(|busy_pct| Saturated {
                busy_pct,
                shards: self.manager.placement_count().get(),
            })
    }

    pub fn observation_due(&self) -> bool {
        self.last_observation
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(5))
    }

    /// Forecast the feed's bitrate from its ring: between looks, from the
    /// bytes published; at the first look at a ring, from the rate its
    /// retained window runs at, so a burst of Adds on a new runtime is priced
    /// at the feed's rate, not the reference rate.
    pub(crate) fn forecast_feed(&mut self, ring: &RingBuffer) {
        let now = Instant::now();
        let identity = std::ptr::from_ref(ring) as usize;
        let published_bytes = ring.published_bytes();
        match self.feed_mark {
            Some((previous_ring, previous, at)) if previous_ring == identity => {
                let elapsed = now.duration_since(at).as_secs_f64();
                if elapsed < 0.5 && published_bytes >= previous {
                    return;
                }
                if let Some(bytes) = published_bytes.checked_sub(previous) {
                    self.sizer.forecast_feed(bytes as f64 * 8.0 / elapsed);
                }
            }
            _ => {
                if let Some(bps) = ring.retained_payload_bitrate_bps() {
                    self.sizer.forecast_feed(bps as f64);
                }
            }
        }
        self.feed_mark = Some((identity, published_bytes, now));
    }

    fn observe_service(&mut self, now: Instant) -> bool {
        if self
            .last_observation
            .is_some_and(|at| now.duration_since(at) < Duration::from_secs(5))
        {
            return false;
        }
        self.last_observation = Some(now);
        let snapshots = self.group.snapshots();
        self.observe_samples(&snapshots)
    }

    fn observe_samples(&mut self, snapshots: &[EgressShardSnapshot]) -> bool {
        let mut sample = ServiceSample {
            shards: snapshots.len() as u32,
            ..Default::default()
        };
        let mut sum_ratio = 0.0;
        let mut sum_sq_ratio = 0.0;
        let mut clocks = 0;
        let mut services_fresh = true;
        for snapshot in snapshots {
            let metrics = &snapshot.metrics;
            let Some(at) = metrics.thread_cpu_at else {
                continue;
            };
            if let Some((previous, before)) = self
                .previous_cpu
                .insert(snapshot.shard_id, (metrics.thread_cpu_ns, at))
            {
                let elapsed = at.saturating_duration_since(before).as_secs_f64();
                if elapsed > 0.0 && metrics.thread_cpu_ns >= previous {
                    let utilization = (metrics.thread_cpu_ns - previous) as f64 / 1e9 / elapsed;
                    sample.cpu_cores += utilization;
                    sample.peak_utilization = sample.peak_utilization.max(utilization);
                    clocks += 1;
                }
            }
            let service = metrics.service;
            if let Some(at) = service.completed_at {
                services_fresh &= self
                    .previous_service
                    .get(&snapshot.shard_id)
                    .is_none_or(|previous| at > *previous);
            } else if service.visited > 0 {
                services_fresh = false;
            }
            sample.rated += service.rated;
            sample.under_floor += service.under_floor;
            sample.offered_bps += service.offered_bps;
            sample.delivered_bps += service.delivered_bps;
            sum_ratio += service.sum_ratio;
            sum_sq_ratio += service.sum_sq_ratio;
        }
        if clocks != snapshots.len() || clocks == 0 {
            // A measurement gap is not evidence of headroom.
            self.sizer.forget_headroom();
            return false;
        }
        if !services_fresh {
            return false;
        }
        for snapshot in snapshots {
            if let Some(at) = snapshot.metrics.service.completed_at {
                self.previous_service.insert(snapshot.shard_id, at);
            }
        }
        sample.fairness = (sample.rated > 1 && sum_sq_ratio > 0.0)
            .then(|| sum_ratio * sum_ratio / (f64::from(sample.rated) * sum_sq_ratio));
        self.sizer.observe(sample, self.manager.output_count());
        tracing::debug!(shards = snapshots.len(), rated = sample.rated,
            under_floor = sample.under_floor, cpu_cores = sample.cpu_cores,
            peak_utilization = sample.peak_utilization,
            offered_bps = sample.offered_bps, delivered_bps = sample.delivered_bps,
            fairness = ?sample.fairness, "egress service observation");
        true
    }

    pub fn dispatch(
        &mut self,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<EgressShardGroupError>> {
        self.manager.dispatch_command(command, &self.group)
    }

    /// A shard thread panicked and waits for `rescale` to replace it.
    pub fn has_panicked_shard(&self) -> bool {
        !self.group.panicked_shards().is_empty()
    }

    /// Shared handle list for feed publication subscriptions (see the field
    /// doc on `wake_handles`); `rescale` keeps it current.
    pub(crate) fn feed_wake_handles(&self) -> Arc<ArcSwap<Vec<FeedWakeHandle>>> {
        Arc::clone(&self.wake_handles)
    }

    /// Plan the shard count before an Add, or from the reconciler's periodic
    /// observation, without touching a live output: growth only adds capacity
    /// for NEW outputs; shrink stops placing on the tail shard and stops its
    /// thread once it holds no output. A destination never reconnects because
    /// of a resize.
    ///
    /// First, a shard whose thread panicked gets a new thread, and the
    /// outputs it held are released as unexpectedly terminated (their
    /// progress sinks are shared with the application, so this is visible
    /// without the dead thread): each application task then retries its
    /// output through the ordinary backoff, onto a live shard. Until its
    /// replacement starts, placement skips the dead shard. Every Add rescales
    /// before it is placed, so a new output never lands on a dead shard
    /// while a live one exists.
    ///
    /// `factory_for(shard_id)` builds each new backend on its shard thread.
    /// The RTMP Compio poller and SRT Compio runtime/Owners can both fail to
    /// initialize; a failed grow attempt stops growing for this call (logged
    /// by the caller via the returned `Err`) rather than panicking or silently
    /// continuing with fewer shards than `touched` would suggest. Shards that
    /// grew before the failure stay, so the next `Add`/`Remove` can retry.
    /// Replacement failures do not prevent other dead shards recovering;
    /// failed replacements retry on the next call, with the first error returned.
    ///
    /// Returns the shard ids touched (replaced, grown or shut down) on
    /// success, for logging.
    pub fn rescale<B, E, F, G>(
        &mut self,
        profile: crate::config::EgressShardProfile,
        host: crate::host_capacity::HostCapacity,
        reason: ResizeReason,
        shard_config: EgressShardConfig,
        mut factory_for: G,
    ) -> Result<Vec<ShardId>, E>
    where
        B: EgressShardBackend,
        E: Send + 'static,
        F: FnOnce() -> Result<B, E> + Send + 'static,
        G: FnMut(ShardId) -> F,
    {
        let mut touched = Vec::new();
        let mut grow_error = None;
        let panicked = self.group.panicked_shards();
        // Isolate every dead shard before any fallible replacement. An
        // unavailable backend on one shard must not strand another's outputs
        // or leave that other dead shard eligible for placement.
        for &shard_id in &panicked {
            let released = self.manager.forget_shard(shard_id);
            tracing::warn!(shard_id = ?shard_id, outputs = released.len(),
                "egress shard panicked; replacing it and retrying its outputs");
            for spec in &released {
                spec.progress.mark_terminated_unexpectedly();
            }
        }
        for shard_id in panicked {
            match self
                .group
                .replace_with(shard_id, shard_config, factory_for(shard_id))
            {
                Ok(()) => {
                    self.manager.revive_shard(shard_id);
                    touched.push(shard_id);
                }
                Err(error) => {
                    // Keep the first error for the caller, but give every
                    // other failed shard its own chance to recover.
                    grow_error.get_or_insert(error);
                }
            }
        }
        let now = Instant::now();
        let placement = self.manager.placement_count().get();
        let outputs =
            self.manager.output_count() + usize::from(matches!(reason, ResizeReason::Add));
        let maximum = host.egress_shards_per_pool().min(self.shard_ceiling);
        let target = if self.adaptive && profile.shard_override().is_none() {
            let prior = profile.outputs_per_shard();
            match reason {
                ResizeReason::Add => placement.max(self.sizer.recommend(outputs, prior, maximum)),
                ResizeReason::Remove => placement,
                ResizeReason::Observe => {
                    if self.observe_service(now) {
                        self.sizer.shrink_target(now, outputs, placement, prior)
                    } else {
                        placement
                    }
                }
            }
        } else {
            crate::config::target_egress_fabric_shards(profile, outputs, host.cpus)
        }
        .max(1) as usize;

        while grow_error.is_none() && self.group.shard_count() < target {
            let shard_id =
                ShardId::new(u32::try_from(self.group.shard_count()).unwrap_or(u32::MAX));
            match self.group.grow_with(shard_config, factory_for(shard_id)) {
                Ok(shard_id) => touched.push(shard_id),
                Err(error) => {
                    grow_error = Some(error);
                    break;
                }
            }
        }
        let mut changed = !touched.is_empty();
        if grow_error.is_none() || self.group.shard_count() as u32 > placement {
            let usable = target.min(self.group.shard_count()).max(1) as u32;
            if usable != placement {
                if usable > self.manager.config().shard_count().get() {
                    self.manager
                        .grow_to(NonZeroU32::new(usable).expect("usable >= 1"));
                } else {
                    self.manager
                        .set_placement(NonZeroU32::new(usable).expect("usable >= 1"));
                }
                changed = true;
            }
        }
        while self.manager.retire_empty_tail() {
            let Some(shard_id) = self.group.shrink() else {
                break;
            };
            touched.push(shard_id);
            changed = true;
        }

        if changed {
            self.sizer.resized(now);
            self.previous_cpu.clear();
            self.previous_service.clear();
            self.last_observation = None;
        }
        if !touched.is_empty() {
            // Replaced/grown/shut-down shards changed the group's real
            // handle set; publish the new list every feed subscription reads
            // through (see the `wake_handles` field doc) -- including on a
            // partial failure below, since whatever changed before the
            // failure is still real and needs a wake path.
            self.wake_handles
                .store(Arc::new(self.group.feed_wake_handles()));
        }

        match grow_error {
            Some(error) => Err(error),
            None => Ok(touched),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn snapshots(&self) -> Vec<EgressShardSnapshot> {
        self.group.snapshots()
    }

    /// Per-shard health for diagnostics and alerting. `stall_after` should
    /// be tuned to the caller's own poll cadence, not a fixed constant —
    /// too short flags healthy-but-quiet shards (nothing to send right
    /// now) as stalled.
    pub(crate) fn heartbeat(
        &self,
        now: Instant,
        stall_after: Duration,
    ) -> Vec<EgressShardHeartbeat> {
        let command_capacity = self.manager.config().command_channel_capacity().get() as u32;
        self.group
            .snapshots()
            .into_iter()
            .map(|snapshot| {
                EgressShardHeartbeat::from_snapshot_with_capacity(
                    snapshot,
                    now,
                    stall_after,
                    command_capacity,
                )
            })
            .collect()
    }

    pub fn shutdown(self) -> Vec<EgressShardSnapshot> {
        self.group.shutdown_and_join()
    }
}

/// Delivers coalesced wakes from the publishing thread. Each handle uses
/// one gate and at most one bounded `try_send` per publication. Per shard,
/// at most one publication wake is queued plus one separate Full-retry hint.
struct FabricWakers {
    handles: Arc<ArcSwap<Vec<FeedWakeHandle>>>,
}

impl PublishWake for FabricWakers {
    #[inline]
    fn wake(&self) {
        for handle in self.handles.load().iter() {
            // Full arms a retry hint serviced even without another publish,
            // through the shard's bounded idle deadline. Closed needs no service.
            let _ = handle.deliver();
        }
    }
}

/// A feed's fabric wake registration. Dropping it unsubscribes.
pub(crate) struct FeedWakeSubscription {
    subscribers: Arc<PublishSubscribers>,
    waker: Arc<dyn PublishWake>,
}

impl std::fmt::Debug for FeedWakeSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedWakeSubscription")
            .finish_non_exhaustive()
    }
}

impl Drop for FeedWakeSubscription {
    fn drop(&mut self) {
        self.subscribers.unsubscribe(&self.waker);
    }
}

/// Wake a fabric's shards directly from the feed's producer: each
/// publication (and each seal or end of stream) calls into the shards'
/// coalescing wake gates on the publishing thread, with no watcher task or
/// runtime hop in between. Shared by all four
/// `engine_*_egress_fabric.rs` callers (`kind` only labels the log line).
///
/// `wake_handles` is shared with `EgressFabricRuntime::rescale`, so shards
/// that grow later are woken too. One wake is delivered on subscribe, so a
/// publication that landed before registration is not left waiting for the
/// shards' idle poll.
pub(crate) fn subscribe_fabric_wakes(
    kind: &'static str,
    feed_id: FeedId,
    subscribers: Arc<restream_sync::PublishSubscribers>,
    wake_handles: Arc<ArcSwap<Vec<FeedWakeHandle>>>,
) -> FeedWakeSubscription {
    let waker: Arc<dyn PublishWake> = Arc::new(FabricWakers {
        handles: wake_handles,
    });
    subscribers.subscribe(Arc::clone(&waker));
    waker.wake();
    tracing::info!(feed_id = %feed_id, "{kind} fabric feed wakes subscribed");
    FeedWakeSubscription { subscribers, waker }
}

#[cfg(test)]
mod tests;
