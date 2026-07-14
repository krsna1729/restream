//! Settings HTTP handlers sit at the dashboard/configuration boundary.
//! They assemble a view model from several stores and normalize incoming
//! settings updates before handing persistence off to application services.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::warn;

use crate::api_view_models;

use crate::application::srt_ingest::SRT_INGEST_GLOBAL_CONFIG_META_KEY;
use crate::domain::ingest_security::IngestSecurityConfig;
use crate::domain::srt_ingest::SrtGlobalIngestConfig;
use crate::domain::transcode_profile::TranscodeProfiles;
use crate::planner::backend_policy::BackendPolicy;

use super::state::{
    AppState, BOOTSTRAP_PASSWORD_PROMPT_META_KEY, DEFAULT_INGEST_HOST, require_authenticated,
};

#[derive(Deserialize)]
pub struct ConfigGetQuery {
    pub jobs: Option<String>,
    pub view: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPatchPayload {
    pub server_name: Option<String>,
    pub ingest_host: Option<String>,
    pub ingest_security: Option<IngestSecurityConfig>,
    pub recording_settings: Option<crate::domain::recording::RecordingSettings>,
    pub srt_ingest: Option<SrtGlobalIngestConfig>,
    pub transcode_profiles: Option<TranscodeProfiles>,
    pub backend_policy: Option<BackendPolicy>,
}

#[derive(Debug, Clone)]
struct NormalizedConfigPatch {
    ingest_security: Option<IngestSecurityConfig>,
    srt_ingest_json: Option<String>,
}

fn dashboard_password_change_recommended(meta: Option<String>) -> bool {
    meta.as_deref() == Some("pending")
}

fn config_response_json(
    settings: &crate::application::settings::SettingsSnapshot,
    pipelines: Vec<serde_json::Value>,
    outputs: &[crate::application::models::Output],
    jobs_json: Vec<serde_json::Value>,
    dashboard_password_change_recommended: bool,
    is_dashboard_view: bool,
) -> serde_json::Value {
    if is_dashboard_view {
        serde_json::json!({
            "serverName": settings.server_name,
            "ingestHost": settings.ingest_host,
            "dashboardPasswordChangeRecommended": dashboard_password_change_recommended,
            "backendPolicy": settings.backend_policy,
            "transcodeProfiles": settings.transcode_profiles,
            "pipelines": pipelines,
            "outputs": api_view_models::output_response_json_list(outputs),
            "jobs": jobs_json
        })
    } else {
        serde_json::json!({
            "serverName": settings.server_name,
            "ingestHost": settings.ingest_host,
            "dashboardPasswordChangeRecommended": dashboard_password_change_recommended,
            "ingestSecurity": settings.ingest_security,
            "recordingSettings": settings.recording_settings,
            "srtIngest": settings.srt_ingest,
            "backendPolicy": settings.backend_policy,
            "transcodeProfiles": settings.transcode_profiles,
            "pipelines": pipelines,
            "outputs": api_view_models::output_response_json_list(outputs),
            "jobs": jobs_json
        })
    }
}

fn validate_config_patch_payload(
    payload: &ConfigPatchPayload,
) -> Result<NormalizedConfigPatch, Response> {
    if let Some(name) = payload.server_name.as_deref()
        && name.trim().is_empty()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "serverName must be a non-empty string",
        )
            .into_response());
    }

    let ingest_security = match payload.ingest_security.clone() {
        Some(mut sec) => {
            if let Err(error) = sec.validate() {
                return Err((StatusCode::BAD_REQUEST, error).into_response());
            }
            sec.normalize();
            Some(sec)
        }
        None => None,
    };

    let srt_ingest_json = match payload.srt_ingest.clone() {
        Some(mut srt_ingest) => {
            if let Err(error) = srt_ingest.validate() {
                return Err((StatusCode::BAD_REQUEST, error).into_response());
            }
            match serde_json::to_string(&srt_ingest) {
                Ok(value) => Some(value),
                Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
            }
        }
        None => None,
    };

    if let Some(profiles) = payload.transcode_profiles.as_ref() {
        for (name, profile) in profiles {
            if let Err(err) = profile.validate() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("Invalid profile '{}': {}", name, err),
                )
                    .into_response());
            }
        }
    }

    Ok(NormalizedConfigPatch {
        ingest_security,
        srt_ingest_json,
    })
}

pub async fn config_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<ConfigGetQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let ingest_host = state
        .settings_service
        .get_ingest_host_raw()
        .await
        .unwrap_or_default();
    let effective_ingest_host = if ingest_host.is_empty() {
        DEFAULT_INGEST_HOST
    } else {
        &ingest_host
    };
    let raw_pipelines = state
        .settings_service
        .list_pipelines()
        .await
        .unwrap_or_default();

    let mut pipelines = Vec::with_capacity(raw_pipelines.len());
    for pipeline in &raw_pipelines {
        let file_ingest = match state
            .file_ingest_service
            .load_pipeline_file_ingest_state(&state.engine, pipeline)
            .await
        {
            Ok(file_ingest) => file_ingest,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        pipelines.push(api_view_models::pipeline_response_json_with_file_ingest(
            pipeline,
            effective_ingest_host,
            state.ports.rtmp,
            state.ports.srt,
            file_ingest.ingest,
            file_ingest.running,
        ));
    }

    let outputs = state
        .settings_service
        .list_outputs()
        .await
        .unwrap_or_default();
    let settings = match state.settings_snapshot().await {
        Ok(s) => s,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let dashboard_password_change_recommended = dashboard_password_change_recommended(
        state
            .settings_service
            .get_meta(BOOTSTRAP_PASSWORD_PROMPT_META_KEY)
            .await
            .unwrap_or(None),
    );
    let is_dashboard_view = query.view.as_deref() == Some("dashboard");
    let jobs_json = if is_dashboard_view {
        Vec::new()
    } else {
        let jobs = state.settings_service.list_jobs().await.unwrap_or_default();
        if query.jobs.as_deref() == Some("latest") {
            api_view_models::latest_job_response_json_list(&jobs)
        } else {
            api_view_models::job_response_json_list(&jobs)
        }
    };

    Json(config_response_json(
        &settings,
        pipelines,
        &outputs,
        jobs_json,
        dashboard_password_change_recommended,
        is_dashboard_view,
    ))
    .into_response()
}

pub async fn config_patch_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<ConfigPatchPayload>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let normalized = match validate_config_patch_payload(&payload) {
        Ok(normalized) => normalized,
        Err(response) => return response,
    };

    if let Some(ref name) = payload.server_name
        && state.settings_service.set_server_name(name).await.is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    if let Some(ref host) = payload.ingest_host
        && state.settings_service.set_ingest_host(host).await.is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // Normalize and validate before persistence so every downstream consumer
    // sees the same canonical security configuration.
    if let Some(sec) = normalized.ingest_security {
        if state
            .settings_service
            .save_ingest_security_config(&sec)
            .await
            .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        state.update_ingest_security_config(sec);
    }

    if let Some(ref recording_settings) = payload.recording_settings
        && state
            .settings_service
            .save_recording_settings(recording_settings)
            .await
            .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    if let Some(raw_json) = normalized.srt_ingest_json {
        if state
            .settings_service
            .set_meta(SRT_INGEST_GLOBAL_CONFIG_META_KEY, &raw_json)
            .await
            .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        if state.refresh_srt_ingest_policy_store().await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    if let Some(ref profiles) = payload.transcode_profiles
        && let Err(e) = state
            .settings_service
            .save_transcode_profiles(profiles)
            .await
    {
        warn!(err = %e, "failed to save transcode profiles");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to save profiles").into_response();
    }

    if let Some(policy) = payload.backend_policy {
        if state
            .settings_service
            .save_backend_policy(policy)
            .await
            .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        state.engine.set_backend_policy(policy);
    }

    let settings = match state.settings_snapshot().await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    Json(serde_json::json!({
        "serverName": settings.server_name,
        "ingestHost": settings.ingest_host,
        "ingestSecurity": settings.ingest_security,
        "recordingSettings": settings.recording_settings,
        "srtIngest": settings.srt_ingest,
        "backendPolicy": settings.backend_policy,
        "transcodeProfiles": settings.transcode_profiles
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigPatchPayload, dashboard_password_change_recommended, validate_config_patch_payload,
    };
    use axum::http::StatusCode;

    #[test]
    fn dashboard_password_change_recommended_only_for_pending_marker() {
        assert!(dashboard_password_change_recommended(Some(
            "pending".to_string()
        )));
        assert!(!dashboard_password_change_recommended(Some(
            "dismissed".to_string()
        )));
        assert!(!dashboard_password_change_recommended(None));
    }

    #[test]
    fn validate_config_patch_payload_rejects_blank_server_name() {
        let payload = ConfigPatchPayload {
            server_name: Some("   ".to_string()),
            ingest_host: None,
            ingest_security: None,
            recording_settings: None,
            srt_ingest: None,
            transcode_profiles: None,
            backend_policy: None,
        };

        let response =
            validate_config_patch_payload(&payload).expect_err("blank server names should fail");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
