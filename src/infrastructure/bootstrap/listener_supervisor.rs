//! Ingest listeners are restarted, not fatal.
//!
//! A listener task (RTMP, SRT or MoQ) that ends without a shutdown request is
//! respawned after a backoff instead of shutting the process down: one
//! failed listener must not stop egress, the API, or the other protocol.
//! Per-entity panics are contained inside the listener (`panic_boundary`);
//! this layer covers whole-listener failures (accept errors, an Owner
//! fault, a MoQ listener thread that panicked or whose start failed) so they
//! are reported and recovered, not escalated.

use restream_engine::engine::{IngestListener, MediaEngine};
use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{error, info};

use crate::infrastructure::restart_backoff::RestartBackoff;

pub(super) struct SupervisedListener {
    name: &'static str,
    spawn: Box<dyn Fn() -> JoinHandle<()> + Send>,
    task: Option<JoinHandle<()>>,
    started_at: Instant,
    restart_at: Option<Instant>,
    backoff: RestartBackoff,
    restart_counter: (Arc<MediaEngine>, IngestListener),
}

impl SupervisedListener {
    /// Wraps a task that `spawn` already started once.
    pub(super) fn new(
        name: &'static str,
        task: JoinHandle<()>,
        spawn: Box<dyn Fn() -> JoinHandle<()> + Send>,
        restart_counter: (Arc<MediaEngine>, IngestListener),
    ) -> Self {
        Self {
            name,
            spawn,
            task: Some(task),
            started_at: Instant::now(),
            restart_at: None,
            backoff: RestartBackoff::new(),
            restart_counter,
        }
    }

    /// Resolves when the running task ends; pending while it is down.
    pub(super) async fn exited(&mut self) {
        let Some(task) = self.task.as_mut() else {
            return std::future::pending().await;
        };
        let result = task.await;
        self.task = None;
        let ran = self.started_at.elapsed();
        let delay = self.backoff.on_exit(ran);
        self.restart_at = Some(Instant::now() + delay);
        error!(
            listener = self.name,
            result = ?result,
            ran_ms = u64::try_from(ran.as_millis()).unwrap_or(u64::MAX),
            restart_in_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            "ingest listener exited without a shutdown request; restarting it"
        );
    }

    /// When a down listener is due to restart.
    pub(super) fn restart_due(&self) -> Option<Instant> {
        self.restart_at
    }

    /// Respawn the listener if it is down and its delay has passed.
    pub(super) fn restart_if_due(&mut self, now: Instant) {
        if self.task.is_none() && self.restart_at.is_some_and(|at| now >= at) {
            self.task = Some((self.spawn)());
            self.started_at = now;
            self.restart_at = None;
            let (engine, listener) = &self.restart_counter;
            engine.record_listener_restart(*listener);
            info!(listener = self.name, "ingest listener restarted");
        }
    }

    /// The running task's handle for shutdown; an ended task's stand-in
    /// while the listener is down.
    pub(super) fn take_handle(&mut self) -> JoinHandle<()> {
        self.task.take().unwrap_or_else(|| tokio::spawn(async {}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// A listener task that ends is respawned after its delay, counted, and
    /// the supervisor never resolves `exited` while it is down.
    #[tokio::test(start_paused = true)]
    async fn an_exited_listener_is_respawned_after_its_delay() {
        let spawned = Arc::new(AtomicU64::new(0));
        let engine = Arc::new(MediaEngine::new());
        let counter = spawned.clone();
        let spawn: Box<dyn Fn() -> JoinHandle<()> + Send> = Box::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(std::future::pending())
        });
        let mut listener = SupervisedListener::new(
            "test",
            tokio::spawn(async {}),
            spawn,
            (engine.clone(), IngestListener::Rtmp),
        );

        listener.exited().await;
        let due = listener.restart_due().expect("restart scheduled");
        listener.restart_if_due(due - Duration::from_millis(1));
        assert_eq!(spawned.load(Ordering::Relaxed), 0, "not before the delay");
        let still_down = tokio::time::timeout(Duration::from_secs(5), listener.exited()).await;
        assert!(still_down.is_err(), "a down listener does not exit again");

        listener.restart_if_due(due);
        assert_eq!(spawned.load(Ordering::Relaxed), 1);
        assert_eq!(engine.rtmp_listener_health().restarts, 1);
        assert!(listener.restart_due().is_none());
    }
}
