//! First-class stage lifecycle state.
//!
//! `StagePhase` and `StageBackendKind` are defined in `domain::state` and
//! re-exported here so existing imports from `media::stage_lifecycle` continue
//! to work.

use std::sync::{Arc, Mutex};
use std::time::Instant;

pub use crate::domain::state::{StageBackendKind, StagePhase};

#[derive(Clone, Debug)]
pub struct StageLifecycleSnapshot {
    pub phase: StagePhase,
    pub backend: StageBackendKind,
    pub phase_started_at: Option<Instant>,
    pub first_input_at: Option<Instant>,
    pub first_output_at: Option<Instant>,
    pub last_error: Option<String>,
}

#[derive(Debug)]
struct StageLifecycleInner {
    phase: StagePhase,
    backend: StageBackendKind,
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
        Self {
            inner: Arc::new(Mutex::new(StageLifecycleInner {
                phase: initial,
                backend: StageBackendKind::ExternalFfmpeg,
                phase_started_at: Some(Instant::now()),
                first_input_at: None,
                first_output_at: None,
                last_error: None,
            })),
        }
    }

    pub fn new_with_backend(initial: StagePhase, backend: StageBackendKind) -> Self {
        Self {
            inner: Arc::new(Mutex::new(StageLifecycleInner {
                phase: initial,
                backend,
                phase_started_at: Some(Instant::now()),
                first_input_at: None,
                first_output_at: None,
                last_error: None,
            })),
        }
    }

    pub fn transition(&self, phase: StagePhase) {
        let mut inner = self.inner.lock().expect("stage lifecycle lock poisoned");
        if let Some(backend) = backend_kind_from_phase(&phase) {
            inner.backend = backend;
        }
        inner.phase = phase;
        inner.phase_started_at = Some(Instant::now());
    }

    pub fn record_first_input(&self) {
        let mut inner = self.inner.lock().expect("stage lifecycle lock poisoned");
        if inner.first_input_at.is_none() {
            inner.first_input_at = Some(Instant::now());
            if matches!(inner.phase, StagePhase::BackendSpawned { .. }) {
                inner.phase = StagePhase::FirstInput;
            }
        }
    }

    pub fn record_first_output(&self) {
        let mut inner = self.inner.lock().expect("stage lifecycle lock poisoned");
        if inner.first_output_at.is_none() {
            inner.first_output_at = Some(Instant::now());
            if matches!(inner.phase, StagePhase::FirstInput) {
                inner.phase = StagePhase::FirstOutput;
            }
        }
    }

    pub fn record_producing(&self) {
        let mut inner = self.inner.lock().expect("stage lifecycle lock poisoned");
        if matches!(
            inner.phase,
            StagePhase::FirstOutput | StagePhase::FirstInput | StagePhase::BackendSpawned { .. }
        ) {
            inner.phase = StagePhase::Producing;
        }
    }

    pub fn record_error(&self, error: impl Into<String>) {
        let mut inner = self.inner.lock().expect("stage lifecycle lock poisoned");
        inner.last_error = Some(error.into());
        inner.phase = StagePhase::Failed;
    }

    pub fn snapshot(&self) -> StageLifecycleSnapshot {
        let inner = self.inner.lock().expect("stage lifecycle lock poisoned");
        StageLifecycleSnapshot {
            phase: inner.phase.clone(),
            backend: inner.backend.clone(),
            phase_started_at: inner.phase_started_at,
            first_input_at: inner.first_input_at,
            first_output_at: inner.first_output_at,
            last_error: inner.last_error.clone(),
        }
    }

    pub fn current_phase(&self) -> StagePhase {
        self.inner
            .lock()
            .expect("stage lifecycle lock poisoned")
            .phase
            .clone()
    }

    pub fn current_backend(&self) -> StageBackendKind {
        self.inner
            .lock()
            .expect("stage lifecycle lock poisoned")
            .backend
            .clone()
    }
}

fn backend_kind_from_phase(phase: &StagePhase) -> Option<StageBackendKind> {
    match phase {
        StagePhase::WaitingForCapacity { backend }
        | StagePhase::CapacityAcquired { backend }
        | StagePhase::BackendSpawned { backend, .. } => Some(backend.clone()),
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
    fn lifecycle_records_error() {
        let lc = StageLifecycle::new(StagePhase::Registered);
        lc.record_error("codec not found");
        assert_eq!(lc.current_phase(), StagePhase::Failed);
        assert_eq!(
            lc.snapshot().last_error,
            Some("codec not found".to_string())
        );
    }
}
