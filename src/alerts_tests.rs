use super::*;
use crate::api_runtime_views::stage_runtime_snapshot_json;
use crate::domain::stage::{StageKey, StageKind};
use crate::domain::state::{StageBackendKind, StagePhase};
use crate::runtime::stage::{StageRuntimeSnapshot, phase_name};
use serde_json::json;

fn snapshot_with_pipeline(pipeline_id: &str, input_status: &str) -> serde_json::Value {
    json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {
            pipeline_id: {
                "input": {
                    "status": input_status,
                    "readerMetrics": []
                },
                "outputs": {}
            }
        }
    })
}

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
fn clean_snapshot_yields_no_alerts() {
    let snap = snapshot_with_pipeline("pipe1", "on");
    assert!(derive_alerts(&snap).is_empty());
}

#[test]
fn publisher_absent_yields_critical_alert() {
    let snap = snapshot_with_pipeline("pipe1", "off");
    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Critical);
    assert_eq!(alerts[0].scope, Scope::Pipeline);
    assert_eq!(alerts[0].pipeline_id.as_deref(), Some("pipe1"));
    assert!(alerts[0].id.contains("no_publisher"));
}

#[test]
fn reader_lag_above_threshold_yields_warning() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {
            "pipe1": {
                "input": {
                    "status": "on",
                    "readerMetrics": [
                        { "name": "rtmp_egress", "lagSlots": 300, "overflowCount": 0 }
                    ]
                },
                "outputs": {}
            }
        }
    });
    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Warning);
    assert_eq!(alerts[0].scope, Scope::Stage);
    assert!(alerts[0].id.contains("lag"));
}

#[test]
fn reader_lag_below_threshold_yields_no_alert() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {
            "pipe1": {
                "input": {
                    "status": "on",
                    "readerMetrics": [
                        { "name": "rtmp_egress", "lagSlots": 10, "overflowCount": 0 }
                    ]
                },
                "outputs": {}
            }
        }
    });
    assert!(derive_alerts(&snap).is_empty());
}

#[test]
fn reader_overflow_yields_warning() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {
            "pipe1": {
                "input": {
                    "status": "on",
                    "readerMetrics": [
                        { "name": "hls", "lagSlots": 0, "overflowCount": 5 }
                    ]
                },
                "outputs": {}
            }
        }
    });
    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Warning);
    assert_eq!(alerts[0].scope, Scope::Stage);
    assert!(alerts[0].id.contains("overflow"));
}

#[test]
fn stopped_output_with_active_publisher_yields_warning() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {
            "pipe1": {
                "input": {
                    "status": "on",
                    "readerMetrics": []
                },
                "outputs": {
                    "out1": { "status": "stopped", "totalSize": 0 }
                }
            }
        }
    });
    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Warning);
    assert_eq!(alerts[0].scope, Scope::Output);
    assert_eq!(alerts[0].output_id.as_deref(), Some("out1"));
}

#[test]
fn stopped_output_without_publisher_yields_no_alert() {
    // Output warnings are suppressed when there's no publisher — nothing to forward.
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {
            "pipe1": {
                "input": {
                    "status": "off",
                    "readerMetrics": []
                },
                "outputs": {
                    "out1": { "status": "stopped", "totalSize": 0 }
                }
            }
        }
    });
    let alerts = derive_alerts(&snap);
    // Only the Critical no_publisher alert, not a Warning for output.
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Critical);
}

#[test]
fn failed_output_phase_yields_warning() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {
            "pipe1": {
                "input": { "status": "on", "readerMetrics": [] },
                "outputs": {
                    "out1": {
                        "status": "running",
                        "phase": "failed",
                        "failurePhase": "connect",
                        "lastError": "connection refused"
                    }
                }
            }
        }
    });

    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].scope, Scope::Output);
    assert!(alerts[0].id.contains("failed_phase"));
    assert!(
        alerts[0]
            .evidence
            .iter()
            .any(|e| e.contains("connection refused"))
    );
}

#[test]
fn output_blocked_by_stage_yields_causal_warning() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {
            "pipe1": {
                "input": { "status": "on", "readerMetrics": [] },
                "outputs": {
                    "out1": {
                        "status": "running",
                        "phase": "waitingUpstream",
                        "terminalStage": "pipe1:video:720p",
                        "blockedBy": {
                            "stage": "pipe1:video:720p",
                            "phase": "waitingForCapacity",
                            "backend": "externalFfmpeg",
                            "capacityWaitMs": 7000
                        }
                    }
                }
            }
        }
    });

    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].scope, Scope::Output);
    assert_eq!(alerts[0].output_id.as_deref(), Some("out1"));
    assert_eq!(alerts[0].stage_id.as_deref(), Some("pipe1:video:720p"));
    assert!(alerts[0].id.contains("blocked_by_stage"));
    assert!(
        alerts[0]
            .recommended_action
            .contains("Increase external FFmpeg capacity")
    );
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
        let graph_node = crate::api_runtime_views::processing_graph_stage_node(
            key.kind.graph_node_id(key.pipeline.as_str()),
            key.kind.graph_type(),
            key.kind.graph_label(),
            key.to_string(),
            Some(&snapshot),
            true,
            None,
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
        "srtListener": { "udpDrops": 0 },
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

#[test]
fn stale_output_progress_yields_warning_after_successful_send() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {
            "pipe1": {
                "input": { "status": "on", "readerMetrics": [] },
                "outputs": {
                    "out1": {
                        "status": "running",
                        "phase": "sending",
                        "totalSize": 1316,
                        "lastProgressAgeMs": 12_000
                    }
                }
            }
        }
    });

    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].scope, Scope::Output);
    assert!(alerts[0].id.contains("stale_progress"));
}

#[test]
fn srt_udp_drops_yield_engine_warning() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 42 },
        "pipelines": {}
    });
    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Warning);
    assert_eq!(alerts[0].scope, Scope::Engine);
}

#[test]
fn low_nofile_limit_yields_engine_warning() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "runtimeLimits": {
            "nofile": {
                "configured": 65536,
                "soft": 1024,
                "hard": 1024,
                "satisfied": false
            }
        },
        "srtListener": { "udpDrops": 0 },
        "pipelines": {}
    });
    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Warning);
    assert_eq!(alerts[0].scope, Scope::Engine);
    assert_eq!(alerts[0].id, "engine:runtime:nofile_limit_too_low");
    assert!(
        alerts[0]
            .evidence
            .iter()
            .any(|evidence| evidence == "soft = 1024")
    );
}

#[test]
fn rtmp_fd_exhaustion_yields_critical_engine_alert() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "rtmpListener": {
            "acceptErrors": 7,
            "fdExhaustionErrors": 3
        },
        "srtListener": { "udpDrops": 0 },
        "pipelines": {}
    });
    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Critical);
    assert_eq!(alerts[0].scope, Scope::Engine);
    assert_eq!(alerts[0].id, "engine:rtmp_listener:fd_exhaustion");
    assert!(
        alerts[0]
            .evidence
            .iter()
            .any(|evidence| evidence == "fdExhaustionErrors = 3")
    );
}

#[test]
fn alerts_sorted_critical_before_warning() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 1 },
        "pipelines": {
            "pipe1": {
                "input": { "status": "off", "readerMetrics": [] },
                "outputs": {}
            }
        }
    });
    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 2);
    assert_eq!(alerts[0].severity, Severity::Critical);
    assert_eq!(alerts[1].severity, Severity::Warning);
}

#[test]
fn tracker_stamps_first_and_last_seen() {
    let tracker = AlertTracker::new();
    let snap = snapshot_with_pipeline("pipe1", "off");
    let mut alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert!(alerts[0].first_seen.is_none());

    tracker.track(&mut alerts);
    let first = alerts[0].first_seen.clone().unwrap();
    let last = alerts[0].last_seen.clone().unwrap();
    assert_eq!(first, last);
    assert_eq!(tracker.active_count(), 1);
}

#[test]
fn tracker_updates_last_seen_preserves_first_seen() {
    let tracker = AlertTracker::new();
    let snap = snapshot_with_pipeline("pipe1", "off");

    let mut alerts1 = derive_alerts(&snap);
    tracker.track(&mut alerts1);
    let first = alerts1[0].first_seen.clone().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(10));

    let mut alerts2 = derive_alerts(&snap);
    tracker.track(&mut alerts2);
    assert_eq!(alerts2[0].first_seen.as_ref().unwrap(), &first);
    assert_ne!(alerts2[0].last_seen.as_ref().unwrap(), &first);
}

#[test]
fn saturated_srt_receive_buffer_yields_input_causal_alert() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {
            "pipe-srt": {
                "input": {
                    "status": "on",
                    "readerMetrics": [],
                    "publisher": {
                        "protocol": "srt",
                        "quality": {
                            "srtRecvBufBytes": 8_218_796,
                            "srtRecvBufAvailBytes": 1_500
                        }
                    }
                },
                "outputs": {
                    "out-a": { "status": "stalled" }
                }
            }
        }
    });

    let alerts = derive_alerts(&snap);
    let alert = alerts
        .iter()
        .find(|alert| alert.id == "pipeline:pipe-srt:input:srt_recv_buffer_saturated")
        .expect("saturated ingest buffer should produce a causal input alert");

    assert_eq!(alert.severity, Severity::Critical);
    assert_eq!(alert.scope, Scope::Pipeline);
    assert!(alert.title.contains("SRT publisher ingest"));
    assert!(alert.cause.contains("not draining ingest data"));
    assert!(alert.evidence.iter().any(|line| line.contains("100%")));
}

#[test]
fn tracker_prunes_resolved_alerts() {
    let tracker = AlertTracker::new();

    let snap_off = snapshot_with_pipeline("pipe1", "off");
    let mut alerts = derive_alerts(&snap_off);
    tracker.track(&mut alerts);
    assert_eq!(tracker.active_count(), 1);

    let snap_on = snapshot_with_pipeline("pipe1", "on");
    let mut alerts = derive_alerts(&snap_on);
    assert!(alerts.is_empty());
    tracker.track(&mut alerts);
    assert_eq!(tracker.active_count(), 0);
}

#[test]
fn tracker_pipeline_scope_does_not_prune_other_pipelines() {
    let tracker = AlertTracker::new();
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {
            "pipe-a": {
                "input": { "status": "off", "readerMetrics": [] },
                "outputs": {}
            },
            "pipe-b": {
                "input": { "status": "off", "readerMetrics": [] },
                "outputs": {}
            }
        }
    });

    let mut all_alerts = derive_alerts(&snap);
    tracker.track(&mut all_alerts);
    assert_eq!(tracker.active_count(), 2);

    let pipe_a = snapshot_with_pipeline("pipe-a", "off");
    let mut pipe_a_alerts = derive_alerts(&pipe_a);
    tracker.track_pipeline("pipe-a", &mut pipe_a_alerts);

    assert_eq!(tracker.active_count(), 2);
    assert_eq!(
        pipe_a_alerts[0].first_seen,
        all_alerts
            .iter()
            .find(|alert| alert.pipeline_id.as_deref() == Some("pipe-a"))
            .and_then(|alert| alert.first_seen.clone())
    );
}

#[test]
fn stage_failed_phase_yields_warning_alert() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {},
        "stages": {
            "pipe1:video_preset(720p)": {
                "phase": "failed",
                "lastError": "FFmpeg process exited with code 1"
            }
        }
    });
    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Warning);
    assert_eq!(alerts[0].scope, Scope::Stage);
    assert_eq!(alerts[0].pipeline_id.as_deref(), Some("pipe1"));
    assert_eq!(alerts[0].stage_id.as_deref(), Some("video_preset(720p)"));
    assert!(alerts[0].id.contains("failed"));
    assert!(alerts[0].cause.contains("exited with code 1"));
}

#[test]
fn stage_waiting_for_capacity_or_high_wait_yields_warning_alert() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {},
        "stages": {
            "pipe2:video_preset(1080p)": {
                "phase": {
                    "phase": "waitingForCapacity",
                    "backend": "externalFfmpeg"
                },
                "capacityWaitMs": 6000
            }
        }
    });
    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Warning);
    assert_eq!(alerts[0].scope, Scope::Stage);
    assert_eq!(alerts[0].pipeline_id.as_deref(), Some("pipe2"));
    assert_eq!(alerts[0].stage_id.as_deref(), Some("video_preset(1080p)"));
    assert!(alerts[0].id.contains("capacity_exhausted"));
}

#[test]
fn stage_receiving_input_without_output_yields_warning_alert() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {},
        "stages": {
            "pipe2:video:720p": {
                "phase": "runningNoOutputYet",
                "bytesIn": 4096,
                "bytesOut": 0,
                "packetsIn": 4,
                "packetsOut": 0
            }
        }
    });

    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].scope, Scope::Stage);
    assert!(alerts[0].id.contains("no_output"));
    assert!(
        alerts[0]
            .evidence
            .iter()
            .any(|evidence| evidence == "packetsIn = 4")
    );
}

#[test]
fn hls_preview_waiting_for_keyframe_yields_warning_alert() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {},
        "stages": {
            "pipe2:preview:low:from:source": {
                "phase": {
                    "phase": "waitingForKeyframe"
                }
            }
        }
    });

    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].scope, Scope::Stage);
    assert!(alerts[0].id.contains("waiting_for_keyframe"));
    assert!(alerts[0].cause.contains("keyframe"));
}

#[test]
fn stage_alerts_are_derived_without_pipeline_object() {
    let snap = json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "stages": {
            "pipe3:video:720p": {
                "phase": "failed",
                "lastError": "synthetic stage failure"
            }
        }
    });

    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].id, "pipeline:pipe3:stage:video:720p:failed");
    assert_eq!(alerts[0].pipeline_id.as_deref(), Some("pipe3"));
    assert_eq!(alerts[0].stage_id.as_deref(), Some("video:720p"));
}

fn snapshot_with_fabric_shard(shard: serde_json::Value) -> serde_json::Value {
    json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "pipelines": {},
        "egressFabricShards": [shard],
    })
}

#[test]
fn healthy_fabric_shard_yields_no_alert() {
    let snap = snapshot_with_fabric_shard(json!({
        "protocol": "rtmp",
        "feedId": "feed-1",
        "shardIndex": 0,
        "state": "healthy",
        "progressAgeMs": 5,
        "commandDepth": 1,
        "commandCapacity": 1024,
    }));
    assert!(derive_alerts(&snap).is_empty());
}

#[test]
fn stalled_fabric_shard_yields_warning_alert() {
    let snap = snapshot_with_fabric_shard(json!({
        "protocol": "srt",
        "feedId": "feed-2",
        "shardIndex": 3,
        "state": "stalled",
        "progressAgeMs": 30_000,
        "commandDepth": 0,
        "commandCapacity": 1024,
    }));

    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Warning);
    assert_eq!(alerts[0].scope, Scope::Engine);
    assert_eq!(alerts[0].id, "engine:egress_fabric:srt:feed-2:3:stalled");
    assert!(alerts[0].evidence.iter().any(|e| e.contains("30000")));
}

#[test]
fn panicked_fabric_shard_yields_critical_alert() {
    let snap = snapshot_with_fabric_shard(json!({
        "protocol": "rtmp",
        "feedId": "feed-3",
        "shardIndex": 1,
        "state": "panicked",
        "commandDepth": 0,
        "commandCapacity": 1024,
    }));

    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Critical);
    assert_eq!(alerts[0].id, "engine:egress_fabric:rtmp:feed-3:1:panicked");
}

#[test]
fn fabric_shard_command_channel_near_capacity_yields_warning_alert() {
    let snap = snapshot_with_fabric_shard(json!({
        "protocol": "sink",
        "feedId": "feed-4",
        "shardIndex": 2,
        "state": "healthy",
        "progressAgeMs": 5,
        "commandDepth": 900,
        "commandCapacity": 1000,
    }));

    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Warning);
    assert_eq!(
        alerts[0].id,
        "engine:egress_fabric:sink:feed-4:2:command_overload"
    );
    assert!(alerts[0].evidence.iter().any(|e| e.contains("90.0%")));
}

#[test]
fn fabric_shard_command_channel_below_threshold_yields_no_alert() {
    let snap = snapshot_with_fabric_shard(json!({
        "protocol": "pipeline",
        "feedId": "feed-5",
        "shardIndex": 0,
        "state": "healthy",
        "progressAgeMs": 5,
        "commandDepth": 100,
        "commandCapacity": 1000,
    }));
    assert!(derive_alerts(&snap).is_empty());
}

fn snapshot_with_retrying_output(
    output_max_retries: u64,
    retry_attempts: u64,
) -> serde_json::Value {
    json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "udpDrops": 0 },
        "tuning": { "outputMaxRetries": output_max_retries },
        "pipelines": {
            "pipe1": {
                "input": { "status": "on", "readerMetrics": [] },
                "outputs": {
                    "out1": {
                        "status": "retrying",
                        "retryAttempts": retry_attempts,
                        "retryBackoffMs": 5_000,
                    }
                }
            }
        }
    })
}

#[test]
fn retry_attempts_near_ceiling_yields_retry_admission_alert() {
    let snap = snapshot_with_retrying_output(10, 8);

    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, Severity::Warning);
    assert_eq!(alerts[0].scope, Scope::Output);
    assert_eq!(
        alerts[0].id,
        "pipeline:pipe1:output:out1:retry_admission_saturation"
    );
    assert!(alerts[0].evidence.iter().any(|e| e.contains("8")));
}

#[test]
fn retry_attempts_below_ceiling_yields_generic_not_running_alert() {
    let snap = snapshot_with_retrying_output(10, 2);

    let alerts = derive_alerts(&snap);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].id, "pipeline:pipe1:output:out1:not_running");
}
