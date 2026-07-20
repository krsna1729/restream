use crate::domain::stage::StageKey;
use crate::domain::state::{StageBackendKind, StagePhase};

/// Combined snapshot of a stage's lifecycle and throughput state.
#[derive(Clone, Debug, PartialEq)]
pub struct StageRuntimeSnapshot {
    pub key: StageKey,
    pub backend: StageBackendKind,
    pub phase: StagePhase,
    pub backend_pid: Option<u32>,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub packets_in: u64,
    pub packets_out: u64,
    pub first_input_at: Option<std::time::Instant>,
    pub first_output_at: Option<std::time::Instant>,
    pub last_error: Option<String>,
    pub capacity_permits_total: Option<usize>,
    pub capacity_permits_available: Option<usize>,
    pub capacity_wait_ms: Option<u64>,
}

pub(crate) const fn phase_name(phase: &StagePhase) -> &'static str {
    match phase {
        StagePhase::Planned => "planned",
        StagePhase::Registered => "registered",
        StagePhase::WaitingForDependency { .. } => "waitingForDependency",
        StagePhase::WaitingForMetadata => "waitingForMetadata",
        StagePhase::WaitingForParameterSets => "waitingForParameterSets",
        StagePhase::WaitingForKeyframe => "waitingForKeyframe",
        StagePhase::WaitingForCapacity { .. } => "waitingForCapacity",
        StagePhase::CapacityAcquired { .. } => "capacityAcquired",
        StagePhase::StartingBackend { .. } => "startingBackend",
        StagePhase::BackendSpawned { .. } => "backendSpawned",
        StagePhase::FirstInput => "firstInput",
        StagePhase::RunningNoOutputYet => "runningNoOutputYet",
        StagePhase::FirstOutput => "firstOutput",
        StagePhase::Producing => "producing",
        StagePhase::Failed => "failed",
        StagePhase::Stopping => "stopping",
        StagePhase::Stopped => "stopped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::stage::StageKind;

    #[test]
    fn phase_name_uses_camel_case() {
        assert_eq!(
            phase_name(&StagePhase::WaitingForCapacity {
                backend: StageBackendKind::ExternalFfmpeg,
            }),
            "waitingForCapacity"
        );
        assert_eq!(phase_name(&StagePhase::Producing), "producing");
        assert_eq!(
            phase_name(&StagePhase::WaitingForDependency {
                dependency: StageKey::new("p", StageKind::source()),
            }),
            "waitingForDependency"
        );
    }
}
