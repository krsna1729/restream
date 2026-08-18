//! First-class stage lifecycle state.
//!
//! `StagePhase` and `StageBackendKind` are defined in `domain::state`. Their
//! re-export here is a deliberate stable public API: downstream lifecycle
//! observers consume the types beside `StageLifecycleSnapshot`, while the
//! implementation and dependency ownership remain in `domain`.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

pub use crate::domain::state::{StageBackendKind, StagePhase};

// A panic while holding `StageLifecycle::inner` (e.g. from an unrelated bug
// elsewhere in the same critical section) must not permanently disable
// lifecycle tracking for the rest of the stage's life: every other lock site
// uses `.expect()`, so poisoning here would cascade into a stuck
// `.expect()` panic on every subsequent transition/snapshot call, in
// violation of the "no internal failure path may crash the engine" rule in
// AGENTS.md. Recovering the guard is safe because the inner state is a
// plain data struct with no invariant that spans multiple field writes.
fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone, Debug)]
pub struct StageLifecycleSnapshot {
    pub phase: StagePhase,
    pub backend: StageBackendKind,
    pub backend_pid: Option<u32>,
    pub phase_started_at: Option<Instant>,
    pub first_input_at: Option<Instant>,
    pub first_output_at: Option<Instant>,
    pub last_error: Option<String>,
}

#[derive(Debug)]
struct StageLifecycleInner {
    phase: StagePhase,
    backend: StageBackendKind,
    backend_pid: Option<u32>,
    phase_started_at: Option<Instant>,
    first_input_at: Option<Instant>,
    first_output_at: Option<Instant>,
    last_error: Option<String>,
}

/// RAII guard that transitions the stage to `Stopped` on normal exit unless
/// the stage has already been marked `Failed`.
pub struct StageLifecycleGuard {
    lifecycle: Arc<StageLifecycle>,
    finished: bool,
}

impl StageLifecycleGuard {
    pub fn new(lifecycle: Arc<StageLifecycle>) -> Self {
        Self {
            lifecycle,
            finished: false,
        }
    }

    pub fn finish(mut self) {
        self.finished = true;
        self.lifecycle.transition(StagePhase::Stopped);
    }
}

impl Drop for StageLifecycleGuard {
    fn drop(&mut self) {
        if !self.finished {
            let phase = self.lifecycle.current_phase();
            if !matches!(phase, StagePhase::Failed) {
                self.lifecycle.transition(StagePhase::Stopped);
            }
        }
    }
}

/// Thread-safe stage lifecycle tracker.
#[derive(Clone, Debug)]
pub struct StageLifecycle {
    inner: Arc<Mutex<StageLifecycleInner>>,
}

impl StageLifecycle {
    pub fn new(initial: StagePhase) -> Self {
        let backend_pid = backend_pid_from_phase(&initial);
        Self {
            inner: Arc::new(Mutex::new(StageLifecycleInner {
                phase: initial,
                backend: StageBackendKind::ExternalFfmpeg,
                backend_pid,
                phase_started_at: Some(Instant::now()),
                first_input_at: None,
                first_output_at: None,
                last_error: None,
            })),
        }
    }

    pub fn new_with_backend(initial: StagePhase, backend: StageBackendKind) -> Self {
        let backend_pid = backend_pid_from_phase(&initial);
        Self {
            inner: Arc::new(Mutex::new(StageLifecycleInner {
                phase: initial,
                backend,
                backend_pid,
                phase_started_at: Some(Instant::now()),
                first_input_at: None,
                first_output_at: None,
                last_error: None,
            })),
        }
    }

    pub fn transition(&self, phase: StagePhase) {
        let mut inner = lock_or_recover(&self.inner);
        // A stale StartingBackend/BackendSpawned event racing in after the
        // stage has already progressed past first input must be ignored
        // wholesale: not just the phase, but also the backend kind and pid
        // it carries, otherwise the snapshot ends up with a phase that says
        // "still running the original backend" alongside metadata for a
        // spawn attempt that was never actually adopted.
        if inner.first_input_at.is_some()
            && matches!(
                phase,
                StagePhase::StartingBackend { .. } | StagePhase::BackendSpawned { .. }
            )
            && matches!(
                inner.phase,
                StagePhase::FirstInput | StagePhase::FirstOutput | StagePhase::Producing
            )
        {
            return;
        }
        if let Some(backend) = backend_kind_from_phase(&phase) {
            inner.backend = backend;
        }
        if let Some(pid) = backend_pid_from_phase(&phase) {
            inner.backend_pid = Some(pid);
        }
        inner.phase = phase;
        inner.phase_started_at = Some(Instant::now());
    }

    pub fn record_first_input(&self) {
        let mut inner = lock_or_recover(&self.inner);
        if inner.first_input_at.is_none() {
            inner.first_input_at = Some(Instant::now());
            if matches!(
                inner.phase,
                StagePhase::BackendSpawned { .. } | StagePhase::StartingBackend { .. }
            ) {
                inner.phase = StagePhase::FirstInput;
            }
        }
    }

    pub fn record_first_output(&self) {
        let mut inner = lock_or_recover(&self.inner);
        if inner.first_output_at.is_none() {
            inner.first_output_at = Some(Instant::now());
            if matches!(
                inner.phase,
                StagePhase::StartingBackend { .. }
                    | StagePhase::BackendSpawned { .. }
                    | StagePhase::FirstInput
                    | StagePhase::RunningNoOutputYet
            ) {
                inner.phase = StagePhase::FirstOutput;
            }
        }
    }

    pub fn record_producing(&self) {
        let mut inner = lock_or_recover(&self.inner);
        if matches!(
            inner.phase,
            StagePhase::FirstOutput
                | StagePhase::FirstInput
                | StagePhase::RunningNoOutputYet
                | StagePhase::BackendSpawned { .. }
        ) {
            inner.phase = StagePhase::Producing;
        }
    }

    pub fn record_error(&self, error: impl Into<String>) {
        let mut inner = lock_or_recover(&self.inner);
        inner.last_error = Some(error.into());
        inner.phase = StagePhase::Failed;
    }

    pub fn snapshot(&self) -> StageLifecycleSnapshot {
        let inner = lock_or_recover(&self.inner);
        StageLifecycleSnapshot {
            phase: inner.phase.clone(),
            backend: inner.backend,
            backend_pid: inner.backend_pid,
            phase_started_at: inner.phase_started_at,
            first_input_at: inner.first_input_at,
            first_output_at: inner.first_output_at,
            last_error: inner.last_error.clone(),
        }
    }

    pub fn current_phase(&self) -> StagePhase {
        lock_or_recover(&self.inner).phase.clone()
    }

    pub fn current_backend(&self) -> StageBackendKind {
        lock_or_recover(&self.inner).backend
    }
}

fn backend_kind_from_phase(phase: &StagePhase) -> Option<StageBackendKind> {
    match phase {
        StagePhase::WaitingForCapacity { backend }
        | StagePhase::CapacityAcquired { backend }
        | StagePhase::StartingBackend { backend }
        | StagePhase::BackendSpawned { backend, .. } => Some(*backend),
        _ => None,
    }
}

fn backend_pid_from_phase(phase: &StagePhase) -> Option<u32> {
    match phase {
        StagePhase::BackendSpawned { pid, .. } => *pid,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_transitions_through_phases() {
        let lc = StageLifecycle::new(StagePhase::Registered);
        assert_eq!(lc.current_phase(), StagePhase::Registered);

        lc.transition(StagePhase::WaitingForCapacity {
            backend: StageBackendKind::ExternalFfmpeg,
        });
        assert!(matches!(
            lc.current_phase(),
            StagePhase::WaitingForCapacity { .. }
        ));

        lc.transition(StagePhase::BackendSpawned {
            backend: StageBackendKind::ExternalFfmpeg,
            pid: None,
        });
        lc.record_first_input();
        assert_eq!(lc.current_phase(), StagePhase::FirstInput);

        lc.record_first_output();
        assert_eq!(lc.current_phase(), StagePhase::FirstOutput);

        lc.record_producing();
        assert_eq!(lc.current_phase(), StagePhase::Producing);
    }

    #[test]
    fn first_output_can_transition_directly_from_backend_spawned() {
        let lc = StageLifecycle::new(StagePhase::BackendSpawned {
            backend: StageBackendKind::InternalFfmpeg,
            pid: None,
        });

        lc.record_first_output();

        let snapshot = lc.snapshot();
        assert_eq!(snapshot.phase, StagePhase::FirstOutput);
        assert!(snapshot.first_output_at.is_some());
    }

    #[test]
    fn backend_spawned_transition_does_not_regress_after_first_input() {
        let lc = StageLifecycle::new(StagePhase::BackendSpawned {
            backend: StageBackendKind::InternalFfmpeg,
            pid: None,
        });

        lc.record_first_input();
        lc.transition(StagePhase::BackendSpawned {
            backend: StageBackendKind::InternalFfmpeg,
            pid: None,
        });

        let snapshot = lc.snapshot();
        assert_eq!(snapshot.phase, StagePhase::FirstInput);
        assert!(snapshot.first_input_at.is_some());
    }

    #[test]
    fn backend_pid_survives_runtime_phase_progression() {
        let lc = StageLifecycle::new(StagePhase::BackendSpawned {
            backend: StageBackendKind::ExternalFfmpeg,
            pid: Some(42),
        });

        lc.record_first_input();
        lc.record_first_output();

        let snapshot = lc.snapshot();
        assert_eq!(snapshot.phase, StagePhase::FirstOutput);
        assert_eq!(snapshot.backend_pid, Some(42));
    }

    #[test]
    fn lifecycle_records_error() {
        let lc = StageLifecycle::new(StagePhase::Registered);
        lc.record_error("codec not found");
        assert_eq!(lc.current_phase(), StagePhase::Failed);
        assert_eq!(
            lc.snapshot().last_error,
            Some("codec not found".to_string())
        );
    }

    #[test]
    fn guard_finish_transitions_to_stopped() {
        let lc = Arc::new(StageLifecycle::new(StagePhase::Producing));
        let guard = StageLifecycleGuard::new(lc.clone());
        guard.finish();
        assert_eq!(lc.current_phase(), StagePhase::Stopped);
    }

    #[test]
    fn guard_drop_without_finish_transitions_to_stopped() {
        let lc = Arc::new(StageLifecycle::new(StagePhase::Producing));
        {
            let _guard = StageLifecycleGuard::new(lc.clone());
        }
        assert_eq!(
            lc.current_phase(),
            StagePhase::Stopped,
            "dropping the guard without calling finish() must still stop the stage"
        );
    }

    #[test]
    fn guard_drop_after_failed_does_not_regress_to_stopped() {
        let lc = Arc::new(StageLifecycle::new(StagePhase::Producing));
        {
            let _guard = StageLifecycleGuard::new(lc.clone());
            lc.record_error("backend crashed");
        }
        assert_eq!(
            lc.current_phase(),
            StagePhase::Failed,
            "an unfinished guard must not overwrite a Failed phase on drop"
        );
    }

    #[test]
    fn guard_finish_forces_stopped_even_after_failed() {
        // Unlike the implicit Drop path, an explicit finish() call
        // unconditionally forces Stopped, even over a Failed phase.
        let lc = Arc::new(StageLifecycle::new(StagePhase::Producing));
        let guard = StageLifecycleGuard::new(lc.clone());
        lc.record_error("backend crashed");
        guard.finish();
        assert_eq!(lc.current_phase(), StagePhase::Stopped);
    }

    #[test]
    fn stale_backend_spawned_transition_does_not_corrupt_backend_metadata() {
        // A stale BackendSpawned event arriving after FirstInput (e.g. from
        // a duplicate or racing spawn attempt) must be ignored wholesale:
        // the phase update is already known to be suppressed, and the
        // backend kind / pid it carries must be suppressed along with it,
        // not silently adopted while the phase stays put.
        let lc = StageLifecycle::new(StagePhase::BackendSpawned {
            backend: StageBackendKind::ExternalFfmpeg,
            pid: Some(111),
        });
        lc.record_first_input();

        lc.transition(StagePhase::BackendSpawned {
            backend: StageBackendKind::InternalFfmpeg,
            pid: Some(222),
        });

        let snapshot = lc.snapshot();
        assert_eq!(snapshot.phase, StagePhase::FirstInput);
        assert_eq!(
            snapshot.backend_pid,
            Some(111),
            "a suppressed BackendSpawned transition must not silently overwrite the pid"
        );
        assert_eq!(
            snapshot.backend,
            StageBackendKind::ExternalFfmpeg,
            "a suppressed BackendSpawned transition must not silently overwrite the backend kind"
        );
    }

    #[test]
    fn record_first_input_is_idempotent() {
        let lc = StageLifecycle::new(StagePhase::BackendSpawned {
            backend: StageBackendKind::InternalFfmpeg,
            pid: None,
        });
        lc.record_first_input();
        let first = lc.snapshot().first_input_at;

        lc.transition(StagePhase::RunningNoOutputYet);
        lc.record_first_input();
        let second = lc.snapshot().first_input_at;

        assert_eq!(first, second, "first_input_at must be set only once");
        assert_eq!(
            lc.current_phase(),
            StagePhase::RunningNoOutputYet,
            "a repeated record_first_input() must not re-force the FirstInput phase"
        );
    }

    #[test]
    fn record_first_output_is_idempotent() {
        let lc = StageLifecycle::new(StagePhase::FirstInput);
        lc.record_first_output();
        let first = lc.snapshot().first_output_at;

        lc.record_producing();
        lc.record_first_output();
        let second = lc.snapshot().first_output_at;

        assert_eq!(first, second, "first_output_at must be set only once");
        assert_eq!(
            lc.current_phase(),
            StagePhase::Producing,
            "a repeated record_first_output() must not regress an already-Producing phase"
        );
    }

    #[test]
    fn record_producing_is_ignored_outside_allowed_phases() {
        let lc = StageLifecycle::new(StagePhase::Registered);
        lc.record_producing();
        assert_eq!(
            lc.current_phase(),
            StagePhase::Registered,
            "record_producing() must not force Producing from an unrelated phase"
        );
    }

    #[test]
    fn new_with_backend_and_current_backend_report_the_constructed_backend() {
        let lc = StageLifecycle::new_with_backend(
            StagePhase::Registered,
            StageBackendKind::InternalFfmpeg,
        );
        assert_eq!(lc.current_backend(), StageBackendKind::InternalFfmpeg);
    }

    #[test]
    fn poisoned_inner_lock_does_not_panic_subsequent_calls() {
        let _expected_panic_silencer = crate::media::test_support::silence_expected_panics();
        let lc = StageLifecycle::new(StagePhase::Producing);

        let poison_lc = lc.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = poison_lc.inner.lock().unwrap();
            panic!("poison stage lifecycle lock");
        }));

        lc.record_error("recovered after poison");
        assert_eq!(lc.current_phase(), StagePhase::Failed);
        assert_eq!(
            lc.snapshot().last_error,
            Some("recovered after poison".to_string()),
            "a poisoned lifecycle lock must still be usable by later transitions and reads"
        );
    }
}
