use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::warn;

use crate::api_view_models;

use crate::application::srt_ingest::SRT_INGEST_GLOBAL_CONFIG_META_KEY;
use crate::domain::ingest_security::IngestSecurityConfig;
use crate::domain::srt_ingest::SrtGlobalIngestConfig;
use crate::domain::transcode_profile::TranscodeProfiles;

use super::state::{
    AppState, BOOTSTRAP_PASSWORD_PROMPT_META_KEY, DEFAULT_INGEST_HOST,
    get_session_token_from_headers, refresh_srt_ingest_policy_store,
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
    pub recording_settings: Option<crate::application::recording::RecordingSettings>,
    pub srt_ingest: Option<SrtGlobalIngestConfig>,
    pub transcode_profiles: Option<TranscodeProfiles>,
}

pub async fn config_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<ConfigGetQuery>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
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
    let settings = match state.settings_service.load_snapshot(&state.security).await {
        Ok(s) => s,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let dashboard_password_change_recommended = state
        .settings_service
        .get_meta(BOOTSTRAP_PASSWORD_PROMPT_META_KEY)
        .await
        .unwrap_or(None)
        .as_deref()
        == Some("pending");
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

    let response = if is_dashboard_view {
        serde_json::json!({
            "serverName": settings.server_name,
            "ingestHost": settings.ingest_host,
            "dashboardPasswordChangeRecommended": dashboard_password_change_recommended,
            "transcodeProfiles": settings.transcode_profiles,
            "pipelines": pipelines,
            "outputs": api_view_models::output_response_json_list(&outputs),
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
            "transcodeProfiles": settings.transcode_profiles,
            "pipelines": pipelines,
            "outputs": api_view_models::output_response_json_list(&outputs),
            "jobs": jobs_json
        })
    };

    Json(response).into_response()
}

pub async fn config_patch_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<ConfigPatchPayload>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    if let Some(ref name) = payload.server_name {
        if name.trim().is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                "serverName must be a non-empty string",
            )
                .into_response();
        }
        let _ = state.settings_service.set_server_name(name).await;
    }

    if let Some(ref host) = payload.ingest_host
        && state.settings_service.set_ingest_host(host).await.is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    if let Some(mut sec) = payload.ingest_security.clone() {
        if let Err(error) = sec.validate() {
            return (StatusCode::BAD_REQUEST, error).into_response();
        }
        sec.normalize();
        state.security.update_config(sec.clone());
        if state
            .settings_service
            .save_ingest_security_config(&sec)
            .await
            .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
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

    if let Some(mut srt_ingest) = payload.srt_ingest.clone() {
        if let Err(error) = srt_ingest.validate() {
            return (StatusCode::BAD_REQUEST, error).into_response();
        }
        let raw_json = match serde_json::to_string(&srt_ingest) {
            Ok(value) => value,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        if state
            .settings_service
            .set_meta(SRT_INGEST_GLOBAL_CONFIG_META_KEY, &raw_json)
            .await
            .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        refresh_srt_ingest_policy_store(&state).await;
    }

    if let Some(ref profiles) = payload.transcode_profiles {
        for (name, profile) in profiles {
            if let Err(err) = profile.validate() {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("Invalid profile '{}': {}", name, err),
                )
                    .into_response();
            }
        }
        if let Err(e) = state
            .settings_service
            .save_transcode_profiles(profiles)
            .await
        {
            warn!(err = %e, "failed to save transcode profiles");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to save profiles").into_response();
        }
    }

    let settings = match state.settings_service.load_snapshot(&state.security).await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    Json(serde_json::json!({
        "serverName": settings.server_name,
        "ingestHost": settings.ingest_host,
        "ingestSecurity": settings.ingest_security,
        "recordingSettings": settings.recording_settings,
        "srtIngest": settings.srt_ingest,
        "transcodeProfiles": settings.transcode_profiles
    }))
    .into_response()
}
