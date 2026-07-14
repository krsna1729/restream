use crate::domain::stage::StageKey;
use crate::domain::state::{StageBackendKind, StagePhase};

/// Combined snapshot of a stage's lifecycle and throughput state.
#[derive(Clone, Debug, PartialEq)]
pub struct StageRuntimeSnapshot {
    pub key: StageKey,
    pub backend: StageBackendKind,
    pub phase: StagePhase,
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

impl StageRuntimeSnapshot {
    /// Serialize to a JSON value matching the API status contract.
    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::json!({
            "stage": self.key.to_string(),
            "backend": serde_json::to_value(self.backend).unwrap_or_default(),
            "phase": phase_name(&self.phase),
            "phaseDetail": serde_json::to_value(&self.phase).unwrap_or_default(),
            "bytesIn": self.bytes_in,
            "bytesOut": self.bytes_out,
            "packetsIn": self.packets_in,
            "packetsOut": self.packets_out,
            "lastError": self.last_error,
        });
        if let Some(total) = self.capacity_permits_total {
            obj["capacityPermitsTotal"] = serde_json::json!(total);
        }
        if let Some(avail) = self.capacity_permits_available {
            obj["capacityPermitsAvailable"] = serde_json::json!(avail);
        }
        if let Some(wait_ms) = self.capacity_wait_ms {
            obj["capacityWaitMs"] = serde_json::json!(wait_ms);
        }
        obj
    }
}

pub(crate) fn phase_name(phase: &StagePhase) -> String {
    match phase {
        StagePhase::Planned => "planned".to_string(),
        StagePhase::Registered => "registered".to_string(),
        StagePhase::WaitingForDependency { .. } => "waitingForDependency".to_string(),
        StagePhase::WaitingForMetadata => "waitingForMetadata".to_string(),
        StagePhase::WaitingForParameterSets => "waitingForParameterSets".to_string(),
        StagePhase::WaitingForKeyframe => "waitingForKeyframe".to_string(),
        StagePhase::WaitingForCapacity { .. } => "waitingForCapacity".to_string(),
        StagePhase::CapacityAcquired { .. } => "capacityAcquired".to_string(),
        StagePhase::StartingBackend { .. } => "startingBackend".to_string(),
        StagePhase::BackendSpawned { .. } => "backendSpawned".to_string(),
        StagePhase::FirstInput => "firstInput".to_string(),
        StagePhase::RunningNoOutputYet => "runningNoOutputYet".to_string(),
        StagePhase::FirstOutput => "firstOutput".to_string(),
        StagePhase::Producing => "producing".to_string(),
        StagePhase::Failed => "failed".to_string(),
        StagePhase::Stopping => "stopping".to_string(),
        StagePhase::Stopped => "stopped".to_string(),
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
