use super::*;
use crate::media::egress::command::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::policy::LeafPolicy;
use std::num::NonZeroU32;
use std::sync::Condvar;

#[derive(Debug, Default)]
struct ProbeState {
    commands: Vec<String>,
    media_ticks: u64,
    shutdowns: u64,
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

    fn state(&self) -> ProbeState {
        let state = self.inner.0.lock().unwrap();
        ProbeState {
            commands: state.commands.clone(),
            media_ticks: state.media_ticks,
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
        let label = command_label(&command);
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.commands.push(label);
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

fn config(capacity: usize, budget: usize) -> EgressShardConfig {
    EgressShardConfig::new(capacity, budget, Duration::from_millis(10)).unwrap()
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
        EgressShardConfig::new(0, 1, Duration::ZERO),
        Err(EgressShardConfigError::ZeroCommandCapacity)
    );
    assert_eq!(
        EgressShardConfig::new(1, 0, Duration::ZERO),
        Err(EgressShardConfigError::ZeroCommandBatch)
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
