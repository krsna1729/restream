use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::media::egress::command::{EgressCommand, ShardId};
use crate::media::egress::manager::{
    EgressManager, EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome,
};
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardConfig, EgressShardGroup, EgressShardGroupError,
    EgressShardHeartbeat, EgressShardSnapshot, FeedWakeHandle,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EgressFabricRuntimeError {
    ShardCountMismatch { expected: usize, actual: usize },
}

#[derive(Debug)]
pub(crate) struct EgressFabricRuntime {
    manager: EgressManager,
    group: EgressShardGroup,
    /// Shared with the feed-wake watcher task (`retain_*_fabric_runtime`'s
    /// `tokio::spawn`), which reads through this on every wake instead of
    /// iterating a `Vec` snapshot captured once at watcher-startup time.
    /// Without sharing it, a shard added later by `rescale` would have no
    /// way to reach that already-running watcher — its leaves would still
    /// work (via the shard's own idle-poll fallback) but lose the fast
    /// feed-wake path indefinitely.
    wake_handles: Arc<Mutex<Vec<FeedWakeHandle>>>,
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
        let wake_handles = Arc::new(Mutex::new(group.feed_wake_handles()));
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
        self.manager.dispatch_to_group(command, &self.group)
    }

    /// Shared handle list for the feed-wake watcher task to read through
    /// on every wake (see the field doc on `wake_handles`). Clone the
    /// returned `Arc` into the watcher at startup; `rescale` keeps the
    /// pointed-to `Vec` fresh as shards grow or shrink.
    pub(crate) fn feed_wake_handles(&self) -> Arc<Mutex<Vec<FeedWakeHandle>>> {
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
    /// `backend_for` is fallible because constructing a real shard's
    /// readiness poller is (`TcpEgressPoller::new`/`SrtFabricPoller::new`
    /// both do a real `epoll_create1`/equivalent syscall that can fail
    /// under resource exhaustion) — a failed grow attempt stops growing
    /// for this call (logged by the caller via the returned `Err`) rather
    /// than panicking or silently continuing with fewer shards than
    /// `touched` would suggest; whatever grew successfully before the
    /// failure stays, so this call can simply be retried on the next
    /// `Add`/`Remove`.
    ///
    /// Returns the shard ids touched (grown or shut down) on success, for
    /// logging.
    pub(crate) fn rescale<B, F, E>(
        &mut self,
        effective_cpus: usize,
        shard_config: EgressShardConfig,
        mut backend_for: F,
    ) -> Result<Vec<ShardId>, E>
    where
        B: EgressShardBackend,
        F: FnMut(ShardId) -> Result<B, E>,
    {
        let target =
            crate::config::target_egress_fabric_shards(self.manager.output_count(), effective_cpus)
                as usize;

        let mut touched = Vec::new();
        let mut grow_error = None;
        while self.group.shard_count() < target {
            let shard_id =
                ShardId::new(u32::try_from(self.group.shard_count()).unwrap_or(u32::MAX));
            match backend_for(shard_id) {
                Ok(backend) => touched.push(self.group.grow(shard_config, backend)),
                Err(error) => {
                    grow_error = Some(error);
                    break;
                }
            }
        }
        while grow_error.is_none() && self.group.shard_count() > target {
            let Some((shard_id, _snapshot)) = self.group.shrink() else {
                break;
            };
            touched.push(shard_id);
        }

        if !touched.is_empty() {
            if let Some(new_count) =
                NonZeroU32::new(u32::try_from(self.group.shard_count()).unwrap_or(1))
            {
                let group = &self.group;
                let _ = self.manager.rehome(new_count, |shard_id, command| {
                    group.try_send_to(shard_id, command)
                });
            }
            // Grown/shut-down shards changed the group's real handle set;
            // refresh the shared list the feed-wake watcher reads through
            // (see the `wake_handles` field doc) so it stays correct
            // without the watcher needing to know shards can resize at
            // all -- including on a partial failure below, since whatever
            // grew before the failure is still real and needs a wake path.
            *self.wake_handles.lock().unwrap() = self.group.feed_wake_handles();
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

        fn state(&self) -> ProbeState {
            let state = self.inner.0.lock().unwrap();
            ProbeState {
                commands: state.commands.clone(),
                shutdowns: state.shutdowns,
            }
        }
    }

    #[derive(Debug)]
    struct ProbeBackend {
        probe: Probe,
    }

    impl EgressShardBackend for ProbeBackend {
        fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
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
            .rescale(1, shard_config(), |_| -> Result<ProbeBackend, String> {
                unreachable!("must not grow")
            })
            .unwrap();

        assert!(touched.is_empty());
        assert_eq!(runtime.snapshots().len(), 1);
        runtime.shutdown();
    }

    #[test]
    fn rescale_grows_and_rehomes_when_output_count_crosses_the_threshold() {
        let probe = Probe::default();
        // A larger command-channel capacity than the shared `manager_config`
        // helper's: nothing in this test acks commands (no
        // `complete_one_command` call, unlike production's real feedback
        // loop), so 200 unacked `Add`s plus the `Remove`+`Add` pairs
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
            .rescale(2, big_shard_config, |_| {
                Ok::<_, String>(ProbeBackend {
                    probe: new_probe.clone(),
                })
            })
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
            .rescale(1, shard_config(), |_| -> Result<ProbeBackend, String> {
                unreachable!("must not grow")
            })
            .unwrap();

        assert_eq!(touched, vec![ShardId::new(1)]);
        assert_eq!(runtime.snapshots().len(), 1);
        runtime.shutdown();
    }
}
