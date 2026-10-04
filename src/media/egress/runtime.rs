use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::media::egress::command::{EgressCommand, FeedId, ShardId};
use crate::media::egress::feed::EgressFeed;
use crate::media::egress::manager::{
    EgressManager, EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome,
};
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardConfig, EgressShardGroup, EgressShardGroupError,
    EgressShardHeartbeat, EgressShardSnapshot, FeedWakeHandle,
};
use crate::media::egress::sizing::{ServiceSample, ShardSizer};
use crate::media::ring_buffer::{PublishSubscribers, PublishWake, RingBuffer};

#[cfg(test)]
#[path = "runtime_sizing_tests.rs"]
mod sizing_tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EgressFabricRuntimeError {
    ShardCountMismatch { expected: usize, actual: usize },
}

#[derive(Clone, Copy)]
pub(crate) enum ResizeReason {
    Add,
    Remove,
    Observe,
}

#[derive(Debug)]
pub(crate) struct EgressFabricRuntime {
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
    pub(crate) fn new(
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

    pub(crate) fn adaptive(mut self, shard_ceiling: u32) -> Self {
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

    pub(crate) fn observation_due(&self) -> bool {
        self.last_observation
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(5))
    }

    pub(crate) fn forecast_feed(&mut self, ring: &RingBuffer) {
        let now = Instant::now();
        let identity = std::ptr::from_ref(ring) as usize;
        let published_bytes = ring.published_bytes();
        if let Some((previous_ring, previous, at)) = self.feed_mark
            && previous_ring == identity
        {
            let elapsed = now.duration_since(at).as_secs_f64();
            if elapsed < 0.5 && published_bytes >= previous {
                return;
            }
            if let Some(bytes) = published_bytes.checked_sub(previous) {
                self.sizer.forecast_feed(bytes as f64 * 8.0 / elapsed);
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

    pub(crate) fn dispatch(
        &mut self,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<EgressShardGroupError>> {
        self.observe_queued_commands();
        self.manager.dispatch_to_group(command, &self.group)
    }

    fn observe_queued_commands(&mut self) {
        let group = &self.group;
        self.manager
            .observe_queued_commands(|shard_id| group.queued_commands(shard_id));
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
    /// `factory_for(shard_id)` builds each new backend on its shard thread.
    /// The RTMP Compio poller and SRT Compio runtime/Owners can both fail to
    /// initialize; a failed grow attempt stops growing for this call (logged
    /// by the caller via the returned `Err`) rather than panicking or silently
    /// continuing with fewer shards than `touched` would suggest. Shards that
    /// grew before the failure stay, so the next `Add`/`Remove` can retry.
    ///
    /// Returns the shard ids touched (grown or shut down) on success, for
    /// logging.
    pub(crate) fn rescale<B, E, F, G>(
        &mut self,
        profile: crate::config::EgressShardProfile,
        effective_cpus: usize,
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
        let now = Instant::now();
        let placement = self.manager.placement_count().get();
        let outputs =
            self.manager.output_count() + usize::from(matches!(reason, ResizeReason::Add));
        let maximum =
            crate::config::default_egress_fabric_shards(effective_cpus).min(self.shard_ceiling);
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
            crate::config::target_egress_fabric_shards(profile, outputs, effective_cpus)
        }
        .max(1) as usize;

        let mut touched = Vec::new();
        let mut grow_error = None;
        while self.group.shard_count() < target {
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
            // Grown/shut-down shards changed the group's real handle set;
            // publish the new list every feed subscription reads through
            // (see the `wake_handles` field doc) -- including on a partial
            // failure below, since whatever grew before the failure is
            // still real and needs a wake path.
            self.wake_handles
                .store(Arc::new(self.group.feed_wake_handles()));
        }

        match grow_error {
            Some(error) => Err(error),
            None => Ok(touched),
        }
    }

    #[cfg(test)]
    pub(crate) fn snapshots(&self) -> Vec<EgressShardSnapshot> {
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

    pub(crate) fn shutdown(self) -> Vec<EgressShardSnapshot> {
        self.group.shutdown_and_join()
    }
}

/// Feed types whose publications can wake a fabric: the ring the feed's
/// producer publishes into. `RingFeed` resolves its current ring; a later
/// replacement shares the subscriber set (`RingBuffer::seal_and_forward`).
pub(crate) trait FabricWatchFeed: EgressFeed + Send + 'static {
    fn publication_ring(&self) -> Arc<RingBuffer>;
}

impl FabricWatchFeed for crate::media::egress::journal::RingFeed {
    fn publication_ring(&self) -> Arc<RingBuffer> {
        crate::media::egress::journal::RingFeed::publication_ring(self)
    }
}

impl FabricWatchFeed for crate::media::egress::journal::TsFeed {
    fn publication_ring(&self) -> Arc<RingBuffer> {
        crate::media::egress::journal::TsFeed::publication_ring(self)
    }
}

/// Delivers one coalesced wake per shard from the publishing thread. Each
/// `FeedWakeHandle::deliver` is an atomic swap on the shard's gate, plus one
/// bounded `try_send` on its clear-to-set transition, so at most one wake per
/// shard is in flight however fast the feed publishes.
struct FabricWakers {
    handles: Arc<ArcSwap<Vec<FeedWakeHandle>>>,
}

impl PublishWake for FabricWakers {
    #[inline]
    fn wake(&self) {
        for handle in self.handles.load().iter() {
            // Full or closed: the gate stays clear, so the next publication
            // retries; the shard's idle poll covers a quiet feed meanwhile.
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
/// runtime hop in between (WI11). Shared by all four
/// `engine_*_egress_fabric.rs` callers (`kind` only labels the log line).
///
/// `wake_handles` is shared with `EgressFabricRuntime::rescale`, so shards
/// that grow later are woken too. One wake is delivered on subscribe, so a
/// publication that landed before registration is not left waiting for the
/// shards' idle poll.
pub(crate) fn subscribe_fabric_wakes<F>(
    kind: &'static str,
    feed_id: FeedId,
    feed: &F,
    wake_handles: Arc<ArcSwap<Vec<FeedWakeHandle>>>,
) -> FeedWakeSubscription
where
    F: FabricWatchFeed,
{
    let subscribers = feed.publication_ring().publication_subscribers();
    let waker: Arc<dyn PublishWake> = Arc::new(FabricWakers {
        handles: wake_handles,
    });
    subscribers.subscribe(Arc::clone(&waker));
    waker.wake();
    tracing::info!(feed_id = %feed_id, "{kind} fabric feed wakes subscribed");
    FeedWakeSubscription { subscribers, waker }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use crate::media::egress::command::{
        EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec, ShardId,
    };
    use crate::media::egress::manager::{EgressManagerConfigError, ManagerCommandOutcome};
    use crate::media::egress::policy::LeafPolicy;
    use crate::media::egress::shard::{
        EgressShardBackend, EgressShardCommandEffect, EgressShardConfig,
    };

    #[derive(Debug, Default)]
    struct ProbeState {
        commands: Vec<String>,
        shutdowns: u64,
        feed_wake_commands: u64,
        feed_wakes_completed: u64,
    }

    #[derive(Clone, Debug, Default)]
    pub(super) struct Probe {
        inner: Arc<(Mutex<ProbeState>, Condvar)>,
    }

    impl Probe {
        fn wait_for_commands(&self, target: usize) {
            let (lock, condvar) = &*self.inner;
            let state = lock.lock().unwrap();
            let result = condvar
                .wait_timeout_while(state, Duration::from_secs(2), |state| {
                    state.commands.len() < target
                })
                .unwrap();
            assert!(result.0.commands.len() >= target);
        }

        fn wait_for_completed_feed_wakes(&self, target: u64) {
            let (lock, condvar) = &*self.inner;
            let state = lock.lock().unwrap();
            let result = condvar
                .wait_timeout_while(state, Duration::from_secs(2), |state| {
                    state.feed_wakes_completed < target
                })
                .unwrap();
            assert!(
                result.0.feed_wakes_completed >= target,
                "expected {target} completed feed wakes, got {}",
                result.0.feed_wakes_completed
            );
        }

        fn state(&self) -> ProbeState {
            let state = self.inner.0.lock().unwrap();
            ProbeState {
                commands: state.commands.clone(),
                shutdowns: state.shutdowns,
                feed_wake_commands: state.feed_wake_commands,
                feed_wakes_completed: state.feed_wakes_completed,
            }
        }
    }
    #[derive(Debug)]
    struct ProbeBackend {
        probe: Probe,
    }

    impl EgressShardBackend for ProbeBackend {
        fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
            let is_feed_wake = matches!(&command, EgressCommand::FeedWake);
            let label = match command {
                EgressCommand::Add(spec) => format!("add:{}", spec.id),
                EgressCommand::Update(spec) => format!("update:{}", spec.id),
                EgressCommand::Remove(output_id) => format!("remove:{output_id}"),
                EgressCommand::FeedWake => "feed-wake".to_string(),
                EgressCommand::DrainShard(shard_id) => format!("drain:{shard_id}"),
                EgressCommand::Shutdown => "shutdown".to_string(),
            };
            let (lock, condvar) = &*self.probe.inner;
            let mut state = lock.lock().unwrap();
            state.commands.push(label);
            if is_feed_wake {
                state.feed_wake_commands = state.feed_wake_commands.saturating_add(1);
            }
            condvar.notify_all();
            EgressShardCommandEffect::Continue
        }

        fn on_media_tick(&mut self) -> EgressShardCommandEffect {
            let (lock, condvar) = &*self.probe.inner;
            let mut state = lock.lock().unwrap();
            state.feed_wakes_completed = state.feed_wake_commands;
            condvar.notify_all();
            EgressShardCommandEffect::Continue
        }

        fn on_shutdown(&mut self) {
            let (lock, condvar) = &*self.probe.inner;
            let mut state = lock.lock().unwrap();
            state.shutdowns = state.shutdowns.saturating_add(1);
            condvar.notify_all();
        }
    }

    fn shard_config() -> EgressShardConfig {
        EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(1)).unwrap()
    }

    fn manager_config(shards: u32) -> Result<EgressManagerConfig, EgressManagerConfigError> {
        EgressManagerConfig::new(shards, 16)
    }

    pub(super) fn group(shards: u32, probes: &[Probe]) -> EgressShardGroup {
        let backends = probes
            .iter()
            .cloned()
            .map(|probe| ProbeBackend { probe })
            .collect::<Vec<_>>();
        EgressShardGroup::spawn(NonZeroU32::new(shards).unwrap(), shard_config(), backends).unwrap()
    }

    pub(super) fn output_spec(id: &str) -> OutputSpec {
        OutputSpec {
            id: OutputId::new(id),
            generation: 1,
            feed: FeedId::new("feed-1"),
            protocol: ProtocolSpec::Sink,
            policy: LeafPolicy::default(),
            progress: Default::default(),
        }
    }

    #[test]
    fn runtime_dispatches_commands_to_owned_shard_group() {
        let probe = Probe::default();
        let mut runtime = EgressFabricRuntime::new(
            manager_config(1).unwrap(),
            group(1, std::slice::from_ref(&probe)),
        )
        .unwrap();

        let outcome = runtime.dispatch(EgressCommand::Add(output_spec("out-1")));

        assert_eq!(
            outcome,
            Ok(ManagerCommandOutcome::Enqueued {
                shard_id: ShardId::new(0)
            })
        );
        probe.wait_for_commands(1);
        assert_eq!(probe.state().commands, vec!["add:out-1"]);
        let snapshots = runtime.shutdown();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(probe.state().shutdowns, 1);
    }

    #[test]
    fn command_admission_tracks_the_real_queue_not_lifetime_dispatches() {
        // Regression: the manager's per-shard command depth was incremented
        // on every dispatch and never released in production, so a feed that
        // sent more than `command_channel_capacity` commands over its life
        // (4000 outputs on 3 shards, or ordinary add/remove churn) got
        // `CommandChannelFull` forever although every shard queue was empty.
        let probe = Probe::default();
        let mut runtime = EgressFabricRuntime::new(
            manager_config(1).unwrap(),
            group(1, std::slice::from_ref(&probe)),
        )
        .unwrap();

        let dispatches = 16 * 4;
        for index in 0..dispatches {
            let outcome =
                runtime.dispatch(EgressCommand::Add(output_spec(&format!("out-{index}"))));
            assert_eq!(
                outcome,
                Ok(ManagerCommandOutcome::Enqueued {
                    shard_id: ShardId::new(0)
                }),
                "dispatch {index} of {dispatches}"
            );
            probe.wait_for_commands(index + 1);
        }
        runtime.shutdown();
    }

    #[test]
    fn runtime_rejects_group_with_wrong_shard_count() {
        let probes = vec![Probe::default(), Probe::default()];
        let result = EgressFabricRuntime::new(manager_config(1).unwrap(), group(2, &probes));

        assert!(matches!(
            result,
            Err(EgressFabricRuntimeError::ShardCountMismatch {
                expected: 1,
                actual: 2
            })
        ));
    }

    // -----------------------------------------------------------------
    // Dynamic shard scaling: rescale
    // -----------------------------------------------------------------

    #[test]
    fn rescale_is_a_noop_when_the_target_is_unchanged() {
        let probe = Probe::default();
        let mut runtime = EgressFabricRuntime::new(
            manager_config(1).unwrap(),
            group(1, std::slice::from_ref(&probe)),
        )
        .unwrap();

        // Zero outputs on a 1-CPU host: target is 1 either way.
        let touched = runtime
            .rescale(
                crate::config::EgressShardProfile::OutputCount,
                1,
                crate::media::egress::runtime::ResizeReason::Remove,
                shard_config(),
                |_| || -> Result<ProbeBackend, String> { unreachable!("must not grow") },
            )
            .unwrap();

        assert!(touched.is_empty());
        assert_eq!(runtime.snapshots().len(), 1);
        runtime.shutdown();
    }

    #[test]
    fn pending_add_is_sized_before_placement_without_counting_replacements_twice() {
        let config = EgressShardConfig::new(1024, 4, 4, 4, Duration::from_millis(1)).unwrap();
        let probe = Probe::default();
        let mut runtime = EgressFabricRuntime::new(
            EgressManagerConfig::new(1, 1024).unwrap(),
            EgressShardGroup::spawn(
                NonZeroU32::new(1).unwrap(),
                config,
                vec![ProbeBackend {
                    probe: probe.clone(),
                }],
            )
            .unwrap(),
        )
        .unwrap()
        .adaptive(2);
        for i in 0..64 {
            runtime
                .dispatch(EgressCommand::Add(output_spec(&format!("out-{i}"))))
                .unwrap();
        }
        probe.wait_for_commands(64);
        let pending = EgressCommand::Add(output_spec("pending"));
        let reason = runtime.reason_for(&pending);
        let touched = runtime
            .rescale(
                crate::config::EgressShardProfile::SrtOutputCount,
                2,
                reason,
                config,
                |_| {
                    let probe = Probe::default();
                    move || Ok::<_, String>(ProbeBackend { probe })
                },
            )
            .unwrap();
        assert_eq!(
            touched,
            vec![ShardId::new(1)],
            "65th output must see the final pool before connecting"
        );
        runtime.dispatch(pending).unwrap();
        assert_eq!(runtime.manager.output_count(), 65);
        assert!(matches!(
            runtime.reason_for(&EgressCommand::Add(output_spec("pending"))),
            ResizeReason::Remove
        ));
        assert!(
            probe
                .state()
                .commands
                .iter()
                .all(|command| !command.starts_with("remove:")),
            "growing for the pending output must not reconnect the 64 live outputs"
        );
        runtime.shutdown();
    }

    fn roomy_runtime(probes: &[Probe]) -> (EgressFabricRuntime, EgressShardConfig) {
        let config = EgressShardConfig::new(1024, 4, 4, 4, Duration::from_millis(1)).unwrap();
        let count = NonZeroU32::new(probes.len() as u32).unwrap();
        let backends = probes
            .iter()
            .cloned()
            .map(|probe| ProbeBackend { probe })
            .collect::<Vec<_>>();
        let runtime = EgressFabricRuntime::new(
            EgressManagerConfig::new(count.get(), 1024).unwrap(),
            EgressShardGroup::spawn(count, config, backends).unwrap(),
        )
        .unwrap();
        (runtime, config)
    }

    fn no_remove(probe: &Probe) -> bool {
        probe
            .state()
            .commands
            .iter()
            .all(|command| !command.starts_with("remove:"))
    }

    #[test]
    fn growing_the_pool_never_reconnects_a_live_output() {
        let old_probe = Probe::default();
        let (mut runtime, config) = roomy_runtime(std::slice::from_ref(&old_probe));
        for i in 0..200 {
            runtime
                .dispatch(EgressCommand::Add(output_spec(&format!("old-{i}"))))
                .unwrap();
        }
        old_probe.wait_for_commands(200);

        let new_probe = Probe::default();
        let touched = runtime
            .rescale(
                crate::config::EgressShardProfile::OutputCount,
                2,
                ResizeReason::Remove,
                config,
                |_| {
                    let probe = new_probe.clone();
                    move || Ok::<_, String>(ProbeBackend { probe })
                },
            )
            .unwrap();

        assert_eq!(touched, vec![ShardId::new(1)]);
        assert_eq!(runtime.snapshots().len(), 2);
        for i in 0..200 {
            let live = runtime
                .manager
                .desired_output(&OutputId::new(format!("old-{i}")))
                .unwrap();
            assert_eq!(live.shard_id(), ShardId::new(0), "old-{i} stayed put");
        }
        assert!(no_remove(&old_probe));
        assert_eq!(
            old_probe.state().commands.len(),
            200,
            "no replayed Add either"
        );

        for i in 0..100 {
            runtime
                .dispatch(EgressCommand::Add(output_spec(&format!("new-{i}"))))
                .unwrap();
        }
        new_probe.wait_for_commands(1);
        assert!(
            new_probe
                .state()
                .commands
                .iter()
                .all(|command| command.starts_with("add:new-")),
            "only new outputs reach the new shard"
        );
        assert!(no_remove(&old_probe) && no_remove(&new_probe));
        runtime.shutdown();
    }

    #[test]
    fn shrinking_waits_for_the_tail_to_empty_and_never_reconnects_live_outputs() {
        let probes = [Probe::default(), Probe::default()];
        let (mut runtime, config) = roomy_runtime(&probes);
        for i in 0..40 {
            runtime
                .dispatch(EgressCommand::Add(output_spec(&format!("out-{i}"))))
                .unwrap();
        }
        let on_tail: Vec<OutputId> = (0..40)
            .map(|i| OutputId::new(format!("out-{i}")))
            .filter(|id| runtime.manager.desired_output(id).unwrap().shard_id() == ShardId::new(1))
            .collect();
        assert!(!on_tail.is_empty());
        probes[0].wait_for_commands(40 - on_tail.len());
        probes[1].wait_for_commands(on_tail.len());

        let shrink = |runtime: &mut EgressFabricRuntime| {
            runtime
                .rescale(
                    crate::config::EgressShardProfile::OutputCount,
                    2,
                    ResizeReason::Remove,
                    config,
                    |_| || -> Result<ProbeBackend, String> { unreachable!("must not grow") },
                )
                .unwrap()
        };
        assert!(
            shrink(&mut runtime).is_empty(),
            "live outputs keep the tail running"
        );
        assert_eq!(runtime.snapshots().len(), 2);
        assert!(no_remove(&probes[0]) && no_remove(&probes[1]));

        for i in 0..20 {
            runtime
                .dispatch(EgressCommand::Add(output_spec(&format!("later-{i}"))))
                .unwrap();
            let shard = runtime
                .manager
                .desired_output(&OutputId::new(format!("later-{i}")))
                .unwrap()
                .shard_id();
            assert_eq!(
                shard,
                ShardId::new(0),
                "a retiring shard takes no new output"
            );
        }
        for id in &on_tail {
            runtime.dispatch(EgressCommand::Remove(id.clone())).unwrap();
        }
        assert_eq!(shrink(&mut runtime), vec![ShardId::new(1)]);
        assert_eq!(runtime.snapshots().len(), 1);
        runtime.shutdown();
    }

    #[test]
    fn fabric_feed_wakes_follow_ring_growth_rescale_and_stop_on_drop() {
        use crate::media::egress::journal::{FeedEpoch, RingFeed};
        use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
        use crate::media::ring_buffer::RingBuffer;

        let packet = |timestamp| MediaPacket {
            media_type: MediaType::Video,
            format: PayloadFormat::Raw,
            is_keyframe: true,
            track_index: 0,
            pts: timestamp,
            dts: timestamp,
            payload: bytes::Bytes::new(),
        };
        let old_ring = Arc::new(RingBuffer::new(4));
        let feed = RingFeed::new(old_ring.clone(), Arc::new(FeedEpoch::new()));
        let probes = [Probe::default(), Probe::default()];
        let group = group(2, &probes);
        let handles = Arc::new(ArcSwap::from_pointee(vec![
            group.feed_wake_handles()[0].clone(),
        ]));
        let subscription =
            subscribe_fabric_wakes("test", FeedId::new("feed-1"), &feed, handles.clone());

        // One wake on subscribe, then one per publication, delivered on the
        // publishing thread.
        probes[0].wait_for_completed_feed_wakes(1);
        old_ring.push(packet(0));
        probes[0].wait_for_completed_feed_wakes(2);

        // Sealing wakes; the replacement ring inherits the subscription.
        let new_ring = Arc::new(RingBuffer::new_continuing(8, old_ring.get_write_idx()));
        new_ring.seed_readable_tail_from(&old_ring);
        old_ring.seal_and_forward(new_ring.clone());
        probes[0].wait_for_completed_feed_wakes(3);
        new_ring.push(packet(33));
        probes[0].wait_for_completed_feed_wakes(4);
        assert_eq!(feed.head_sequence(), 2);

        // A shard added to the shared list (as `rescale` does) is woken by
        // the existing subscription.
        handles.store(Arc::new(group.feed_wake_handles()));
        new_ring.push(packet(66));
        probes[0].wait_for_completed_feed_wakes(5);
        probes[1].wait_for_completed_feed_wakes(1);

        // Dropping the subscription stops wakes: the gates are clear, so a
        // wake would be sent if anything were still subscribed.
        drop(subscription);
        assert!(new_ring.publication_subscribers().is_empty());
        new_ring.push(packet(99));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(probes[0].state().feed_wake_commands, 5);
        assert_eq!(probes[1].state().feed_wake_commands, 1);
        group.shutdown_and_join();
    }
}
