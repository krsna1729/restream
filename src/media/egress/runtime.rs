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
use crate::media::ring_buffer::{PublishSubscribers, PublishWake, RingBuffer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EgressFabricRuntimeError {
    ShardCountMismatch { expected: usize, actual: usize },
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
        })
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

    /// Grow or shrink the shard pool to match
    /// `target_egress_fabric_shards(self.manager.output_count(), effective_cpus)`
    /// (`src/config.rs`), then rehome only the outputs whose assignment
    /// actually changed. Callers dispatch this right after every
    /// `Add`/`Remove` (see the four `engine_*_egress_fabric.rs` files) —
    /// event-driven, no background timer. A no-op (no allocation, no
    /// rehoming) on the common case where the target hasn't changed.
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
        shard_config: EgressShardConfig,
        mut factory_for: G,
    ) -> Result<Vec<ShardId>, E>
    where
        B: EgressShardBackend,
        E: Send + 'static,
        F: FnOnce() -> Result<B, E> + Send + 'static,
        G: FnMut(ShardId) -> F,
    {
        let target = crate::config::target_egress_fabric_shards(
            profile,
            self.manager.output_count(),
            effective_cpus,
        ) as usize;

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
        while grow_error.is_none() && self.group.shard_count() > target {
            let Some(shard_id) = self.group.shrink() else {
                break;
            };
            touched.push(shard_id);
        }

        if !touched.is_empty() {
            if let Some(new_count) =
                NonZeroU32::new(u32::try_from(self.group.shard_count()).unwrap_or(1))
            {
                self.observe_queued_commands();
                let group = &self.group;
                let _ = self.manager.rehome(new_count, |shard_id, command| {
                    group.try_send_to(shard_id, command)
                });
            }
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
    struct Probe {
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

    fn group(shards: u32, probes: &[Probe]) -> EgressShardGroup {
        let backends = probes
            .iter()
            .cloned()
            .map(|probe| ProbeBackend { probe })
            .collect::<Vec<_>>();
        EgressShardGroup::spawn(NonZeroU32::new(shards).unwrap(), shard_config(), backends).unwrap()
    }

    fn output_spec(id: &str) -> OutputSpec {
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
                shard_config(),
                |_| || -> Result<ProbeBackend, String> { unreachable!("must not grow") },
            )
            .unwrap();

        assert!(touched.is_empty());
        assert_eq!(runtime.snapshots().len(), 1);
        runtime.shutdown();
    }

    #[test]
    fn rescale_grows_and_rehomes_when_output_count_crosses_the_threshold() {
        let probe = Probe::default();
        // A larger command-channel capacity than the shared `manager_config`
        // helper's: this test dispatches without waiting for the shard to
        // take commands, so 200 queued `Add`s plus the `Remove`+`Add` pairs
        // `rehome` issues for moved outputs must all fit under one cap --
        // both the manager's soft admission-control depth and the real
        // shard mpsc channel `EgressShardHandle::spawn` sizes from
        // `EgressShardConfig`'s first argument (the shared `shard_config()`
        // helper's 16 is fine for other tests but too small for 200 rapid
        // sends here).
        let big_shard_config =
            EgressShardConfig::new(1024, 4, 4, 4, Duration::from_millis(1)).unwrap();
        let mut runtime = EgressFabricRuntime::new(
            EgressManagerConfig::new(1, 1024).unwrap(),
            EgressShardGroup::spawn(
                NonZeroU32::new(1).unwrap(),
                big_shard_config,
                vec![ProbeBackend {
                    probe: probe.clone(),
                }],
            )
            .unwrap(),
        )
        .unwrap();
        for i in 0..200 {
            runtime
                .dispatch(EgressCommand::Add(output_spec(&format!("out-{i}"))))
                .unwrap();
        }
        probe.wait_for_commands(200);

        let new_probe = Probe::default();
        let touched = runtime
            .rescale(
                crate::config::EgressShardProfile::OutputCount,
                2,
                big_shard_config,
                |_| {
                    let probe = new_probe.clone();
                    move || Ok::<_, String>(ProbeBackend { probe })
                },
            )
            .unwrap();

        assert_eq!(touched, vec![ShardId::new(1)]);
        assert_eq!(runtime.snapshots().len(), 2);
        // Rehoming moved some outputs onto the new shard (as a Remove on
        // shard 0 + an Add on shard 1) -- with 200 outputs split across 2
        // shards by rendezvous hashing, the new shard gets a real share,
        // not zero.
        let (lock, condvar) = &*new_probe.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                !state
                    .commands
                    .iter()
                    .any(|command| command.starts_with("add:"))
            })
            .unwrap();
        assert!(
            result
                .0
                .commands
                .iter()
                .any(|command| command.starts_with("add:")),
            "expected at least one output rehomed onto the new shard"
        );
        drop(result);

        runtime.shutdown();
    }

    #[test]
    fn rescale_shrinks_and_rehomes_when_output_count_drops() {
        let probe_zero = Probe::default();
        let probe_one = Probe::default();
        let mut runtime = EgressFabricRuntime::new(
            manager_config(2).unwrap(),
            group(2, &[probe_zero.clone(), probe_one.clone()]),
        )
        .unwrap();
        // Force a known assignment split isn't needed here -- we only
        // need at least one output to survive on shard 0 so the drained
        // shard-1 outputs (if any) have somewhere to land, and to prove
        // the group actually shrinks back to 1 shard.
        for i in 0..5 {
            runtime
                .dispatch(EgressCommand::Add(output_spec(&format!("out-{i}"))))
                .unwrap();
        }
        assert_eq!(runtime.snapshots().len(), 2);

        // Zero live outputs after removal: target collapses to 1 shard on
        // any CPU count.
        for i in 0..5 {
            runtime
                .dispatch(EgressCommand::Remove(OutputId::new(format!("out-{i}"))))
                .unwrap();
        }
        let touched = runtime
            .rescale(
                crate::config::EgressShardProfile::OutputCount,
                1,
                shard_config(),
                |_| || -> Result<ProbeBackend, String> { unreachable!("must not grow") },
            )
            .unwrap();

        assert_eq!(touched, vec![ShardId::new(1)]);
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
