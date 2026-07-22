use std::num::NonZeroUsize;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::media::egress::command::{EgressCommand, ShardId};

mod group;
pub use group::{EgressShardGroup, EgressShardGroupError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressShardConfigError {
    ZeroCommandCapacity,
    ZeroCommandBatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressShardConfig {
    command_channel_capacity: NonZeroUsize,
    command_batch_budget: NonZeroUsize,
    idle_wait: Duration,
}

impl EgressShardConfig {
    pub fn new(
        command_channel_capacity: usize,
        command_batch_budget: usize,
        idle_wait: Duration,
    ) -> Result<Self, EgressShardConfigError> {
        let command_channel_capacity = NonZeroUsize::new(command_channel_capacity)
            .ok_or(EgressShardConfigError::ZeroCommandCapacity)?;
        let command_batch_budget = NonZeroUsize::new(command_batch_budget)
            .ok_or(EgressShardConfigError::ZeroCommandBatch)?;
        Ok(Self {
            command_channel_capacity,
            command_batch_budget,
            idle_wait,
        })
    }

    pub fn command_channel_capacity(self) -> NonZeroUsize {
        self.command_channel_capacity
    }

    pub fn command_batch_budget(self) -> NonZeroUsize {
        self.command_batch_budget
    }

    pub fn idle_wait(self) -> Duration {
        self.idle_wait
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressShardSnapshot {
    pub shard_id: ShardId,
    pub loop_iterations: u64,
    pub commands_processed: u64,
    pub media_ticks: u64,
    pub last_progress_at: Option<Instant>,
    pub stopped: bool,
    pub panicked: bool,
}

impl EgressShardSnapshot {
    fn new(shard_id: ShardId) -> Self {
        Self {
            shard_id,
            loop_iterations: 0,
            commands_processed: 0,
            media_ticks: 0,
            last_progress_at: Some(Instant::now()),
            stopped: false,
            panicked: false,
        }
    }
}

pub trait EgressShardBackend: Send + 'static {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect;

    fn on_media_tick(&mut self) {}

    fn on_shutdown(&mut self) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressShardCommandEffect {
    Continue,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressShardSendError {
    Full,
    Closed,
}

#[derive(Debug)]
pub struct EgressShardHandle {
    shard_id: ShardId,
    sender: SyncSender<EgressCommand>,
    snapshot: Arc<Mutex<EgressShardSnapshot>>,
    join: Option<JoinHandle<()>>,
}

impl EgressShardHandle {
    pub fn spawn<B: EgressShardBackend>(
        shard_id: ShardId,
        config: EgressShardConfig,
        backend: B,
    ) -> Self {
        let (sender, receiver) = mpsc::sync_channel(config.command_channel_capacity.get());
        let snapshot = Arc::new(Mutex::new(EgressShardSnapshot::new(shard_id)));
        let thread_snapshot = Arc::clone(&snapshot);
        let join = thread::Builder::new()
            .name(format!("egress-{shard_id}"))
            .spawn(move || run_shard_thread(shard_id, config, receiver, backend, thread_snapshot))
            .expect("spawn egress shard thread");
        Self {
            shard_id,
            sender,
            snapshot,
            join: Some(join),
        }
    }

    pub fn shard_id(&self) -> ShardId {
        self.shard_id
    }

    pub fn try_send(&self, command: EgressCommand) -> Result<(), EgressShardSendError> {
        self.sender.try_send(command).map_err(|err| match err {
            TrySendError::Full(_) => EgressShardSendError::Full,
            TrySendError::Disconnected(_) => EgressShardSendError::Closed,
        })
    }

    pub fn snapshot(&self) -> EgressShardSnapshot {
        self.snapshot.lock().unwrap().clone()
    }

    pub fn shutdown_and_join(mut self) -> EgressShardSnapshot {
        let _ = self.sender.send(EgressCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        self.snapshot()
    }
}

fn run_shard_thread<B: EgressShardBackend>(
    shard_id: ShardId,
    config: EgressShardConfig,
    receiver: Receiver<EgressCommand>,
    mut backend: B,
    snapshot: Arc<Mutex<EgressShardSnapshot>>,
) {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let mut runtime = EgressShardRuntime {
            shard_id,
            config,
            receiver,
            backend: &mut backend,
            snapshot: Arc::clone(&snapshot),
        };
        runtime.run();
    }));
    let mut snapshot = snapshot.lock().unwrap();
    if result.is_err() {
        snapshot.panicked = true;
    }
    snapshot.stopped = true;
}

struct EgressShardRuntime<'a, B: EgressShardBackend> {
    shard_id: ShardId,
    config: EgressShardConfig,
    receiver: Receiver<EgressCommand>,
    backend: &'a mut B,
    snapshot: Arc<Mutex<EgressShardSnapshot>>,
}

impl<B: EgressShardBackend> EgressShardRuntime<'_, B> {
    fn run(&mut self) {
        let mut running = true;
        while running {
            let mut processed = self.process_command_batch(&mut running);
            if running && processed == 0 {
                processed = self.wait_for_command(&mut running);
            }
            self.backend.on_media_tick();
            self.record_iteration(processed);
        }
        self.backend.on_shutdown();
    }

    fn process_command_batch(&mut self, running: &mut bool) -> usize {
        let mut processed = 0;
        while processed < self.config.command_batch_budget.get() {
            let Ok(command) = self.receiver.try_recv() else {
                break;
            };
            processed += 1;
            if self.process_command(command).is_stop() {
                *running = false;
                break;
            }
        }
        processed
    }

    fn wait_for_command(&mut self, running: &mut bool) -> usize {
        match self.receiver.recv_timeout(self.config.idle_wait) {
            Ok(command) => {
                if self.process_command(command).is_stop() {
                    *running = false;
                }
                1
            }
            Err(mpsc::RecvTimeoutError::Timeout) => 0,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                *running = false;
                0
            }
        }
    }

    fn process_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        match command {
            EgressCommand::DrainShard(target) if target != self.shard_id => {
                EgressShardCommandEffect::Continue
            }
            EgressCommand::Shutdown => EgressShardCommandEffect::Stop,
            command => self.backend.on_command(command),
        }
    }

    fn record_iteration(&self, commands_processed: usize) {
        let mut snapshot = self.snapshot.lock().unwrap();
        snapshot.loop_iterations = snapshot.loop_iterations.saturating_add(1);
        snapshot.commands_processed = snapshot
            .commands_processed
            .saturating_add(commands_processed as u64);
        snapshot.media_ticks = snapshot.media_ticks.saturating_add(1);
        snapshot.last_progress_at = Some(Instant::now());
    }
}

impl EgressShardCommandEffect {
    fn is_stop(self) -> bool {
        matches!(self, Self::Stop)
    }
}

#[cfg(test)]
mod tests;
