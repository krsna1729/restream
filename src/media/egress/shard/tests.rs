use super::*;
use crate::media::egress::command::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::policy::LeafPolicy;
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
