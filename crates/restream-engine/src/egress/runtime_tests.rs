use super::*;
use crate::egress::feed::EgressFeed;
use std::num::NonZeroU32;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::egress::command::{EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec, ShardId};
use crate::egress::manager::{EgressManagerConfigError, ManagerCommandOutcome};
use crate::egress::policy::LeafPolicy;
use crate::egress::shard::{
    EgressShardBackend, EgressShardCommandEffect, EgressShardConfig, EgressShardHealth,
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
            EgressCommand::Abandon(output_id) => format!("abandon:{output_id}"),
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
        let outcome = runtime.dispatch(EgressCommand::Add(output_spec(&format!("out-{index}"))));
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
            crate::host_capacity::HostCapacity { cpus: 1 },
            crate::egress::runtime::ResizeReason::Remove,
            shard_config(),
            |_| || -> Result<ProbeBackend, String> { unreachable!("must not grow") },
        )
        .unwrap();

    assert!(touched.is_empty());
    assert_eq!(runtime.snapshots().len(), 1);
    runtime.shutdown();
}

/// A probe whose shard thread panics on the Add of an output named
/// `boom-*` when `panics` is set.
#[derive(Debug)]
struct PanicOnAdd {
    panics: bool,
    backend: ProbeBackend,
}

impl EgressShardBackend for PanicOnAdd {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        if self.panics
            && matches!(&command, EgressCommand::Add(spec) if spec.id.as_str().starts_with("boom"))
        {
            crate::test_support::with_expected_panic_suppressed(|| panic!("scripted shard panic"));
        }
        self.backend.on_command(command)
    }
}

/// Two shards; shard 0 panics on a `boom-*` Add.
fn panicking_runtime(probes: &[Probe; 2]) -> EgressFabricRuntime {
    let backends = probes
        .iter()
        .enumerate()
        .map(|(index, probe)| PanicOnAdd {
            panics: index == 0,
            backend: ProbeBackend {
                probe: probe.clone(),
            },
        })
        .collect::<Vec<_>>();
    EgressFabricRuntime::new(
        manager_config(2).unwrap(),
        EgressShardGroup::spawn(NonZeroU32::new(2).unwrap(), shard_config(), backends).unwrap(),
    )
    .unwrap()
}

/// An output that rendezvous places on `shard` of two, with its own
/// termination flag, as the application task holds it.
fn output_on(shard: u32, prefix: &str) -> (OutputSpec, Arc<std::sync::atomic::AtomicBool>) {
    output_on_pool(shard, NonZeroU32::new(2).unwrap(), prefix)
}

fn output_on_pool(
    shard: u32,
    shards: NonZeroU32,
    prefix: &str,
) -> (OutputSpec, Arc<std::sync::atomic::AtomicBool>) {
    let mut spec = (0..)
        .map(|index| output_spec(&format!("{prefix}-{index}")))
        .find(|spec| {
            crate::egress::manager::assign_output_to_shard(&spec.id, shards) == ShardId::new(shard)
        })
        .unwrap();
    let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
    spec.progress.terminated_unexpectedly = Some(ended.clone());
    (spec, ended)
}

fn panic_shard_zero(runtime: &mut EgressFabricRuntime) {
    runtime
        .dispatch(EgressCommand::Add(output_on(0, "boom").0))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !runtime.snapshots()[0].panicked {
        assert!(Instant::now() < deadline, "the shard never panicked");
        std::thread::yield_now();
    }
}

fn rescale_with(
    runtime: &mut EgressFabricRuntime,
    replacement: Result<Probe, &'static str>,
) -> Result<Vec<ShardId>, &'static str> {
    runtime.rescale(
        crate::config::EgressShardProfile::OutputCount,
        crate::host_capacity::HostCapacity { cpus: 2 },
        ResizeReason::Observe,
        shard_config(),
        |_| {
            let replacement = replacement.clone();
            move || replacement.map(|probe| ProbeBackend { probe })
        },
    )
}

fn ended(flag: &std::sync::atomic::AtomicBool) -> bool {
    flag.load(std::sync::atomic::Ordering::Acquire)
}

/// A shard that panics must not strand its outputs. Until the next
/// rescale, commands to it fail at once and nothing is forgotten; the
/// rescale gives it a new thread and ends each of ITS outputs as
/// unexpectedly terminated through the progress sink the application
/// task polls, so the task retries and the retry is delivered on the
/// replacement. The other shard's outputs are untouched, and shard
/// health reports the panic until the replacement runs.
#[test]
fn a_panicked_shard_is_replaced_and_its_retried_outputs_are_delivered() {
    let probes = [Probe::default(), Probe::default()];
    let mut runtime = panicking_runtime(&probes);
    let (lost, lost_ended) = output_on(0, "lost");
    let (kept, kept_ended) = output_on(1, "kept");
    runtime.dispatch(EgressCommand::Add(lost.clone())).unwrap();
    runtime.dispatch(EgressCommand::Add(kept.clone())).unwrap();
    probes[0].wait_for_commands(1);
    probes[1].wait_for_commands(1);
    panic_shard_zero(&mut runtime);

    // Between the panic and the replacement: a command to the dead
    // shard fails at once, and the output is not forgotten.
    assert!(matches!(
        runtime.dispatch(EgressCommand::Remove(lost.id.clone())),
        Err(EgressManagerDispatchError::Dispatch { shard_id, .. }) if shard_id == ShardId::new(0)
    ));
    assert!(runtime.manager.desired_output(&lost.id).is_some());
    assert!(!ended(&lost_ended));
    let health = |runtime: &EgressFabricRuntime| {
        runtime
            .heartbeat(Instant::now(), Duration::from_secs(60))
            .into_iter()
            .map(|heartbeat| heartbeat.state)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        health(&runtime),
        [EgressShardHealth::Panicked, EgressShardHealth::Healthy]
    );

    let replacement = Probe::default();
    assert_eq!(
        rescale_with(&mut runtime, Ok(replacement.clone())),
        Ok(vec![ShardId::new(0)])
    );
    assert!(
        ended(&lost_ended),
        "the output's task must see it end, or it never retries"
    );
    assert!(runtime.manager.desired_output(&lost.id).is_none());
    assert!(
        !ended(&kept_ended),
        "the other shard's output keeps running"
    );
    assert_eq!(
        runtime
            .manager
            .desired_output(&kept.id)
            .map(|kept| kept.shard_id()),
        Some(ShardId::new(1))
    );
    assert_eq!(probes[1].state().commands, [format!("add:{}", kept.id)]);
    assert_eq!(
        health(&runtime),
        [EgressShardHealth::Healthy, EgressShardHealth::Healthy]
    );

    // The application task's retry: the next attempt of the output.
    let mut retry = lost.clone();
    retry.generation = 2;
    assert_eq!(
        runtime.dispatch(EgressCommand::Add(retry)),
        Ok(ManagerCommandOutcome::Enqueued {
            shard_id: ShardId::new(0)
        })
    );
    replacement.wait_for_commands(1);
    assert_eq!(replacement.state().commands, [format!("add:{}", lost.id)]);
    runtime.shutdown();
}

/// A dead shard whose replacement cannot start takes no new output:
/// placement skips it until a replacement runs.
#[test]
fn placement_skips_a_dead_shard_until_its_replacement_starts() {
    let probes = [Probe::default(), Probe::default()];
    let mut runtime = panicking_runtime(&probes);
    panic_shard_zero(&mut runtime);
    assert_eq!(
        rescale_with(&mut runtime, Err("no io_uring")),
        Err("no io_uring")
    );

    let (diverted, _) = output_on(0, "diverted");
    assert_eq!(
        runtime.dispatch(EgressCommand::Add(diverted.clone())),
        Ok(ManagerCommandOutcome::Enqueued {
            shard_id: ShardId::new(1)
        })
    );
    probes[1].wait_for_commands(1);

    let replacement = Probe::default();
    assert_eq!(
        rescale_with(&mut runtime, Ok(replacement.clone())),
        Ok(vec![ShardId::new(0)])
    );
    // One output only needs one placement shard, so that rescale also
    // shrank placement to shard 0; widen it again, as later growth
    // would, so the replacement competes with live shard 1.
    runtime.manager.set_placement(NonZeroU32::new(2).unwrap());
    let (placed, _) = output_on(0, "placed");
    assert_eq!(
        runtime.dispatch(EgressCommand::Add(placed.clone())),
        Ok(ManagerCommandOutcome::Enqueued {
            shard_id: ShardId::new(0)
        })
    );
    replacement.wait_for_commands(1);
    assert_eq!(replacement.state().commands, [format!("add:{}", placed.id)]);
    runtime.shutdown();
}

#[test]
fn all_panicked_shards_are_isolated_before_any_replacement_can_fail() {
    let count = NonZeroU32::new(3).unwrap();
    let probes = [Probe::default(), Probe::default(), Probe::default()];
    let backends = probes
        .iter()
        .enumerate()
        .map(|(index, probe)| PanicOnAdd {
            panics: index < 2,
            backend: ProbeBackend {
                probe: probe.clone(),
            },
        })
        .collect::<Vec<_>>();
    let mut runtime = EgressFabricRuntime::new(
        manager_config(3).unwrap(),
        EgressShardGroup::spawn(count, shard_config(), backends).unwrap(),
    )
    .unwrap()
    .adaptive(3);
    let (lost_zero, zero_ended) = output_on_pool(0, count, "lost-zero");
    let (lost_one, one_ended) = output_on_pool(1, count, "lost-one");
    let (kept, kept_ended) = output_on_pool(2, count, "kept");
    for output in [&lost_zero, &lost_one, &kept] {
        runtime
            .dispatch(EgressCommand::Add(output.clone()))
            .unwrap();
    }
    for probe in &probes {
        probe.wait_for_commands(1);
    }
    for shard in 0..2 {
        runtime
            .dispatch(EgressCommand::Add(output_on_pool(shard, count, "boom").0))
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while !runtime.snapshots()[..2]
        .iter()
        .all(|snapshot| snapshot.panicked)
    {
        assert!(Instant::now() < deadline, "both shards must panic");
        std::thread::yield_now();
    }

    let replacement = Probe::default();
    let mut diverted = Vec::new();
    for recover_one in [false, true] {
        let mut attempts = Vec::new();
        let result = runtime.rescale(
            crate::config::EgressShardProfile::OutputCount,
            crate::host_capacity::HostCapacity { cpus: 3 },
            ResizeReason::Remove,
            shard_config(),
            |shard| {
                assert!(
                    ended(&zero_ended) && ended(&one_ended),
                    "all affected tasks must be signaled before trying any replacement"
                );
                attempts.push(shard);
                let replacement = replacement.clone();
                move || {
                    if shard == ShardId::new(0) {
                        Err("shard zero unavailable")
                    } else if !recover_one {
                        Err("shard one unavailable")
                    } else {
                        Ok(ProbeBackend { probe: replacement })
                    }
                }
            },
        );
        assert_eq!(result, Err("shard zero unavailable"));
        assert_eq!(attempts, [ShardId::new(0), ShardId::new(1)]);
        assert!(runtime.manager.desired_output(&lost_zero.id).is_none());
        assert!(runtime.manager.desired_output(&lost_one.id).is_none());
        assert!(!ended(&kept_ended), "the healthy sibling must keep running");
        assert_eq!(
            runtime.manager.desired_output(&kept.id).unwrap().shard_id(),
            ShardId::new(2)
        );
        let snapshots = runtime.snapshots();
        assert!(snapshots[0].panicked);
        assert_eq!(snapshots[1].panicked, !recover_one);
        assert!(!snapshots[2].panicked && !snapshots[2].stopped);

        if !recover_one {
            // Both dead shards remain excluded even though replacement zero
            // failed first. New outputs hashing to either must reach shard 2.
            for shard in 0..2 {
                let spec = output_on_pool(shard, count, "diverted").0;
                assert_eq!(
                    runtime.dispatch(EgressCommand::Add(spec.clone())),
                    Ok(ManagerCommandOutcome::Enqueued {
                        shard_id: ShardId::new(2)
                    }),
                );
                diverted.push(format!("add:{}", spec.id));
            }
            probes[2].wait_for_commands(3);
        }
    }
    // Failure zero must not suppress replacement one's success or disturb
    // healthy outputs, and the retried output must reach that replacement.
    let mut retry = lost_one.clone();
    retry.generation += 1;
    assert_eq!(
        runtime.dispatch(EgressCommand::Add(retry)),
        Ok(ManagerCommandOutcome::Enqueued {
            shard_id: ShardId::new(1)
        }),
    );
    replacement.wait_for_commands(1);
    assert_eq!(
        replacement.state().commands,
        [format!("add:{}", lost_one.id)]
    );
    let mut expected = vec![format!("add:{}", kept.id)];
    expected.extend(diverted);
    assert_eq!(probes[2].state().commands, expected);
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
            crate::host_capacity::HostCapacity { cpus: 2 },
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

pub(super) fn roomy_runtime(probes: &[Probe]) -> (EgressFabricRuntime, EgressShardConfig) {
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
            crate::host_capacity::HostCapacity { cpus: 2 },
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
                crate::host_capacity::HostCapacity { cpus: 2 },
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
    use crate::egress::journal::{FeedEpoch, RingFeed};
    use restream_core::{MediaPacket, MediaType, PayloadFormat};
    use restream_sync::RingBuffer;

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
    let subscription = subscribe_fabric_wakes(
        "test",
        FeedId::new("feed-1"),
        feed.publication_ring().publication_subscribers(),
        handles.clone(),
    );

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
