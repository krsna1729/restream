use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::media::egress::command::{EgressCommand, OutputId, ShardId};
use crate::media::egress::journal::WakeGate;
use crate::media::egress::metrics::ShardMetrics;
use crate::media::egress::timer::TimerWheel;

mod group;
pub use group::{EgressShardGroup, EgressShardGroupError, EgressShardHealth, EgressShardHeartbeat};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressShardConfigError {
    ZeroCommandCapacity,
    ZeroCommandBatch,
    ZeroReadyBatch,
    ZeroTimerBatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressShardConfig {
    command_channel_capacity: NonZeroUsize,
    command_batch_budget: NonZeroUsize,
    readiness_batch_budget: NonZeroUsize,
    timer_batch_budget: NonZeroUsize,
    idle_wait: Duration,
    drain_timeout: Duration,
}

impl EgressShardConfig {
    /// Bound on how long `Shutdown` keeps the shard loop alive draining
    /// leaves before forcing a close, when no explicit
    /// [`Self::with_drain_timeout`] override is given.
    pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

    pub fn new(
        command_channel_capacity: usize,
        command_batch_budget: usize,
        readiness_batch_budget: usize,
        timer_batch_budget: usize,
        idle_wait: Duration,
    ) -> Result<Self, EgressShardConfigError> {
        let command_channel_capacity = NonZeroUsize::new(command_channel_capacity)
            .ok_or(EgressShardConfigError::ZeroCommandCapacity)?;
        let command_batch_budget = NonZeroUsize::new(command_batch_budget)
            .ok_or(EgressShardConfigError::ZeroCommandBatch)?;
        let readiness_batch_budget = NonZeroUsize::new(readiness_batch_budget)
            .ok_or(EgressShardConfigError::ZeroReadyBatch)?;
        let timer_batch_budget =
            NonZeroUsize::new(timer_batch_budget).ok_or(EgressShardConfigError::ZeroTimerBatch)?;
        Ok(Self {
            command_channel_capacity,
            command_batch_budget,
            readiness_batch_budget,
            timer_batch_budget,
            idle_wait,
            drain_timeout: Self::DEFAULT_DRAIN_TIMEOUT,
        })
    }

    /// Override the drain-on-shutdown deadline. Tests use this for fast,
    /// deterministic timing; production leaves the constructor default.
    pub fn with_drain_timeout(mut self, drain_timeout: Duration) -> Self {
        self.drain_timeout = drain_timeout;
        self
    }

    pub fn command_channel_capacity(self) -> NonZeroUsize {
        self.command_channel_capacity
    }

    pub fn command_batch_budget(self) -> NonZeroUsize {
        self.command_batch_budget
    }

    pub fn timer_batch_budget(self) -> NonZeroUsize {
        self.timer_batch_budget
    }

    pub fn readiness_batch_budget(self) -> NonZeroUsize {
        self.readiness_batch_budget
    }

    pub fn idle_wait(self) -> Duration {
        self.idle_wait
    }

    pub fn drain_timeout(self) -> Duration {
        self.drain_timeout
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EgressShardSnapshot {
    pub shard_id: ShardId,
    pub loop_iterations: u64,
    pub commands_processed: u64,
    pub timers_processed: u64,
    pub pending_timers: usize,
    pub media_ticks: u64,
    pub last_progress_at: Option<Instant>,
    pub stopped: bool,
    pub panicked: bool,
    pub metrics: ShardMetrics,
}

impl EgressShardSnapshot {
    fn new(shard_id: ShardId) -> Self {
        Self {
            shard_id,
            loop_iterations: 0,
            commands_processed: 0,
            timers_processed: 0,
            pending_timers: 0,
            media_ticks: 0,
            last_progress_at: Some(Instant::now()),
            stopped: false,
            panicked: false,
            metrics: ShardMetrics::new(shard_id),
        }
    }
}

pub trait EgressShardBackend: Send + 'static {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect;

    fn next_wakeup(&self) -> Option<Instant> {
        None
    }

    fn on_wakeup(&mut self) -> EgressShardCommandEffect {
        EgressShardCommandEffect::Continue
    }

    fn timer_generation(&self, _output_id: &OutputId) -> Option<u64> {
        None
    }

    fn on_timer(&mut self, _output_id: OutputId, _generation: u64) -> EgressShardCommandEffect {
        EgressShardCommandEffect::Continue
    }

    fn on_ready(&mut self) -> EgressShardCommandEffect {
        EgressShardCommandEffect::Continue
    }

    /// A leaf whose async connect completes here (see `RtmpShardBackend`/
    /// `SrtShardBackend`) has no independent way to discover its own fresh
    /// I/O readiness: `on_ready`/`poll_ready` (the only path that actually
    /// polls the native readiness backend) only ever runs when something
    /// schedules ready work via the returned effect — nothing does that by
    /// default just because a connect resolved. Without a backend
    /// returning `ScheduleReady` here when it completes a connect, that
    /// leaf sits registered but unvisited until an unrelated `FeedWake`
    /// happens to arrive, which on a source with any real gap before its
    /// first published unit (a cold-starting transcoder, a file ingest)
    /// can be many seconds away even though the connection itself was
    /// ready from the moment it completed.
    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        EgressShardCommandEffect::Continue
    }

    fn on_shutdown(&mut self) {}

    /// Total `EngineProgress::FeedOverrun` resynchronizations this backend
    /// has observed across every leaf it has ever visited. Read once per
    /// loop iteration into `ShardMetrics::feed_resyncs` for the
    /// repeated-resync alert; backends with no per-leaf feed cursor (e.g.
    /// `PipelineShardBackend`, `SinkShardBackend`) keep the default of 0.
    fn resync_count(&self) -> u64 {
        0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressShardCommandEffect {
    Continue,
    ScheduleTimer {
        output_id: OutputId,
        generation: u64,
        fire_at: Instant,
    },
    ScheduleReady {
        count: usize,
    },
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressShardSendError {
    Full,
    Closed,
}

/// Publisher-side handle that delivers coalesced feed wakes to one shard.
#[derive(Debug, Clone)]
pub struct FeedWakeHandle {
    gate: Arc<WakeGate>,
    sender: SyncSender<EgressCommand>,
}

impl FeedWakeHandle {
    /// Send [`EgressCommand::FeedWake`] only on the gate's clear-to-set
    /// transition, so at most one wake is in flight per shard.
    pub fn deliver(&self) -> Result<(), EgressShardSendError> {
        if self.gate.notify() {
            return self
                .sender
                .try_send(EgressCommand::FeedWake)
                .map_err(|err| match err {
                    TrySendError::Full(_) => EgressShardSendError::Full,
                    TrySendError::Disconnected(_) => EgressShardSendError::Closed,
                });
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct EgressShardHandle {
    shard_id: ShardId,
    sender: SyncSender<EgressCommand>,
    snapshot: Arc<Mutex<EgressShardSnapshot>>,
    wake_gate: Arc<WakeGate>,
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
        let wake_gate = Arc::new(WakeGate::new());
        let thread_wake_gate = Arc::clone(&wake_gate);
        let join = thread::Builder::new()
            .name(format!("egress-{shard_id}"))
            .spawn(move || {
                run_shard_thread(
                    shard_id,
                    config,
                    receiver,
                    backend,
                    thread_snapshot,
                    thread_wake_gate,
                )
            })
            .expect("spawn egress shard thread");
        Self {
            shard_id,
            sender,
            snapshot,
            wake_gate,
            join: Some(join),
        }
    }

    /// The coalescing wake gate for this shard.  A publisher-side watcher
    /// calls `notify()` and sends [`EgressCommand::FeedWake`] only on the
    /// clear-to-set transition; the shard clears the gate before draining.
    pub fn wake_gate(&self) -> Arc<WakeGate> {
        Arc::clone(&self.wake_gate)
    }

    /// Deliver a coalesced feed wake: send the command only when the gate
    /// transitioned from clear to set, so at most one wake is in flight.
    pub fn deliver_feed_wake(&self) -> Result<(), EgressShardSendError> {
        self.feed_wake_handle().deliver()
    }

    /// A cloneable delivery handle for publisher-side feed watchers.
    pub fn feed_wake_handle(&self) -> FeedWakeHandle {
        FeedWakeHandle {
            gate: Arc::clone(&self.wake_gate),
            sender: self.sender.clone(),
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

    /// Send `Shutdown` but don't block on the shard thread's graceful
    /// drain window (up to `EgressShardConfig::drain_timeout`) -- used by
    /// `EgressShardGroup::shrink` (`EgressFabricRuntime::rescale`'s
    /// scale-in path), which runs under the caller's shared per-protocol
    /// registry lock. Blocking there would stall every other feed's
    /// dispatch on that lock for however long this one shard takes to
    /// drain. Dropping a `JoinHandle` without joining it is safe and
    /// well-defined: the OS thread keeps running independently and
    /// reclaims its own resources on exit; nothing here needs the join to
    /// observe a panic either, since that's recorded in the shared
    /// snapshot `Arc` the thread writes before it exits, not in the
    /// `JoinHandle`'s `Result`. The final full-runtime shutdown path
    /// (`EgressShardGroup::shutdown_and_join`) still joins inline, since
    /// that IS the definitive "fully stopped" signal its callers wait for.
    pub fn shutdown_detached(mut self) {
        let _ = self.sender.send(EgressCommand::Shutdown);
        self.join.take();
    }
}

fn run_shard_thread<B: EgressShardBackend>(
    shard_id: ShardId,
    config: EgressShardConfig,
    receiver: Receiver<EgressCommand>,
    mut backend: B,
    snapshot: Arc<Mutex<EgressShardSnapshot>>,
    wake_gate: Arc<WakeGate>,
) {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let mut runtime = EgressShardRuntime {
            shard_id,
            config,
            receiver,
            backend: &mut backend,
            ready_backlog: VecDeque::new(),
            timers: TimerWheel::new(),
            metrics: ShardMetrics::new(shard_id),
            snapshot: Arc::clone(&snapshot),
            wake_gate,
            draining_until: None,
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
    ready_backlog: VecDeque<()>,
    timers: TimerWheel<OutputId>,
    metrics: ShardMetrics,
    snapshot: Arc<Mutex<EgressShardSnapshot>>,
    wake_gate: Arc<WakeGate>,
    /// Set once `Shutdown` is received; the loop keeps running (so leaves
    /// with queued application bytes get a chance to flush and close
    /// themselves gracefully — see backend-level draining in
    /// e.g. `RtmpShardBackend`) until either everything goes idle or this
    /// deadline passes, whichever comes first. `None` means "not shutting
    /// down yet."
    draining_until: Option<Instant>,
}

impl<B: EgressShardBackend> EgressShardRuntime<'_, B> {
    fn run(&mut self) {
        let mut running = true;
        while running {
            let loop_started_at = Instant::now();
            let mut processed = self.process_command_batch(&mut running);
            let mut ready_processed = self.process_ready_batch(&mut running);
            let mut timers_processed = self.process_timer_batch(&mut running);
            if running && processed == 0 && ready_processed == 0 && timers_processed == 0 {
                processed = self.wait_for_command(&mut running);
                if running && processed == 0 {
                    let effect = self.backend.on_wakeup();
                    self.apply_effect(effect);
                }
                if running {
                    ready_processed += self.process_ready_batch(&mut running);
                }
                if running {
                    timers_processed = self.process_timer_batch(&mut running);
                }
                let mut idle_poll_found_work = false;
                if running && processed == 0 && ready_processed == 0 && timers_processed == 0 {
                    // Nothing scheduled a readiness check this iteration (no
                    // `FeedWake`, no self-perpetuating `ScheduleReady` from a
                    // previous visit). A leaf waiting on real I/O readiness
                    // that isn't feed-related — most commonly a handshake or
                    // negotiation response — has no other way to ever be
                    // rediscovered on an otherwise-quiet shard: `on_ready`'s
                    // `poll_ready()` (the only thing that actually calls the
                    // native poller) never runs unless something schedules
                    // it, and a shard with one output and no active media
                    // yet schedules nothing. This closes that gap by giving
                    // every idle shard one real poller check per idle-wait
                    // cycle, matching the cadence `RtmpShardBackend`'s
                    // `FeedWake` handling doc comments already assume exists
                    // ("the shard's own idle poll cycle").
                    let effect = self.backend.on_ready();
                    idle_poll_found_work = effect != EgressShardCommandEffect::Continue;
                    if self.apply_effect(effect).stops_shard() {
                        running = false;
                    }
                }
                // Fully idle while draining: nothing left to flush, so stop
                // now instead of waiting out the rest of the deadline.
                if running
                    && self.draining_until.is_some()
                    && processed == 0
                    && ready_processed == 0
                    && timers_processed == 0
                    && !idle_poll_found_work
                {
                    running = false;
                }
            }
            // Clear the wake gate before draining so a publish landing after
            // this point re-arms exactly one wake delivery (loom-proven seam).
            self.wake_gate.take();
            let media_tick_effect = self.backend.on_media_tick();
            self.apply_effect(media_tick_effect);
            self.record_iteration(loop_started_at, processed, timers_processed);
            if running
                && let Some(deadline) = self.draining_until
                && Instant::now() >= deadline
            {
                running = false;
            }
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
            let effect = self.process_command(command);
            if self.apply_effect(effect).stops_shard() {
                *running = false;
                break;
            }
        }
        processed
    }

    fn wait_for_command(&mut self, running: &mut bool) -> usize {
        let timeout = self
            .backend
            .next_wakeup()
            .map(|deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(self.config.idle_wait)
            })
            .unwrap_or(self.config.idle_wait);
        match self.receiver.recv_timeout(timeout) {
            Ok(command) => {
                let effect = self.process_command(command);
                if self.apply_effect(effect).stops_shard() {
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
            // A wake both ends the sleep and schedules ready work: backends
            // drain feeds from ready visits, so the wake must pump the
            // poll-and-visit chain, not just the media tick. It must also
            // still reach the backend's own `on_command`: a backend whose
            // leaves can register for no I/O interest at all once their
            // feed is fully drained (RTMP, unlike SRT's always-write
            // registration) needs the wake itself to re-enqueue those
            // leaves, since a bare `poll_leaves()` re-poll cannot discover
            // readiness a leaf never registered for. SRT's backend treats
            // `FeedWake` as a no-op today, so this is free for it.
            EgressCommand::FeedWake => {
                self.backend.on_command(EgressCommand::FeedWake);
                EgressShardCommandEffect::ScheduleReady { count: 1 }
            }
            EgressCommand::DrainShard(target) if target != self.shard_id => {
                EgressShardCommandEffect::Continue
            }
            EgressCommand::Shutdown => {
                // Forward to the backend, once, so it can mark every leaf
                // for a graceful close (flush pending bytes, then close)
                // instead of the shard thread stopping immediately and
                // truncating whatever each leaf still had queued. A caller
                // may send `Shutdown` more than once (e.g. a broadcast
                // followed by `EgressShardGroup::shutdown_and_join`'s own
                // send) — only the first actually starts the drain window
                // and applies the backend's returned effect (e.g. a
                // `ScheduleReady` to kick off draining visits promptly);
                // later sends are no-ops.
                if self.draining_until.is_none() {
                    self.draining_until = Some(Instant::now() + self.config.drain_timeout());
                    self.backend.on_command(EgressCommand::Shutdown)
                } else {
                    EgressShardCommandEffect::Continue
                }
            }
            command => self.backend.on_command(command),
        }
    }

    fn process_ready_batch(&mut self, running: &mut bool) -> usize {
        let mut processed = 0;
        while processed < self.config.readiness_batch_budget.get() {
            if self.ready_backlog.pop_front().is_none() {
                break;
            }
            processed += 1;
            let effect = self.backend.on_ready();
            if self.apply_effect(effect).stops_shard() {
                *running = false;
                break;
            }
        }
        processed
    }

    fn process_timer_batch(&mut self, running: &mut bool) -> usize {
        let now = Instant::now();
        let expired = self.timers.drain_expired_limited(
            now,
            self.config.timer_batch_budget.get(),
            |output_id| self.backend.timer_generation(output_id),
        );
        let mut processed = 0;
        for (output_id, generation) in expired {
            processed += 1;
            let effect = self.backend.on_timer(output_id, generation);
            if self.apply_effect(effect).stops_shard() {
                *running = false;
                break;
            }
        }
        processed
    }

    fn apply_effect(&mut self, effect: EgressShardCommandEffect) -> EgressShardCommandEffect {
        match effect {
            EgressShardCommandEffect::ScheduleTimer {
                output_id,
                generation,
                fire_at,
            } => {
                self.timers.insert(fire_at, output_id, generation);
                EgressShardCommandEffect::Continue
            }
            EgressShardCommandEffect::ScheduleReady { count } => {
                self.ready_backlog.extend(std::iter::repeat_n((), count));
                EgressShardCommandEffect::Continue
            }
            effect => effect,
        }
    }

    fn record_iteration(
        &mut self,
        loop_started_at: Instant,
        commands_processed: usize,
        timers_processed: usize,
    ) {
        self.metrics
            .record_loop_iteration(loop_started_at.elapsed());
        self.metrics.commands_processed = self
            .metrics
            .commands_processed
            .saturating_add(commands_processed as u64);
        self.metrics.timers_processed = self
            .metrics
            .timers_processed
            .saturating_add(timers_processed as u64);
        self.metrics.pending_timers = u32::try_from(self.timers.len()).unwrap_or(u32::MAX);
        self.metrics
            .observe_ready_depth(u32::try_from(self.ready_backlog.len()).unwrap_or(u32::MAX));
        self.metrics.media_ticks = self.metrics.media_ticks.saturating_add(1);
        self.metrics.feed_resyncs = self.backend.resync_count();
        self.metrics.collected_at = Some(Instant::now());

        let mut snapshot = self.snapshot.lock().unwrap();
        snapshot.loop_iterations = self.metrics.loop_iterations;
        snapshot.commands_processed = self.metrics.commands_processed;
        snapshot.timers_processed = self.metrics.timers_processed;
        snapshot.pending_timers = self.timers.len();
        snapshot.media_ticks = self.metrics.media_ticks;
        snapshot.last_progress_at = self.metrics.collected_at;
        snapshot.metrics = self.metrics.clone();
    }
}

impl EgressShardCommandEffect {
    fn stops_shard(&self) -> bool {
        matches!(self, Self::Stop)
    }
}

#[cfg(test)]
mod tests;
