use crate::runtime::stage::{StageRuntimeSnapshot, phase_name};

pub(crate) fn stage_runtime_snapshot_json(snapshot: &StageRuntimeSnapshot) -> serde_json::Value {
    let mut value = serde_json::json!({
        "stage": snapshot.key.to_string(),
        "backend": serde_json::to_value(snapshot.backend).unwrap_or_default(),
        "phase": phase_name(&snapshot.phase),
        "phaseDetail": serde_json::to_value(&snapshot.phase).unwrap_or_default(),
        "backendPid": snapshot.backend_pid,
        "bytesIn": snapshot.bytes_in,
        "bytesOut": snapshot.bytes_out,
        "packetsIn": snapshot.packets_in,
        "packetsOut": snapshot.packets_out,
        "lastError": snapshot.last_error,
    });
    if let Some(total) = snapshot.capacity_permits_total {
        value["capacityPermitsTotal"] = serde_json::json!(total);
    }
    if let Some(available) = snapshot.capacity_permits_available {
        value["capacityPermitsAvailable"] = serde_json::json!(available);
    }
    if let Some(wait_ms) = snapshot.capacity_wait_ms {
        value["capacityWaitMs"] = serde_json::json!(wait_ms);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::stage::{StageKey, StageKind};
    use crate::domain::state::{StageBackendKind, StagePhase};

    #[test]
    fn snapshot_projection_preserves_status_contract() {
        let snapshot = StageRuntimeSnapshot {
            key: StageKey::new("pipe-1", StageKind::video_preset("720p")),
            backend: StageBackendKind::ExternalFfmpeg,
            phase: StagePhase::BackendSpawned {
                backend: StageBackendKind::ExternalFfmpeg,
                pid: Some(1234),
            },
            backend_pid: Some(1234),
            bytes_in: 1,
            bytes_out: 2,
            packets_in: 3,
            packets_out: 4,
            first_input_at: None,
            first_output_at: None,
            last_error: Some("worker exited".to_string()),
            capacity_permits_total: Some(8),
            capacity_permits_available: Some(3),
            capacity_wait_ms: Some(55),
        };

        let value = stage_runtime_snapshot_json(&snapshot);

        assert_eq!(
            value,
            serde_json::json!({
                "stage": "pipe-1:video:720p",
                "backend": "externalFfmpeg",
                "phase": "backendSpawned",
                "phaseDetail": {
                    "phase": "backendSpawned",
                    "backend": "externalFfmpeg",
                    "pid": 1234,
                },
                "backendPid": 1234,
                "bytesIn": 1,
                "bytesOut": 2,
                "packetsIn": 3,
                "packetsOut": 4,
                "lastError": "worker exited",
                "capacityPermitsTotal": 8,
                "capacityPermitsAvailable": 3,
                "capacityWaitMs": 55,
            })
        );
    }

    #[test]
    fn snapshot_projection_omits_absent_capacity_fields() {
        let snapshot = StageRuntimeSnapshot {
            key: StageKey::new("pipe-1", StageKind::source()),
            backend: StageBackendKind::InternalFfmpeg,
            phase: StagePhase::Producing,
            backend_pid: None,
            bytes_in: 0,
            bytes_out: 0,
            packets_in: 0,
            packets_out: 0,
            first_input_at: None,
            first_output_at: None,
            last_error: None,
            capacity_permits_total: None,
            capacity_permits_available: None,
            capacity_wait_ms: None,
        };

        let value = stage_runtime_snapshot_json(&snapshot);

        assert!(value.get("capacityPermitsTotal").is_none());
        assert!(value.get("capacityPermitsAvailable").is_none());
        assert!(value.get("capacityWaitMs").is_none());
    }
}
