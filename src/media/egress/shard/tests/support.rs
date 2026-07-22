use super::super::*;
use crate::media::egress::command::{
    EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec, ShardId,
};
use crate::media::egress::manager::{EgressManager, EgressManagerConfig};
use crate::media::egress::metrics::ShardMetrics;
use crate::media::egress::policy::LeafPolicy;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub(super) struct ProbeState {
    pub(super) commands: Vec<String>,
    pub(super) timers: Vec<String>,
    pub(super) ready_events: u64,
    pub(super) media_ticks: u64,
    pub(super) shutdowns: u64,
    generations: HashMap<String, u64>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct Probe {
    inner: Arc<(Mutex<ProbeState>, Condvar)>,
}

impl Probe {
    pub(super) fn wait_for_media_ticks(&self, target: u64) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.media_ticks < target
            })
            .unwrap();
        assert!(result.0.media_ticks >= target);
    }

    pub(super) fn wait_for_commands(&self, target: usize) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.commands.len() < target
            })
            .unwrap();
        assert!(result.0.commands.len() >= target);
    }

    pub(super) fn wait_for_timers(&self, target: usize) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.timers.len() < target
            })
            .unwrap();
        assert!(result.0.timers.len() >= target);
    }

    pub(super) fn wait_for_ready_events(&self, target: u64) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.ready_events < target
            })
            .unwrap();
        assert!(result.0.ready_events >= target);
    }

    pub(super) fn state(&self) -> ProbeState {
        let state = self.inner.0.lock().unwrap();
        ProbeState {
            commands: state.commands.clone(),
            timers: state.timers.clone(),
            ready_events: state.ready_events,
            media_ticks: state.media_ticks,
            shutdowns: state.shutdowns,
            generations: state.generations.clone(),
        }
    }
}

#[derive(Debug)]
pub(super) struct ProbeBackend {
    pub(super) probe: Probe,
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

    fn on_ready(&mut self) -> EgressShardCommandEffect {
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.ready_events = state.ready_events.saturating_add(1);
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
pub(super) enum ScriptBackend {
    Blocking(BlockingBackend),
    Probe(ProbeBackend),
    Panic,
}

impl EgressShardBackend for ScriptBackend {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        match self {
            Self::Blocking(backend) => backend.on_command(command),
            Self::Probe(backend) => backend.on_command(command),
            Self::Panic => panic!("scripted shard panic"),
        }
    }

    fn on_media_tick(&mut self) {
        match self {
            Self::Blocking(_) => {}
            Self::Probe(backend) => backend.on_media_tick(),
            Self::Panic => {}
        }
    }

    fn timer_generation(&self, output_id: &OutputId) -> Option<u64> {
        match self {
            Self::Blocking(_) => None,
            Self::Probe(backend) => backend.timer_generation(output_id),
            Self::Panic => None,
        }
    }

    fn on_timer(&mut self, output_id: OutputId, generation: u64) -> EgressShardCommandEffect {
        match self {
            Self::Blocking(_) => EgressShardCommandEffect::Continue,
            Self::Probe(backend) => backend.on_timer(output_id, generation),
            Self::Panic => EgressShardCommandEffect::Continue,
        }
    }

    fn on_shutdown(&mut self) {
        match self {
            Self::Blocking(backend) => backend.on_shutdown(),
            Self::Probe(backend) => backend.on_shutdown(),
            Self::Panic => {}
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct Gate {
    inner: Arc<(Mutex<GateState>, Condvar)>,
}

#[derive(Debug, Default)]
struct GateState {
    entered: bool,
    released: bool,
}

impl Gate {
    pub(super) fn wait_until_entered(&self) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| !state.entered)
            .unwrap();
        assert!(result.0.entered);
    }

    pub(super) fn release(&self) {
        let (lock, condvar) = &*self.inner;
        let mut state = lock.lock().unwrap();
        state.released = true;
        condvar.notify_all();
    }
}

#[derive(Debug)]
pub(super) struct BlockingBackend {
    pub(super) gate: Gate,
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
pub(super) struct TimerBackend {
    pub(super) probe: Probe,
    pub(super) delay: Duration,
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

#[derive(Debug)]
pub(super) struct ReadyFloodBackend {
    pub(super) probe: Probe,
}

impl EgressShardBackend for ReadyFloodBackend {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        let label = command_label(&command);
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.commands.push(label);
        condvar.notify_all();
        EgressShardCommandEffect::ScheduleReady { count: 8 }
    }

    fn on_ready(&mut self) -> EgressShardCommandEffect {
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.ready_events = state.ready_events.saturating_add(1);
        condvar.notify_all();
        EgressShardCommandEffect::ScheduleReady { count: 1 }
    }

    fn on_shutdown(&mut self) {
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.shutdowns += 1;
        condvar.notify_all();
    }
}

pub(super) fn config(capacity: usize, command_budget: usize) -> EgressShardConfig {
    EgressShardConfig::new(
        capacity,
        command_budget,
        command_budget,
        command_budget,
        Duration::from_millis(10),
    )
    .unwrap()
}

pub(super) fn output_spec(id: &str) -> OutputSpec {
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

pub(super) fn snapshot(shard_id: ShardId) -> EgressShardSnapshot {
    EgressShardSnapshot {
        shard_id,
        loop_iterations: 0,
        commands_processed: 0,
        timers_processed: 0,
        pending_timers: 0,
        media_ticks: 0,
        last_progress_at: None,
        stopped: false,
        panicked: false,
        metrics: ShardMetrics::new(shard_id),
    }
}

pub(super) fn manager(shards: u32) -> EgressManager {
    EgressManager::new(EgressManagerConfig::new(shards, 16).unwrap())
}

pub(super) fn spec_for_shard(manager: &EgressManager, target: ShardId) -> OutputSpec {
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
