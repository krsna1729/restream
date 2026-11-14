//! Background loops (event consumer, projector, samplers, notifier,
//! scheduler, recording catalog writer) are restarted after a panic instead
//! of staying dead while the process keeps serving: a bare `tokio::spawn`
//! would drop the panic with the task, and dashboard state, reconcile
//! triggers, schedules or recording rows would silently stop. A loop that
//! returns (shutdown) is not restarted.

use std::future::Future;
use std::panic::AssertUnwindSafe;

use futures_util::FutureExt as _;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::error;

use super::listener_supervisor::RestartBackoff;

/// Spawns the loop `run` builds, rebuilding and restarting it after the
/// listener restart backoff each time it panics.
pub(crate) fn spawn_supervised<F, Fut>(name: &'static str, mut run: F) -> JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let mut backoff = RestartBackoff::new();
        loop {
            let started = Instant::now();
            if AssertUnwindSafe(run()).catch_unwind().await.is_ok() {
                return;
            }
            let delay = backoff.on_exit(started.elapsed());
            error!(
                task = name,
                restart_delay_ms = delay.as_millis() as u64,
                event_class = "lifecycle",
                event_type = "background_task.panicked",
                "background task panicked; restarting"
            );
            tokio::time::sleep(delay).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    /// A panicking loop is rebuilt and run again after the backoff; a loop
    /// that returns ends the supervisor.
    #[tokio::test(start_paused = true)]
    async fn a_panicking_loop_is_restarted_and_a_returning_loop_ends() {
        let runs = Arc::new(AtomicU32::new(0));
        let counter = runs.clone();
        let task = spawn_supervised("test", move || {
            let run = counter.fetch_add(1, Ordering::Relaxed);
            async move {
                if run == 0 {
                    std::panic::resume_unwind(Box::new("injected background task panic"));
                }
            }
        });
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the supervisor ends once the loop returns")
            .expect("the panic stays inside the supervisor");
        assert_eq!(runs.load(Ordering::Relaxed), 2);
    }
}
