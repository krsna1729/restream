//! One stage-phase table read three ways: the status JSON, the processing
//! graph node and the derived alerts must agree on every phase.

use super::super::graph_projection::processing_graph_stage_node;
use super::super::stage_projection::stage_runtime_snapshot_json;
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_domain::domain::state::{StageBackendKind, StagePhase};
use restream_domain::runtime::stage::{StageRuntimeSnapshot, phase_name};
use restream_health::alerts::derive_alerts;
use serde_json::json;

fn stage_snapshot(
    key: StageKey,
    phase: StagePhase,
    bytes_in: u64,
    bytes_out: u64,
    last_error: Option<&str>,
) -> StageRuntimeSnapshot {
    let backend = match &phase {
        StagePhase::WaitingForCapacity { backend }
        | StagePhase::CapacityAcquired { backend }
        | StagePhase::StartingBackend { backend }
        | StagePhase::BackendSpawned { backend, .. } => *backend,
        _ => StageBackendKind::ExternalFfmpeg,
    };
    StageRuntimeSnapshot {
        key,
        backend,
        phase,
        backend_pid: None,
        bytes_in,
        bytes_out,
        packets_in: bytes_in.min(1),
        packets_out: bytes_out.min(1),
        first_input_at: None,
        first_output_at: None,
        last_error: last_error.map(ToString::to_string),
        capacity_permits_total: None,
        capacity_permits_available: None,
        capacity_wait_ms: None,
    }
}

#[test]
fn stage_phase_table_is_consistent_for_status_graph_and_alerts() {
    let dependency = StageKey::new("pipe-stage-table", StageKind::source());
    let cases = vec![
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("planned")),
            StagePhase::Planned,
            0,
            0,
            None,
            None,
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("registered")),
            StagePhase::Registered,
            0,
            0,
            None,
            None,
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("dependency")),
            StagePhase::WaitingForDependency {
                dependency: dependency.clone(),
            },
            0,
            0,
            None,
            None,
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("metadata")),
            StagePhase::WaitingForMetadata,
            0,
            0,
            None,
            None,
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("parameters")),
            StagePhase::WaitingForParameterSets,
            0,
            0,
            None,
            None,
        ),
        (
            StageKey::new(
                "pipe-stage-table",
                StageKind::preview("720p", StageKind::source()),
            ),
            StagePhase::WaitingForKeyframe,
            0,
            0,
            None,
            Some("waiting_for_keyframe"),
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("capacity")),
            StagePhase::WaitingForCapacity {
                backend: StageBackendKind::ExternalFfmpeg,
            },
            0,
            0,
            None,
            Some("capacity_exhausted"),
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("acquired")),
            StagePhase::CapacityAcquired {
                backend: StageBackendKind::ExternalFfmpeg,
            },
            0,
            0,
            None,
            None,
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("starting")),
            StagePhase::StartingBackend {
                backend: StageBackendKind::ExternalFfmpeg,
            },
            0,
            0,
            None,
            None,
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("spawned")),
            StagePhase::BackendSpawned {
                backend: StageBackendKind::ExternalFfmpeg,
                pid: Some(1234),
            },
            0,
            0,
            None,
            None,
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("first-input")),
            StagePhase::FirstInput,
            256,
            0,
            None,
            None,
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("no-output")),
            StagePhase::RunningNoOutputYet,
            256,
            0,
            None,
            Some("no_output"),
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("failed")),
            StagePhase::Failed,
            0,
            0,
            Some("synthetic failure"),
            Some("failed"),
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("stopping")),
            StagePhase::Stopping,
            0,
            0,
            None,
            None,
        ),
        (
            StageKey::new("pipe-stage-table", StageKind::video_preset("stopped")),
            StagePhase::Stopped,
            0,
            0,
            None,
            None,
        ),
    ];

    let mut stages = serde_json::Map::new();
    let mut expected_alert_fragments = Vec::new();
    for (key, phase, bytes_in, bytes_out, last_error, expected_alert) in cases {
        let snapshot = stage_snapshot(key.clone(), phase.clone(), bytes_in, bytes_out, last_error);
        let status_json = stage_runtime_snapshot_json(&snapshot);
        let graph_node = processing_graph_stage_node(
            key.kind.graph_node_id(key.pipeline.as_str()),
            key.kind.graph_type(),
            key.kind.graph_label(),
            key.to_string(),
            Some(&snapshot),
            true,
            None,
            None,
            json!({}),
        );

        assert_eq!(status_json["phase"], phase_name(&phase));
        assert_eq!(graph_node["details"]["phase"], status_json["phase"]);
        assert_eq!(
            graph_node["details"]["phaseDetail"],
            status_json["phaseDetail"]
        );
        if let Some(fragment) = expected_alert {
            expected_alert_fragments.push(fragment);
        }
        stages.insert(key.to_string(), status_json);
    }

    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "bondingAvailable": true },
        "pipelines": {},
        "stages": stages
    });
    let alerts = derive_alerts(&snap);
    let alert_ids = alerts
        .iter()
        .map(|alert| alert.id.as_str())
        .collect::<Vec<_>>();

    for fragment in expected_alert_fragments {
        assert!(
            alert_ids.iter().any(|id| id.contains(fragment)),
            "missing alert containing {fragment}; got {alert_ids:?}"
        );
    }
}
