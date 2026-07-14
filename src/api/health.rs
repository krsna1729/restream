//! Health HTTP handlers build runtime snapshots for dashboard and operator
//! views. They stay close to the transport layer because they choose which
//! pipeline set and snapshot shape should be exposed for each request.

use axum::{Json, extract::State, http::HeaderMap, response::IntoResponse};
use serde::Deserialize;
use std::sync::Arc;

use super::state::{AppState, recording_enabled_map, require_authenticated};
use crate::application::services::ApiError;

#[derive(Deserialize)]
pub struct EngineHealthQuery {
    pub view: Option<String>,
}

async fn snapshot_with_recording_state(
    state: &AppState,
    pipeline_ids: &[String],
    summary: bool,
) -> serde_json::Value {
    let recording_enabled = recording_enabled_map(state, pipeline_ids).await;
    if summary {
        crate::api_runtime_views::health_summary_snapshot(
            &state.engine,
            pipeline_ids,
            &recording_enabled,
            state.ingest_disconnect_grace_ms,
        )
        .await
    } else {
        crate::api_runtime_views::health_snapshot(
            &state.engine,
            pipeline_ids,
            &recording_enabled,
            state.ingest_disconnect_grace_ms,
        )
        .await
    }
}

pub async fn build_health_snapshot(state: &AppState) -> Result<serde_json::Value, ApiError> {
    let pipeline_ids = list_dashboard_runtime_pipeline_ids(state).await?;
    Ok(build_health_snapshot_for_pipeline_ids(state, &pipeline_ids).await)
}

pub async fn build_health_summary_snapshot(
    state: &AppState,
) -> Result<serde_json::Value, ApiError> {
    let pipeline_ids = list_dashboard_runtime_pipeline_ids(state).await?;
    Ok(build_health_summary_snapshot_for_pipeline_ids(state, &pipeline_ids).await)
}

pub async fn build_health_snapshot_for_pipeline_ids(
    state: &AppState,
    pipeline_ids: &[String],
) -> serde_json::Value {
    snapshot_with_recording_state(state, pipeline_ids, false).await
}

pub async fn build_health_summary_snapshot_for_pipeline_ids(
    state: &AppState,
    pipeline_ids: &[String],
) -> serde_json::Value {
    snapshot_with_recording_state(state, pipeline_ids, true).await
}

pub fn select_dashboard_runtime_pipeline_ids(
    requested_pipeline_id: Option<&str>,
    summary_health: bool,
    all_pipeline_ids: Vec<String>,
) -> Vec<String> {
    if summary_health {
        return all_pipeline_ids;
    }

    requested_pipeline_id
        .map(|pipeline_id| vec![pipeline_id.to_string()])
        .unwrap_or(all_pipeline_ids)
}

pub fn merge_dashboard_runtime_focus_pipeline(
    health: &mut serde_json::Value,
    focused_health: &serde_json::Value,
    pipeline_id: &str,
) {
    let Some(focused_pipeline) = focused_health
        .get("pipelines")
        .and_then(|pipelines| pipelines.as_object())
        .and_then(|pipelines| pipelines.get(pipeline_id))
        .cloned()
    else {
        return;
    };

    let Some(health_object) = health.as_object_mut() else {
        return;
    };
    let pipelines = health_object
        .entry("pipelines")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(pipelines_object) = pipelines.as_object_mut() else {
        return;
    };
    pipelines_object.insert(pipeline_id.to_string(), focused_pipeline);
}

pub async fn list_dashboard_runtime_pipeline_ids(
    state: &AppState,
) -> Result<Vec<String>, ApiError> {
    state
        .pipeline_service
        .list_pipeline_ids()
        .await
        .map_err(|err| ApiError::internal(format!("list dashboard pipeline ids: {err}")))
}

pub async fn v1_engine_health_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<EngineHealthQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    // The query only chooses the snapshot shape; both branches derive from the
    // same runtime state helpers above.
    let response = if query.view.as_deref() == Some("summary") {
        match build_health_summary_snapshot(&state).await {
            Ok(response) => response,
            Err(error) => return error.into_response(),
        }
    } else {
        match build_health_snapshot(&state).await {
            Ok(response) => response,
            Err(error) => return error.into_response(),
        }
    };
    Json(response).into_response()
}

pub async fn healthz_get_handler() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

#[cfg(test)]
mod tests {
    use super::{merge_dashboard_runtime_focus_pipeline, select_dashboard_runtime_pipeline_ids};

    #[test]
    fn select_dashboard_runtime_pipeline_ids_prefers_summary_over_focus() {
        let ids = select_dashboard_runtime_pipeline_ids(
            Some("pipe-2"),
            true,
            vec!["pipe-1".to_string(), "pipe-2".to_string()],
        );

        assert_eq!(ids, vec!["pipe-1".to_string(), "pipe-2".to_string()]);
    }

    #[test]
    fn merge_dashboard_runtime_focus_pipeline_inserts_focused_pipeline() {
        let mut health = serde_json::json!({ "pipelines": {} });
        let focused = serde_json::json!({
            "pipelines": {
                "pipe-1": { "status": "running" }
            }
        });

        merge_dashboard_runtime_focus_pipeline(&mut health, &focused, "pipe-1");

        assert_eq!(health["pipelines"]["pipe-1"]["status"], "running");
    }
}
