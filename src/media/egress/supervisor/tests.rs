use super::*;
use crate::media::egress::command::{EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::manager::{EgressManager, EgressManagerConfig};
use crate::media::egress::policy::LeafPolicy;
use crate::media::egress::shard::{EgressShardCommandEffect, EgressShardHealth};
use std::num::NonZeroU32;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
struct Probe {
    inner: Arc<(Mutex<Vec<String>>, Condvar)>,
}

impl Probe {
    fn wait_for_commands(&self, target: usize) {
        let (lock, condvar) = &*self.inner;
        let commands = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(commands, Duration::from_secs(2), |commands| {
                commands.len() < target
            })
            .unwrap();
        assert!(result.0.len() >= target);
    }

    fn commands(&self) -> Vec<String> {
        self.inner.0.lock().unwrap().clone()
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
enum TestBackend {
    Blocking(Gate),
    Panic,
    Probe(Probe),
}

impl EgressShardBackend for TestBackend {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        match self {
            Self::Blocking(gate) => {
                let (lock, condvar) = &*gate.inner;
                let mut state = lock.lock().unwrap();
                state.entered = true;
                condvar.notify_all();
                let _guard = condvar
                    .wait_timeout_while(state, Duration::from_secs(2), |state| !state.released)
                    .unwrap();
                EgressShardCommandEffect::Continue
            }
            Self::Panic => panic!("scripted shard panic"),
            Self::Probe(probe) => {
                let (lock, condvar) = &*probe.inner;
                let mut commands = lock.lock().unwrap();
                commands.push(command_label(&command));
                condvar.notify_all();
                EgressShardCommandEffect::Continue
            }
        }
    }
}

#[test]
fn supervisor_replaces_panicked_shard_and_replays_only_its_outputs() {
    let _expected_panic_silencer = crate::media::test_support::silence_expected_panics();
    let mut manager = EgressManager::new(EgressManagerConfig::new(2, 16).unwrap());
    let survivor = Probe::default();
    let replacement = Probe::default();
    let panicked_output = spec_for_shard(&manager, ShardId::new(0));
    let survivor_output = spec_for_shard(&manager, ShardId::new(1));
    let panicked_output_id = panicked_output.id.clone();
    let survivor_output_id = survivor_output.id.clone();
    let mut group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        shard_config(),
        vec![TestBackend::Panic, TestBackend::Probe(survivor.clone())],
    )
    .unwrap();

    assert!(matches!(
        manager.dispatch_to_group(EgressCommand::Add(panicked_output), &group),
        Ok(ManagerCommandOutcome::Enqueued { shard_id }) if shard_id == ShardId::new(0)
    ));
    assert!(matches!(
        manager.dispatch_to_group(EgressCommand::Add(survivor_output), &group),
        Ok(ManagerCommandOutcome::Enqueued { shard_id }) if shard_id == ShardId::new(1)
    ));
    survivor.wait_for_commands(1);
    wait_for_panicked(&group, ShardId::new(0));

    let recovery = supervisor().recover_panicked_shards(&mut manager, &mut group, |_| {
        TestBackend::Probe(replacement.clone())
    });

    assert_eq!(
        recovery,
        Ok(EgressSupervisorRecovery {
            recoveries: vec![EgressShardRecovery::Replayed {
                shard_id: ShardId::new(0),
                output_count: 1,
            }],
        })
    );
    replacement.wait_for_commands(1);
    let snapshots = group.shutdown_and_join();

    assert_eq!(
        replacement.commands(),
        vec![format!("add:{panicked_output_id}"), "shutdown".to_string()]
    );
    assert_eq!(
        survivor.commands(),
        vec![format!("add:{survivor_output_id}"), "shutdown".to_string()]
    );
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.stopped && !snapshot.panicked)
    );
}

#[test]
fn supervisor_observes_stalled_shard_without_replacing_it() {
    let mut manager = EgressManager::new(EgressManagerConfig::new(1, 16).unwrap());
    let gate = Gate::default();
    let replacement = Probe::default();
    let output = output_spec("out-stalled");
    let mut group = EgressShardGroup::spawn(
        NonZeroU32::new(1).unwrap(),
        shard_config(),
        vec![TestBackend::Blocking(gate.clone())],
    )
    .unwrap();

    assert!(matches!(
        manager.dispatch_to_group(EgressCommand::Add(output), &group),
        Ok(ManagerCommandOutcome::Enqueued {
            shard_id: ShardId { .. }
        })
    ));
    gate.wait_until_entered();
    let heartbeats = supervisor().observe_shards(&group, Instant::now());
    let recovery = supervisor().recover_panicked_shards(&mut manager, &mut group, |_| {
        TestBackend::Probe(replacement.clone())
    });
    gate.release();
    let snapshots = group.shutdown_and_join();

    assert_eq!(heartbeats.len(), 1);
    assert_eq!(heartbeats[0].state, EgressShardHealth::Stalled);
    assert_eq!(
        recovery,
        Ok(EgressSupervisorRecovery {
            recoveries: Vec::new(),
        })
    );
    assert!(replacement.commands().is_empty());
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.stopped && !snapshot.panicked)
    );
}

fn supervisor() -> EgressSupervisor {
    EgressSupervisor::new(EgressSupervisorConfig::new(shard_config(), Duration::ZERO))
}

fn shard_config() -> EgressShardConfig {
    EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(10)).unwrap()
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
        progress: Default::default(),
    }
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

fn wait_for_panicked(group: &EgressShardGroup, shard_id: ShardId) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if group
            .snapshots()
            .iter()
            .any(|snapshot| snapshot.shard_id == shard_id && snapshot.panicked)
        {
            return;
        }
        std::thread::yield_now();
    }
    panic!("timed out waiting for {shard_id:?} to report panic");
}

fn command_label(command: &EgressCommand) -> String {
    match command {
        EgressCommand::Add(spec) => format!("add:{}", spec.id.as_str()),
        EgressCommand::Update(spec) => format!("update:{}", spec.id.as_str()),
        EgressCommand::Remove(id) => format!("remove:{}", id.as_str()),
        EgressCommand::FeedWake => "feed-wake".to_string(),
        EgressCommand::DrainShard(shard_id) => format!("drain:{}", shard_id.index()),
        EgressCommand::Shutdown => "shutdown".to_string(),
    }
}
