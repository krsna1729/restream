use std::path::Path;

use sysinfo::{Disks, System};

use super::super::{
    state::AppState,
    telemetry::{process_resource_snapshot, system_status},
};
use super::{agent_health_snapshot, pipeline_input_is_on};
use crate::api_runtime_views::ResourceMapOptions;
use crate::application::models::{Ingest, Job, Output, Pipeline};
use crate::domain::{output_spec::OutputUrlScheme, state::DesiredOutputState};
use crate::{alerts, api_view_models, events};

pub(super) async fn build_agent_context(state: &AppState) -> serde_json::Value {
    let catalog = state.agent_context_catalog().await;
    let pipelines = catalog.pipelines;
    let pipeline_ids = pipelines
        .iter()
        .map(|pipeline| pipeline.id.clone())
        .collect::<Vec<_>>();
    let outputs = catalog.outputs;
    let jobs = catalog.jobs;
    let jobs_json = api_view_models::job_response_json_list(&jobs);
    let ingests = catalog.ingests;
    let (recording_enabled, health) = agent_health_snapshot(state, &pipeline_ids).await;
    let alerts = alerts::derive_alerts(&health);
    let events = state.engine.recent_events(events::MAX_EVENTS, None);
    let engine_telemetry = crate::api_runtime_views::engine_telemetry(&state.engine).await;
    let system = System::new_all();
    let resource_map = crate::api_runtime_views::resource_map(
        &state.engine,
        process_resource_snapshot(&system),
        None,
        ResourceMapOptions::summary(),
    )
    .await;
    let mut pipeline_telemetry = Vec::new();
    let mut graphs = Vec::new();
    for pipeline_id in &pipeline_ids {
        pipeline_telemetry
            .push(crate::api_runtime_views::pipeline_telemetry(&state.engine, pipeline_id).await);
        graphs.push(
            crate::api_runtime_views::processing_graph(&state.engine, pipeline_id, &outputs).await,
        );
    }
    let desired_vs_actual = desired_vs_actual(
        &pipelines,
        &outputs,
        &ingests,
        &jobs,
        &recording_enabled,
        &health,
    );
    let diagnostics = diagnostics_summary(&pipelines, &outputs, &health, &graphs);
    let dependencies = dependency_summary(
        state,
        &pipelines,
        &outputs,
        &ingests,
        &recording_enabled,
        &health,
    )
    .await;

    let bonding_available = state.engine.bonding_available();
    let (mut status, _) = crate::runtime_info::status_and_sbom(bonding_available);
    status["os"] = system_status(&system);

    let settings = catalog.settings;
    let custom_encoding_len = catalog.custom_encoding_len;
    let configuration = serde_json::json!({
        "serverName": settings
            .as_ref()
            .map(|settings| settings.server_name.clone())
            .unwrap_or_else(|| "Name".to_string()),
        "ingestHost": settings
            .as_ref()
            .map(|settings| settings.ingest_host.clone())
            .unwrap_or_default(),
        "ingestSecurity": settings
            .as_ref()
            .map(|settings| settings.ingest_security.clone())
            .unwrap_or_else(|| state.ingest_security_config()),
        "transcodeProfiles": settings
            .as_ref()
            .map(|settings| settings.transcode_profiles.clone())
            .unwrap_or_else(crate::application::transcode_profiles::default_transcode_profiles),
        "customEncoding": {
            "configured": custom_encoding_len > 0,
            "byteLength": custom_encoding_len,
        },
        "ports": {
            "rtmp": state.ports.rtmp,
            "srt": state.ports.srt,
        }
    });
    let media = media_inventory(state).await;
    let storage = storage_summary(state, &media).await;

    crate::agent_plane::redacted_context(
        &pipelines,
        &outputs,
        &jobs_json,
        &ingests,
        status,
        health,
        engine_telemetry,
        pipeline_telemetry,
        resource_map,
        graphs,
        alerts,
        events,
        configuration,
        media,
        desired_vs_actual,
        diagnostics,
        dependencies,
        storage,
    )
}

async fn media_inventory(state: &AppState) -> serde_json::Value {
    let files = state
        .media_library_service
        .list_media_files(&state.media_dir)
        .await;
    serde_json::json!({
        "mediaDir": state.media_dir,
        "files": files,
    })
}

fn desired_output_reason(
    desired_state: DesiredOutputState,
    actual_status: &str,
    input_is_on: bool,
) -> &'static str {
    if desired_state == DesiredOutputState::Running && !input_is_on {
        "pendingInput"
    } else if (desired_state == DesiredOutputState::Running && actual_status == "running")
        || (desired_state == DesiredOutputState::Stopped && actual_status != "running")
    {
        "converged"
    } else {
        "desiredActualMismatch"
    }
}

fn desired_vs_actual(
    pipelines: &[Pipeline],
    outputs: &[Output],
    ingests: &[Ingest],
    jobs: &[Job],
    recording_enabled: &std::collections::HashMap<String, bool>,
    health: &serde_json::Value,
) -> serde_json::Value {
    let mut pipeline_reports = Vec::new();
    let mut drift_count = 0usize;
    let mut converged_count = 0usize;
    let mut pending_count = 0usize;

    for pipeline in pipelines {
        let pipeline_health = &health["pipelines"][&pipeline.id];
        let input_status = pipeline_health["input"]["status"].as_str().unwrap_or("off");
        let input_is_on = pipeline_input_is_on(pipeline_health);
        let file_ingests = ingests
            .iter()
            .filter(|ingest| ingest.stream_key == pipeline.stream_key)
            .collect::<Vec<_>>();
        let input_desired = if file_ingests.is_empty() {
            "externalPublisherOptional"
        } else {
            "fileIngestConfigured"
        };
        let mut output_reports = Vec::new();
        for output in outputs
            .iter()
            .filter(|output| output.pipeline_id == pipeline.id)
        {
            let runtime = &pipeline_health["outputs"][&output.id];
            let actual = runtime["status"].as_str().unwrap_or("stopped");
            let reason = desired_output_reason(output.desired_state, actual, input_is_on);
            match reason {
                "pendingInput" => pending_count += 1,
                "converged" => converged_count += 1,
                _ => drift_count += 1,
            }
            let recent_jobs = jobs
                .iter()
                .filter(|job| job.pipeline_id == pipeline.id && job.output_id == output.id)
                .take(5)
                .map(|job| {
                    crate::agent_plane::redact_secrets_from_serializable(
                        &api_view_models::job_response_json(job),
                    )
                })
                .collect::<Vec<_>>();
            output_reports.push(serde_json::json!({
                "outputId": output.id,
                "name": output.name,
                "desiredState": output.desired_state,
                "actualStatus": actual,
                "actualPhase": runtime["phase"],
                "converged": reason == "converged",
                "reason": reason,
                "config": output.config,
                "recentJobs": recent_jobs,
            }));
        }

        let recording_desired = recording_enabled
            .get(&pipeline.id)
            .copied()
            .unwrap_or(false);
        let recording_active = pipeline_health["recording"]["active"]
            .as_bool()
            .unwrap_or(false);
        let recording_reason = if recording_desired == recording_active {
            "converged"
        } else if recording_desired && input_status != "on" {
            "pendingInput"
        } else {
            "desiredActualMismatch"
        };
        pipeline_reports.push(serde_json::json!({
            "pipelineId": pipeline.id,
            "name": pipeline.name,
            "input": {
                "desired": input_desired,
                "actualStatus": input_status,
                "fileIngestCount": file_ingests.len(),
                "externalPublishersAllowed": true
            },
            "outputs": output_reports,
            "recording": {
                "desiredEnabled": recording_desired,
                "actualActive": recording_active,
                "converged": recording_reason == "converged",
                "reason": recording_reason
            },
            "hlsPreview": {
                "desired": "onDemand",
                "actualActive": pipeline_health["hlsPreview"]["active"].as_bool().unwrap_or(false)
            }
        }));
    }

    serde_json::json!({
        "summary": {
            "pipelines": pipelines.len(),
            "outputs": outputs.len(),
            "convergedOutputs": converged_count,
            "pendingOutputs": pending_count,
            "driftedOutputs": drift_count,
        },
        "pipelines": pipeline_reports,
    })
}

fn diagnostics_summary(
    pipelines: &[Pipeline],
    outputs: &[Output],
    health: &serde_json::Value,
    graphs: &[serde_json::Value],
) -> serde_json::Value {
    let pipeline_reports = pipelines
        .iter()
        .map(|pipeline| {
            let pipeline_health = &health["pipelines"][&pipeline.id];
            let graph = graphs
                .iter()
                .find(|graph| graph["pipelineId"].as_str() == Some(pipeline.id.as_str()));
            let inactive_nodes = graph
                .and_then(|graph| graph["nodes"].as_array())
                .map(|nodes| {
                    nodes
                        .iter()
                        .filter(|node| !node["active"].as_bool().unwrap_or(false))
                        .map(|node| {
                            serde_json::json!({
                                "id": node["id"],
                                "type": node["type"],
                                "label": node["label"],
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let desired_running_outputs = outputs
                .iter()
                .filter(|output| {
                    output.pipeline_id == pipeline.id
                        && output.desired_state == DesiredOutputState::Running
                })
                .count();
            let actual_running_outputs = pipeline_health["outputs"]
                .as_object()
                .map(|outputs| {
                    outputs
                        .values()
                        .filter(|output| output["status"].as_str() == Some("running"))
                        .count()
                })
                .unwrap_or(0);
            let mut findings = Vec::new();
            if pipeline_health["input"]["status"].as_str() != Some("on") {
                findings.push(serde_json::json!({
                    "severity": "critical",
                    "code": "noActivePublisher",
                    "message": "Pipeline has no active publisher."
                }));
            }
            if actual_running_outputs < desired_running_outputs {
                findings.push(serde_json::json!({
                    "severity": "warning",
                    "code": "desiredOutputsNotRunning",
                    "message": "One or more desired running outputs are not active.",
                    "desiredRunningOutputs": desired_running_outputs,
                    "actualRunningOutputs": actual_running_outputs
                }));
            }
            serde_json::json!({
                "pipelineId": pipeline.id,
                "passive": true,
                "activeProbeEndpoint": format!(
                    "/api/v1/pipelines/{}/diagnostics/run",
                    pipeline.id
                ),
                "activeProbeMethod": "POST",
                "includedActiveProbeResults": false,
                "reason": "The context endpoint is read-only and does not run active diagnostics checks.",
                "inactiveGraphNodes": inactive_nodes,
                "findings": findings,
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "activeProbeEndpointTemplate": "/api/v1/pipelines/:pipeline_id/diagnostics/run",
        "activeProbeMethod": "POST",
        "includedActiveProbeResults": false,
        "pipelines": pipeline_reports,
    })
}

async fn dependency_summary(
    state: &AppState,
    pipelines: &[Pipeline],
    outputs: &[Output],
    ingests: &[Ingest],
    recording_enabled: &std::collections::HashMap<String, bool>,
    health: &serde_json::Value,
) -> serde_json::Value {
    let mut hls = Vec::new();
    let mut recordings = Vec::new();
    for pipeline in pipelines {
        let snapshot = state.engine.hls_dependency_snapshot(&pipeline.id).await;
        hls.push(serde_json::json!({
            "pipelineId": pipeline.id,
            "storeExists": snapshot.store_exists,
            "active": snapshot.active,
            "persistentConsumers": snapshot.persistent_consumers,
            "lastAccessAgeMs": snapshot.last_access_age_ms,
            "segments": snapshot.segments,
            "playlistBytes": snapshot.playlist_bytes,
        }));
        let desired_enabled = recording_enabled
            .get(&pipeline.id)
            .copied()
            .unwrap_or(false);
        recordings.push(serde_json::json!({
            "pipelineId": pipeline.id,
            "desiredEnabled": desired_enabled,
            "active": state.engine.is_recording_active(&pipeline.id).await,
            "inputStatus": health["pipelines"][&pipeline.id]["input"]["status"],
        }));
    }

    let file_ingest_backend = if state.engine.config.use_internal_file_ingest {
        "internal"
    } else {
        "ffmpeg-subprocess"
    };
    let mut file_ingest = Vec::new();
    for ingest in ingests {
        let media_path = Path::new(&state.media_dir).join(&ingest.filename);
        let runtime = state
            .engine
            .file_ingest_dependency_snapshot(&ingest.id)
            .await;
        file_ingest.push(serde_json::json!({
            "id": ingest.id,
            "filename": ingest.filename,
            "mediaExists": media_path.exists(),
            "markedActive": runtime.marked_active,
            "childRegistered": runtime.child_registered,
            "backend": file_ingest_backend,
            "loop": ingest.loop_flag,
            "startTime": ingest.start_time,
            "liveOptimized": ingest.live_optimized,
            "targetGopSeconds": ingest.target_gop_seconds,
            "streamKey": ingest.stream_key,
        }));
    }
    let hls_output_count = outputs
        .iter()
        .filter(|output| OutputUrlScheme::from_url(&output.url).is_hls_family())
        .count();

    serde_json::json!({
        "hls": {
            "config": {
                "minSegmentSecs": state.engine.config.hls_min_segment_ms,
                "segmentCapacity": state.engine.config.hls_segment_capacity_bytes,
                "maxSegments": state.engine.config.hls_max_segments,
            },
            "outputCount": hls_output_count,
            "pipelines": hls,
        },
        "recording": { "pipelines": recordings },
        "fileIngest": {
            "configured": file_ingest.len(),
            "backend": file_ingest_backend,
            "ingests": file_ingest,
        },
        "ingestSecurity": {
            "config": state.ingest_security_config(),
            "loopbackExempt": true,
            "trackedIpRuntimeStateRedacted": true,
        }
    })
}

async fn storage_summary(state: &AppState, media: &serde_json::Value) -> serde_json::Value {
    let media_bytes = media["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|file| file["size"].as_u64())
                .sum::<u64>()
        })
        .unwrap_or(0);
    let media_file_count = media["files"]
        .as_array()
        .map(|files| files.len())
        .unwrap_or(0);
    let media_root = std::fs::canonicalize(&state.media_dir)
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| state.media_dir.clone());
    let disks = Disks::new_with_refreshed_list();
    let selected_disk = disks
        .list()
        .iter()
        .rfind(|disk| Path::new(&media_root).starts_with(disk.mount_point()))
        .map(|disk| {
            serde_json::json!({
                "mountPoint": disk.mount_point().display().to_string(),
                "totalBytes": disk.total_space(),
                "availableBytes": disk.available_space(),
            })
        });

    serde_json::json!({
        "mediaDir": state.media_dir,
        "mediaRoot": media_root,
        "mediaFileCount": media_file_count,
        "mediaBytes": media_bytes,
        "disk": selected_disk,
        "databasePath": state.db_path,
    })
}

#[cfg(test)]
mod tests {
    use super::desired_output_reason;
    use crate::domain::state::DesiredOutputState;

    #[test]
    fn desired_output_reason_distinguishes_converged_pending_and_drifted() {
        assert_eq!(
            desired_output_reason(DesiredOutputState::Running, "running", true),
            "converged"
        );
        assert_eq!(
            desired_output_reason(DesiredOutputState::Running, "stopped", false),
            "pendingInput"
        );
        assert_eq!(
            desired_output_reason(DesiredOutputState::Stopped, "running", true),
            "desiredActualMismatch"
        );
    }
}
