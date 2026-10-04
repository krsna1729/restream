//! Ingest listeners are restarted, not fatal.
//!
//! A listener task (RTMP or SRT) that ends without a shutdown request is
//! respawned after a backoff instead of shutting the process down: one
//! failed listener must not stop egress, the API, or the other protocol.
//! Per-entity panics are contained inside the listener (`panic_boundary`);
//! this layer covers whole-listener failures (accept errors, an Owner
//! fault) so they are reported and recovered, not escalated.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{error, info};

const RESTART_DELAY_MIN: Duration = Duration::from_secs(1);
const RESTART_DELAY_MAX: Duration = Duration::from_secs(30);
/// A listener that ran this long before exiting restarts at the minimum
/// delay again.
const HEALTHY_RUN: Duration = Duration::from_secs(60);

/// Exponential restart delay: 1 s doubling to 30 s, reset after a healthy run.
#[derive(Debug)]
pub(super) struct RestartBackoff {
    next_delay: Duration,
}

impl RestartBackoff {
    pub(super) fn new() -> Self {
        Self {
            next_delay: RESTART_DELAY_MIN,
        }
    }

    /// The delay before restarting a listener that ran for `ran`.
    pub(super) fn on_exit(&mut self, ran: Duration) -> Duration {
        if ran >= HEALTHY_RUN {
            self.next_delay = RESTART_DELAY_MIN;
        }
        let delay = self.next_delay;
        self.next_delay = (self.next_delay * 2).min(RESTART_DELAY_MAX);
        delay
    }
}

pub(super) struct SupervisedListener {
    name: &'static str,
    spawn: Box<dyn Fn() -> JoinHandle<()> + Send>,
    task: Option<JoinHandle<()>>,
    started_at: Instant,
    restart_at: Option<Instant>,
    backoff: RestartBackoff,
    restarts: Arc<AtomicU64>,
}

impl SupervisedListener {
    /// Wraps a task that `spawn` already started once.
    pub(super) fn new(
        name: &'static str,
        task: JoinHandle<()>,
        spawn: Box<dyn Fn() -> JoinHandle<()> + Send>,
        restarts: Arc<AtomicU64>,
    ) -> Self {
        Self {
            name,
            spawn,
            task: Some(task),
            started_at: Instant::now(),
            restart_at: None,
            backoff: RestartBackoff::new(),
            restarts,
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
            self.restarts.fetch_add(1, Ordering::Relaxed);
            info!(listener = self.name, "ingest listener restarted");
        }
    }

    pub(super) fn into_handle(self) -> JoinHandle<()> {
        self.task.unwrap_or_else(|| tokio::spawn(async {}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_delay_doubles_to_the_cap_and_resets_after_a_healthy_run() {
        let mut backoff = RestartBackoff::new();
        let quick = Duration::from_secs(1);
        let delays: Vec<u64> = (0..7).map(|_| backoff.on_exit(quick).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(backoff.on_exit(HEALTHY_RUN), RESTART_DELAY_MIN);
        assert_eq!(backoff.on_exit(quick), Duration::from_secs(2));
    }

    /// A listener task that ends is respawned after its delay, counted, and
    /// the supervisor never resolves `exited` while it is down.
    #[tokio::test(start_paused = true)]
    async fn an_exited_listener_is_respawned_after_its_delay() {
        let spawned = Arc::new(AtomicU64::new(0));
        let restarts = Arc::new(AtomicU64::new(0));
        let counter = spawned.clone();
        let spawn: Box<dyn Fn() -> JoinHandle<()> + Send> = Box::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(std::future::pending())
        });
        let mut listener =
            SupervisedListener::new("test", tokio::spawn(async {}), spawn, restarts.clone());

        listener.exited().await;
        let due = listener.restart_due().expect("restart scheduled");
        listener.restart_if_due(due - Duration::from_millis(1));
        assert_eq!(spawned.load(Ordering::Relaxed), 0, "not before the delay");
        let still_down = tokio::time::timeout(Duration::from_secs(5), listener.exited()).await;
        assert!(still_down.is_err(), "a down listener does not exit again");

        listener.restart_if_due(due);
        assert_eq!(spawned.load(Ordering::Relaxed), 1);
        assert_eq!(restarts.load(Ordering::Relaxed), 1);
        assert!(listener.restart_due().is_none());
    }
}
