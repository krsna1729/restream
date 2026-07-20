//! Telemetry HTTP handlers expose runtime health, metrics, diagnostics, and
//! event snapshots. This module keeps query-to-view selection and transport
//! policy close to the API boundary while delegating heavy lifting to runtime
//! view builders and the diagnostics engine.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use sysinfo::System;

use crate::alerts;
use crate::api_runtime_views::{ResourceMapOptions, ResourceMapView};
use crate::diag;
use crate::events;

use super::health::{
    build_health_snapshot_for_pipeline_ids, build_health_summary_snapshot_for_pipeline_ids,
    list_dashboard_runtime_pipeline_ids, merge_dashboard_runtime_focus_pipeline,
    select_dashboard_runtime_pipeline_ids,
};
use super::state::{
    AppState, get_session_token_from_headers, recording_enabled_map, require_authenticated,
};

mod system;

pub(crate) use system::process_resource_snapshot;
pub use system::{
    build_system_metrics_snapshot, cpu_status, detect_hypervisor_vendor, engine_metrics,
    engine_process_pids, proc_process_ticks, proc_total_ticks, read_cpuinfo_summary,
    read_trimmed_file, selected_cpu_flags, system_status,
};

pub fn default_events_limit() -> usize {
    100
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    pub pipeline_id: Option<String>,
    #[serde(default = "default_events_limit")]
    pub limit: usize,
}

#[derive(Deserialize)]
pub struct DashboardRuntimeQuery {
    pub health_view: Option<String>,
    pub metrics_view: Option<String>,
    pub pipeline_id: Option<String>,
}

#[derive(Deserialize)]
pub struct MetricsSystemQuery {
    pub view: Option<String>,
}

#[derive(Deserialize)]
pub struct ResourceMapQuery {
    pub pipeline_id: Option<String>,
    pub view: Option<String>,
    pub top_n: Option<usize>,
}

impl ResourceMapQuery {
    // Invalid or missing view names intentionally fall back to the grouped
    // presentation so the telemetry surface stays permissive for dashboards.
    fn options(&self) -> ResourceMapOptions {
        let view = match self.view.as_deref() {
            Some("summary") => ResourceMapView::Summary,
            Some("detail") => ResourceMapView::Detail,
            Some("grouped") | None => ResourceMapView::Grouped,
            Some(_) => ResourceMapView::Grouped,
        };
        ResourceMapOptions::new(view, self.top_n)
    }
}

fn unauthorized_json_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": "Unauthorized" })),
    )
        .into_response()
}

fn summary_view_requested(view: Option<&str>) -> bool {
    view == Some("summary")
}

fn snapshot_generated_at(snapshot: &serde_json::Value) -> String {
    snapshot["generatedAt"].as_str().unwrap_or("").to_string()
}

fn dashboard_runtime_requested_pipeline<'a>(
    query: &'a DashboardRuntimeQuery,
    all_pipeline_ids: &'a [String],
) -> Option<&'a str> {
    query.pipeline_id.as_deref().filter(|pipeline_id| {
        all_pipeline_ids
            .iter()
            .any(|candidate| candidate == *pipeline_id)
    })
}

fn configured_media_root(media_dir: &str) -> PathBuf {
    let configured = PathBuf::from(media_dir);
    if configured.is_absolute() {
        configured
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(configured)
    }
}

pub fn expected_media_path(media_dir: &str, filename: &str) -> PathBuf {
    configured_media_root(media_dir).join(filename)
}

/// Builds the diagnostics context for one file-backed ingest, including a
/// blocking media-file probe when the expected library file exists.
pub async fn build_file_diagnostics_context(
    state: &AppState,
    pipeline_id: &str,
) -> Option<diag::FileDiagnosticsContext> {
    let pipeline = state.pipeline_service.get_by_id(pipeline_id).await.ok()?;
    let ingest = state
        .file_ingest_service
        .load_pipeline_file_ingest_state(&state.engine, &pipeline)
        .await
        .ok()?
        .ingest?;
    let path = expected_media_path(&state.media_dir, &ingest.filename);
    let metadata = std::fs::metadata(&path).ok();
    let file_exists = metadata.is_some();
    let file_size_bytes = metadata.as_ref().map(std::fs::Metadata::len);
    let file_modified_at = metadata
        .as_ref()
        .and_then(|meta| meta.modified().ok())
        .map(|timestamp| chrono::DateTime::<chrono::Utc>::from(timestamp).to_rfc3339());

    let (analysis, analysis_error) = if file_exists {
        match state
            .media_library_service
            .analyze_media_file(path.clone())
            .await
        {
            Ok(analysis) => (Some(analysis), None),
            Err(error) => (None, Some(error)),
        }
    } else {
        (None, None)
    };

    Some(diag::FileDiagnosticsContext {
        ingest_id: ingest.id,
        filename: ingest.filename,
        path,
        file_exists,
        file_size_bytes,
        file_modified_at,
        loop_enabled: ingest.loop_flag,
        start_time: ingest.start_time,
        live_optimized: ingest.live_optimized,
        target_gop_seconds: ingest.target_gop_seconds,
        analysis,
        analysis_error,
    })
}

pub async fn pipeline_diagnostics_run_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return unauthorized_json_response();
        }
    } else {
        // Diagnostics runs are API-only callers, so auth failures must stay on
        // the JSON contract instead of inheriting any HTML/session redirect flow.
        return unauthorized_json_response();
    }

    let probe_protocol = match state
        .engine
        .active_ingest_protocol_for_probe(&pipeline_id)
        .await
    {
        Some(protocol) => protocol,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "No active ingest for this pipeline"
                })),
            )
                .into_response();
        }
    };

    let engine = state.engine.clone();
    let sem = engine.get_or_create_diag_semaphore(&pipeline_id).await;
    let permit = match sem.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "A diagnostic is already running for this pipeline"
                })),
            )
                .into_response();
        }
    };

    // The owned task keeps the permit through blocking file analysis even when
    // the HTTP client disconnects. Browser abort suppresses stale UI, while the
    // server batch intentionally runs to completion before another run may start.
    let run_state = state.clone();
    let report = match tokio::spawn(async move {
        let _permit = permit;
        let file_context = if probe_protocol == "file" {
            build_file_diagnostics_context(&run_state, &pipeline_id).await
        } else {
            None
        };

        // These checks form one short batch. Restore streaming only if genuinely
        // progressive, multi-second probes return.
        diag::run_diagnostics(
            engine,
            pipeline_id,
            probe_protocol,
            run_state.media_dir.clone(),
            file_context,
        )
        .await
    })
    .await
    {
        Ok(report) => report,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Diagnostics task failed: {error}")
                })),
            )
                .into_response();
        }
    };

    (StatusCode::OK, Json(report)).into_response()
}

pub async fn status_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let sys = System::new_all();
    let bonding_available = state.engine.bonding_available();
    let (mut status, _) = crate::runtime_info::status_and_sbom(bonding_available);
    status["os"] = system_status(&sys);

    Json(status).into_response()
}

pub async fn v1_dashboard_runtime_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<DashboardRuntimeQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let summary_health = summary_view_requested(query.health_view.as_deref());
    let summary_metrics = summary_view_requested(query.metrics_view.as_deref());
    let all_pipeline_ids = match list_dashboard_runtime_pipeline_ids(&state).await {
        Ok(pipeline_ids) => pipeline_ids,
        Err(error) => return error.into_response(),
    };
    let requested_pipeline_id = dashboard_runtime_requested_pipeline(&query, &all_pipeline_ids);
    let health_pipeline_ids = select_dashboard_runtime_pipeline_ids(
        requested_pipeline_id,
        summary_health,
        all_pipeline_ids.clone(),
    );
    let (health, metrics) = tokio::join!(
        async {
            if summary_health {
                let mut health =
                    build_health_summary_snapshot_for_pipeline_ids(&state, &health_pipeline_ids)
                        .await;
                if let Some(pipeline_id) = requested_pipeline_id {
                    let focused_health =
                        build_health_snapshot_for_pipeline_ids(&state, &[pipeline_id.to_string()])
                            .await;
                    merge_dashboard_runtime_focus_pipeline(
                        &mut health,
                        &focused_health,
                        pipeline_id,
                    );
                }
                health
            } else {
                build_health_snapshot_for_pipeline_ids(&state, &health_pipeline_ids).await
            }
        },
        build_system_metrics_snapshot(&state, summary_metrics)
    );

    Json(serde_json::json!({
        "health": health,
        "metrics": metrics,
    }))
    .into_response()
}

pub async fn status_sbom_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let bonding_available = state.engine.bonding_available();
    let (_, sbom) = crate::runtime_info::status_and_sbom(bonding_available);
    (
        [
            (
                header::CONTENT_TYPE,
                "application/vnd.cyclonedx+json; version=1.5",
            ),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"restream-sbom.cdx.json\"",
            ),
        ],
        Json(sbom),
    )
        .into_response()
}

pub async fn v1_engine_resource_map_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<ResourceMapQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let sys = System::new_all();
    let process = process_resource_snapshot(&sys);
    let snapshot = crate::api_runtime_views::resource_map(
        &state.engine,
        process,
        query.pipeline_id.as_deref(),
        query.options(),
    )
    .await;
    Json(snapshot).into_response()
}

/// Returns the system metrics snapshot directly, using the query only to choose
/// between the summary and full transport views.
pub async fn metrics_system_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<MetricsSystemQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let response =
        build_system_metrics_snapshot(&state, summary_view_requested(query.view.as_deref())).await;
    Json(response).into_response()
}

pub async fn v1_events_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<EventsQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let limit = query.limit.min(events::MAX_EVENTS);
    let pipeline_filter = query.pipeline_id.as_deref();
    let event_list = state.engine.recent_events(limit, pipeline_filter);

    Json(serde_json::json!({
        "generatedAt": chrono::Utc::now().to_rfc3339(),
        "count": event_list.len(),
        "events": event_list,
    }))
    .into_response()
}

pub async fn v1_overview_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let pipelines = state
        .pipeline_service
        .list_pipelines()
        .await
        .unwrap_or_default();
    let pipeline_ids: Vec<String> = pipelines.iter().map(|p| p.id.clone()).collect();
    let recording_enabled = recording_enabled_map(&state, &pipeline_ids).await;
    let snapshot = crate::api_runtime_views::health_snapshot(
        &state.engine,
        &pipeline_ids,
        &recording_enabled,
        0,
    )
    .await;

    let alert_list = alerts::derive_alerts(&snapshot);
    let critical = alert_list
        .iter()
        .filter(|a| matches!(a.severity, alerts::Severity::Critical))
        .count();
    let warning = alert_list
        .iter()
        .filter(|a| matches!(a.severity, alerts::Severity::Warning))
        .count();

    let snap_pipelines = snapshot["pipelines"].as_object();

    let total = pipeline_ids.len();
    let mut active = 0usize;
    let mut degraded = 0usize;
    let mut failed_outputs = 0usize;

    if let Some(pip_map) = snap_pipelines {
        for (pip_id, pip) in pip_map {
            let is_live = pip["input"]["status"].as_str() == Some("on");
            if is_live {
                active += 1;
            }
            let has_alerts = alert_list
                .iter()
                .any(|a| a.pipeline_id.as_deref() == Some(pip_id.as_str()));
            if has_alerts {
                degraded += 1;
            }
            if is_live && let Some(outputs) = pip["outputs"].as_object() {
                for output in outputs.values() {
                    if output["status"].as_str().unwrap_or("") != "running" {
                        failed_outputs += 1;
                    }
                }
            }
        }
    }

    Json(serde_json::json!({
        "generatedAt": snapshot_generated_at(&snapshot),
        "totalPipelines": total,
        "activePipelines": active,
        "degradedPipelines": degraded,
        "failedOutputs": failed_outputs,
        "alertCount": { "critical": critical, "warning": warning },
        "srtListener": snapshot["srtListener"],
    }))
    .into_response()
}

pub async fn v1_engine_telemetry_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    Json(crate::api_runtime_views::engine_telemetry(&state.engine).await).into_response()
}

pub async fn v1_pipeline_telemetry_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    Json(crate::api_runtime_views::pipeline_telemetry(&state.engine, &pipeline_id).await)
        .into_response()
}

pub async fn v1_stage_telemetry_handler(
    State(state): State<Arc<AppState>>,
    Path(stage_key): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    match crate::api_runtime_views::stage_telemetry_by_display(&state.engine, &stage_key).await {
        Some(val) => Json(val).into_response(),
        None => (StatusCode::NOT_FOUND, "Stage not found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DashboardRuntimeQuery, ResourceMapQuery, configured_media_root,
        dashboard_runtime_requested_pipeline, snapshot_generated_at, summary_view_requested,
        unauthorized_json_response,
    };
    use crate::api_runtime_views::ResourceMapView;
    use axum::http::StatusCode;

    #[test]
    fn resource_map_query_defaults_to_grouped_view() {
        let query = ResourceMapQuery {
            pipeline_id: None,
            view: None,
            top_n: None,
        };

        assert!(matches!(query.options().view, ResourceMapView::Grouped));
    }

    #[test]
    fn dashboard_runtime_requested_pipeline_requires_known_pipeline() {
        let query = DashboardRuntimeQuery {
            health_view: None,
            metrics_view: None,
            pipeline_id: Some("pipe-2".to_string()),
        };
        let ids = vec!["pipe-1".to_string()];

        assert_eq!(dashboard_runtime_requested_pipeline(&query, &ids), None);
    }

    #[test]
    fn diagnostics_unauthorized_response_uses_json_contract() {
        assert_eq!(
            unauthorized_json_response().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn snapshot_generated_at_falls_back_to_empty_string() {
        assert_eq!(snapshot_generated_at(&serde_json::json!({})), "");
        assert_eq!(
            snapshot_generated_at(&serde_json::json!({"generatedAt": "2026-07-14T00:00:00Z"})),
            "2026-07-14T00:00:00Z"
        );
    }

    #[test]
    fn summary_view_requested_only_matches_summary() {
        assert!(summary_view_requested(Some("summary")));
        assert!(!summary_view_requested(Some("detail")));
        assert!(!summary_view_requested(None));
    }

    #[test]
    fn configured_media_root_makes_relative_paths_absolute() {
        assert!(configured_media_root("media").is_absolute());
        assert_eq!(
            configured_media_root("/tmp/media"),
            std::path::PathBuf::from("/tmp/media")
        );
    }
}
