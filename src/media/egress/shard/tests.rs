use super::*;
use crate::media::egress::command::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::manager::{
    DesiredOutput, EgressManager, EgressManagerConfig, EgressManagerDispatchError,
    ManagerCommandOutcome,
};
use crate::media::egress::policy::LeafPolicy;
use std::num::NonZeroU32;
use std::sync::Condvar;

#[derive(Debug, Default)]
struct ProbeState {
    commands: Vec<String>,
    timers: Vec<String>,
    media_ticks: u64,
    shutdowns: u64,
    generations: std::collections::HashMap<String, u64>,
}

#[derive(Debug, Clone, Default)]
struct Probe {
    inner: Arc<(Mutex<ProbeState>, Condvar)>,
}

impl Probe {
    fn wait_for_media_ticks(&self, target: u64) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.media_ticks < target
            })
            .unwrap();
        assert!(result.0.media_ticks >= target);
    }

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

    fn wait_for_timers(&self, target: usize) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.timers.len() < target
            })
            .unwrap();
        assert!(result.0.timers.len() >= target);
    }

    fn state(&self) -> ProbeState {
        let state = self.inner.0.lock().unwrap();
        ProbeState {
            commands: state.commands.clone(),
            timers: state.timers.clone(),
            media_ticks: state.media_ticks,
            shutdowns: state.shutdowns,
            generations: state.generations.clone(),
        }
    }
}

#[derive(Debug)]
struct ProbeBackend {
    probe: Probe,
}

impl EgressShardBackend for ProbeBackend {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        let label = command_label(&command);
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.commands.push(label);
        if let EgressCommand::Add(spec) | EgressCommand::Update(spec) = &command {
            state
                .generations
                .insert(spec.id.as_str().to_string(), spec.generation);
        }
        condvar.notify_all();
        EgressShardCommandEffect::Continue
    }

    fn timer_generation(&self, output_id: &OutputId) -> Option<u64> {
        let state = self.probe.inner.0.lock().unwrap();
        state.generations.get(output_id.as_str()).copied()
    }

    fn on_timer(&mut self, output_id: OutputId, generation: u64) -> EgressShardCommandEffect {
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.timers.push(format!("{output_id}:{generation}"));
        condvar.notify_all();
        EgressShardCommandEffect::Continue
    }

    fn on_media_tick(&mut self) {
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.media_ticks += 1;
        condvar.notify_all();
    }

    fn on_shutdown(&mut self) {
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.shutdowns += 1;
        condvar.notify_all();
    }
}

#[derive(Debug)]
enum ScriptBackend {
    Probe(ProbeBackend),
    Panic,
}

impl EgressShardBackend for ScriptBackend {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        match self {
            Self::Probe(backend) => backend.on_command(command),
            Self::Panic => panic!("scripted shard panic"),
        }
    }

    fn on_media_tick(&mut self) {
        match self {
            Self::Probe(backend) => backend.on_media_tick(),
            Self::Panic => {}
        }
    }

    fn timer_generation(&self, output_id: &OutputId) -> Option<u64> {
        match self {
            Self::Probe(backend) => backend.timer_generation(output_id),
            Self::Panic => None,
        }
    }

    fn on_timer(&mut self, output_id: OutputId, generation: u64) -> EgressShardCommandEffect {
        match self {
            Self::Probe(backend) => backend.on_timer(output_id, generation),
            Self::Panic => EgressShardCommandEffect::Continue,
        }
    }

    fn on_shutdown(&mut self) {
        match self {
            Self::Probe(backend) => backend.on_shutdown(),
            Self::Panic => {}
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Gate {
    inner: Arc<(Mutex<GateState>, Condvar)>,
}

#[derive(Debug, Default)]
struct GateState {
    entered: bool,
    released: bool,
}

impl Gate {
    fn wait_until_entered(&self) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| !state.entered)
            .unwrap();
        assert!(result.0.entered);
    }

    fn release(&self) {
        let (lock, condvar) = &*self.inner;
        let mut state = lock.lock().unwrap();
        state.released = true;
        condvar.notify_all();
    }
}

#[derive(Debug)]
struct BlockingBackend {
    gate: Gate,
}

impl EgressShardBackend for BlockingBackend {
    fn on_command(&mut self, _command: EgressCommand) -> EgressShardCommandEffect {
        let (lock, condvar) = &*self.gate.inner;
        let mut state = lock.lock().unwrap();
        state.entered = true;
        condvar.notify_all();
        let _guard = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| !state.released)
            .unwrap();
        EgressShardCommandEffect::Continue
    }
}

#[derive(Debug)]
struct TimerBackend {
    probe: Probe,
    delay: Duration,
}

impl EgressShardBackend for TimerBackend {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        let label = command_label(&command);
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.commands.push(label);
        let (EgressCommand::Add(spec) | EgressCommand::Update(spec)) = command else {
            condvar.notify_all();
            return EgressShardCommandEffect::Continue;
        };
        state
            .generations
            .insert(spec.id.as_str().to_string(), spec.generation);
        condvar.notify_all();
        EgressShardCommandEffect::ScheduleTimer {
            output_id: spec.id,
            generation: spec.generation,
            fire_at: Instant::now() + self.delay,
        }
    }

    fn timer_generation(&self, output_id: &OutputId) -> Option<u64> {
        let state = self.probe.inner.0.lock().unwrap();
        state.generations.get(output_id.as_str()).copied()
    }

    fn on_timer(&mut self, output_id: OutputId, generation: u64) -> EgressShardCommandEffect {
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.timers.push(format!("{output_id}:{generation}"));
        condvar.notify_all();
        EgressShardCommandEffect::Continue
    }
}

fn config(capacity: usize, command_budget: usize) -> EgressShardConfig {
    EgressShardConfig::new(
        capacity,
        command_budget,
        command_budget,
        Duration::from_millis(10),
    )
    .unwrap()
}

fn output_spec(id: &str) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation: 1,
        feed: FeedId::new("feed-1"),
        protocol: ProtocolSpec::Rtmp {
            url: "rtmp://localhost/live".into(),
            tls: false,
        },
        policy: LeafPolicy::default(),
    }
}

fn manager(shards: u32) -> EgressManager {
    EgressManager::new(EgressManagerConfig::new(shards, 16).unwrap())
}

fn spec_for_shard(manager: &EgressManager, target: ShardId) -> OutputSpec {
    for index in 0..1_000 {
        let candidate = output_spec(&format!("out-target-{index}"));
        if manager.assign_spec(&candidate) == target {
            return candidate;
        }
    }
    panic!("test fixture could not find output for {target}");
}

fn command_label(command: &EgressCommand) -> String {
    match command {
        EgressCommand::Add(spec) => format!("add:{}", spec.id.as_str()),
        EgressCommand::Update(spec) => format!("update:{}", spec.id.as_str()),
        EgressCommand::Remove(id) => format!("remove:{}", id.as_str()),
        EgressCommand::DrainShard(shard_id) => format!("drain:{}", shard_id.index()),
        EgressCommand::Shutdown => "shutdown".to_string(),
    }
}

#[test]
fn config_rejects_zero_capacity_and_budget() {
    assert_eq!(
        EgressShardConfig::new(0, 1, 1, Duration::ZERO),
        Err(EgressShardConfigError::ZeroCommandCapacity)
    );
    assert_eq!(
        EgressShardConfig::new(1, 0, 1, Duration::ZERO),
        Err(EgressShardConfigError::ZeroCommandBatch)
    );
    assert_eq!(
        EgressShardConfig::new(1, 1, 0, Duration::ZERO),
        Err(EgressShardConfigError::ZeroTimerBatch)
    );
}

#[test]
fn command_channel_is_bounded() {
    let gate = Gate::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(1, 1),
        BlockingBackend { gate: gate.clone() },
    );

    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-a"))),
        Ok(())
    );
    gate.wait_until_entered();
    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-b"))),
        Ok(())
    );
    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-c"))),
        Err(EgressShardSendError::Full)
    );
    gate.release();
    let snapshot = handle.shutdown_and_join();

    assert_eq!(snapshot.shard_id, ShardId::new(0));
    assert!(snapshot.stopped);
}

#[test]
fn command_batch_budget_allows_media_ticks_during_flood() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(16, 2),
        ProbeBackend {
            probe: probe.clone(),
        },
    );

    for i in 0..6 {
        assert_eq!(
            handle.try_send(EgressCommand::Add(output_spec(&format!("out-{i}")))),
            Ok(())
        );
    }
    probe.wait_for_media_ticks(2);
    let snapshot = handle.shutdown_and_join();

    assert!(snapshot.commands_processed >= 4);
    assert!(snapshot.media_ticks >= 2);
    assert!(snapshot.stopped);
}

#[test]
fn timer_batch_budget_allows_media_ticks_during_timer_flood() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        EgressShardConfig::new(16, 16, 2, Duration::from_millis(10)).unwrap(),
        TimerBackend {
            probe: probe.clone(),
            delay: Duration::ZERO,
        },
    );

    for index in 0..6 {
        assert_eq!(
            handle.try_send(EgressCommand::Add(output_spec(&format!(
                "out-timer-{index}"
            )))),
            Ok(())
        );
    }
    probe.wait_for_timers(4);
    let running_snapshot = handle.snapshot();
    let snapshot = handle.shutdown_and_join();

    assert!(snapshot.timers_processed >= 4);
    assert!(running_snapshot.media_ticks >= 1);
    assert!(snapshot.stopped);
}

#[test]
fn stale_timer_generation_is_ignored_on_shard_thread() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(8, 4),
        TimerBackend {
            probe: probe.clone(),
            delay: Duration::from_millis(20),
        },
    );
    let mut first = output_spec("out-stale-timer");
    first.generation = 1;
    let mut second = output_spec("out-stale-timer");
    second.generation = 2;

    assert_eq!(handle.try_send(EgressCommand::Add(first)), Ok(()));
    assert_eq!(handle.try_send(EgressCommand::Update(second)), Ok(()));
    probe.wait_for_timers(1);
    let snapshot = handle.shutdown_and_join();

    assert_eq!(probe.state().timers, vec!["out-stale-timer:2"]);
    assert_eq!(snapshot.timers_processed, 1);
    assert!(snapshot.stopped);
}

#[test]
fn drain_for_other_shard_is_ignored_locally() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(1),
        config(4, 4),
        ProbeBackend {
            probe: probe.clone(),
        },
    );

    assert_eq!(
        handle.try_send(EgressCommand::DrainShard(ShardId::new(0))),
        Ok(())
    );
    probe.wait_for_media_ticks(1);
    let snapshot = handle.shutdown_and_join();

    assert!(probe.state().commands.is_empty());
    assert_eq!(snapshot.shard_id, ShardId::new(1));
}

#[test]
fn shutdown_joins_without_leaking_thread() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(4, 4),
        ProbeBackend {
            probe: probe.clone(),
        },
    );

    let snapshot = handle.shutdown_and_join();

    assert!(snapshot.stopped);
    assert!(!snapshot.panicked);
    assert_eq!(probe.state().shutdowns, 1);
}

#[test]
fn heartbeat_classifies_snapshot_health_states() {
    let now = Instant::now();
    let base = EgressShardSnapshot {
        shard_id: ShardId::new(0),
        loop_iterations: 7,
        commands_processed: 3,
        timers_processed: 2,
        pending_timers: 1,
        media_ticks: 5,
        last_progress_at: Some(now - Duration::from_millis(10)),
        stopped: false,
        panicked: false,
    };

    let healthy =
        EgressShardHeartbeat::from_snapshot(base.clone(), now, Duration::from_millis(100));
    let stalled = EgressShardHeartbeat::from_snapshot(
        EgressShardSnapshot {
            last_progress_at: Some(now - Duration::from_secs(5)),
            ..base.clone()
        },
        now,
        Duration::from_millis(100),
    );
    let stopped = EgressShardHeartbeat::from_snapshot(
        EgressShardSnapshot {
            stopped: true,
            ..base.clone()
        },
        now,
        Duration::from_millis(100),
    );
    let panicked = EgressShardHeartbeat::from_snapshot(
        EgressShardSnapshot {
            panicked: true,
            stopped: true,
            ..base
        },
        now,
        Duration::from_millis(100),
    );

    assert_eq!(healthy.state, EgressShardHealth::Healthy);
    assert_eq!(healthy.loop_iterations, 7);
    assert_eq!(healthy.media_ticks, 5);
    assert_eq!(healthy.progress_age, Some(Duration::from_millis(10)));
    assert_eq!(stalled.state, EgressShardHealth::Stalled);
    assert_eq!(stopped.state, EgressShardHealth::Stopped);
    assert_eq!(panicked.state, EgressShardHealth::Panicked);
}

#[test]
fn heartbeat_treats_missing_progress_as_stalled() {
    let now = Instant::now();
    let heartbeat = EgressShardHeartbeat::from_snapshot(
        EgressShardSnapshot {
            shard_id: ShardId::new(0),
            loop_iterations: 0,
            commands_processed: 0,
            timers_processed: 0,
            pending_timers: 0,
            media_ticks: 0,
            last_progress_at: None,
            stopped: false,
            panicked: false,
        },
        now,
        Duration::from_millis(100),
    );

    assert_eq!(heartbeat.state, EgressShardHealth::Stalled);
    assert_eq!(heartbeat.progress_age, None);
}

#[test]
fn shard_group_starts_fixed_shards_and_routes_by_shard_id() {
    let shard_zero = Probe::default();
    let shard_one = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(4, 4),
        vec![
            ProbeBackend {
                probe: shard_zero.clone(),
            },
            ProbeBackend {
                probe: shard_one.clone(),
            },
        ],
    )
    .unwrap();

    assert_eq!(group.shard_count(), 2);
    assert_eq!(
        group.try_send_to(ShardId::new(0), EgressCommand::Add(output_spec("out-zero"))),
        Ok(())
    );
    assert_eq!(
        group.try_send_to(ShardId::new(1), EgressCommand::Add(output_spec("out-one"))),
        Ok(())
    );
    shard_zero.wait_for_commands(1);
    shard_one.wait_for_commands(1);
    let snapshots = group.shutdown_and_join();

    assert_eq!(shard_zero.state().commands, vec!["add:out-zero"]);
    assert_eq!(shard_one.state().commands, vec!["add:out-one"]);
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.iter().all(|snapshot| snapshot.stopped));
}

#[test]
fn shard_group_heartbeat_reports_running_shard() {
    let probe = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(1).unwrap(),
        config(4, 4),
        vec![ProbeBackend {
            probe: probe.clone(),
        }],
    )
    .unwrap();

    probe.wait_for_media_ticks(1);
    let heartbeat = group.heartbeat(Instant::now(), Duration::from_secs(10));
    let snapshots = group.shutdown_and_join();

    assert_eq!(heartbeat.len(), 1);
    assert_eq!(heartbeat[0].state, EgressShardHealth::Healthy);
    assert_eq!(heartbeat[0].shard_id, ShardId::new(0));
    assert_eq!(snapshots.len(), 1);
}

#[test]
fn manager_dispatch_to_group_routes_add_to_assigned_thread() {
    let mut manager = manager(2);
    let shard_zero = Probe::default();
    let shard_one = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(4, 4),
        vec![
            ProbeBackend {
                probe: shard_zero.clone(),
            },
            ProbeBackend {
                probe: shard_one.clone(),
            },
        ],
    )
    .unwrap();
    let output = spec_for_shard(&manager, ShardId::new(1));
    let output_id = output.id.clone();

    let result = manager.dispatch_to_group(EgressCommand::Add(output.clone()), &group);
    shard_one.wait_for_commands(1);
    let snapshots = group.shutdown_and_join();

    assert_eq!(
        result,
        Ok(ManagerCommandOutcome::Enqueued {
            shard_id: ShardId::new(1)
        })
    );
    assert!(shard_zero.state().commands.is_empty());
    assert_eq!(shard_one.state().commands, vec![format!("add:{output_id}")]);
    assert_eq!(
        manager.desired_output(&output_id),
        Some(&DesiredOutput {
            id: output_id,
            generation: 1,
            shard_id: ShardId::new(1),
        })
    );
    assert!(snapshots.iter().all(|snapshot| snapshot.stopped));
}

#[test]
fn manager_dispatch_to_group_preserves_state_when_group_rejects_shard() {
    let mut manager = manager(2);
    let shard_zero = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(1).unwrap(),
        config(4, 4),
        vec![ProbeBackend {
            probe: shard_zero.clone(),
        }],
    )
    .unwrap();
    let output = spec_for_shard(&manager, ShardId::new(1));
    let output_id = output.id.clone();

    let result = manager.dispatch_to_group(EgressCommand::Add(output), &group);
    let snapshots = group.shutdown_and_join();

    assert_eq!(
        result,
        Err(EgressManagerDispatchError::Dispatch {
            shard_id: ShardId::new(1),
            source: EgressShardGroupError::UnknownShard {
                shard_id: ShardId::new(1)
            },
        })
    );
    assert!(manager.desired_output(&output_id).is_none());
    assert_eq!(manager.command_depth(ShardId::new(1)), 0);
    assert!(shard_zero.state().commands.is_empty());
    assert_eq!(snapshots.len(), 1);
}

#[test]
fn shard_group_rejects_mismatched_backend_count() {
    let result = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(4, 4),
        vec![ProbeBackend {
            probe: Probe::default(),
        }],
    );

    assert!(matches!(
        result,
        Err(EgressShardGroupError::BackendCountMismatch {
            expected: 2,
            actual: 1,
        })
    ));
}

#[test]
fn shard_group_reports_unknown_shard_without_touching_live_shards() {
    let probe = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(1).unwrap(),
        config(4, 4),
        vec![ProbeBackend {
            probe: probe.clone(),
        }],
    )
    .unwrap();

    assert_eq!(
        group.try_send_to(
            ShardId::new(7),
            EgressCommand::Add(output_spec("out-missing"))
        ),
        Err(EgressShardGroupError::UnknownShard {
            shard_id: ShardId::new(7)
        })
    );
    let snapshots = group.shutdown_and_join();

    assert!(probe.state().commands.is_empty());
    assert_eq!(snapshots.len(), 1);
}

#[test]
fn shard_group_contains_panic_to_assigned_shard() {
    let survivor = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(4, 4),
        vec![
            ScriptBackend::Panic,
            ScriptBackend::Probe(ProbeBackend {
                probe: survivor.clone(),
            }),
        ],
    )
    .unwrap();

    assert_eq!(
        group.try_send_to(
            ShardId::new(0),
            EgressCommand::Add(output_spec("out-panic"))
        ),
        Ok(())
    );
    assert_eq!(
        group.try_send_to(
            ShardId::new(1),
            EgressCommand::Add(output_spec("out-survivor"))
        ),
        Ok(())
    );
    survivor.wait_for_commands(1);
    survivor.wait_for_media_ticks(1);
    let snapshots = group.shutdown_and_join();

    let panicked = snapshots
        .iter()
        .find(|snapshot| snapshot.shard_id == ShardId::new(0))
        .unwrap();
    let healthy = snapshots
        .iter()
        .find(|snapshot| snapshot.shard_id == ShardId::new(1))
        .unwrap();
    assert!(panicked.panicked);
    assert!(panicked.stopped);
    assert!(!healthy.panicked);
    assert!(healthy.stopped);
    assert_eq!(survivor.state().commands, vec!["add:out-survivor"]);
}
