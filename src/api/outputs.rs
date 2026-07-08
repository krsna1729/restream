use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::api_view_models;
use crate::application::services::ApiError;
use crate::db;
use crate::domain::output_spec::{OutputConfig, OutputUrlScheme};

use crate::domain::state::DesiredOutputState;

use super::state::{
    AppState, MAX_ENCODING_LEN, MAX_NAME_LEN, MAX_URL_LEN, check_field_len,
    get_session_token_from_headers, require_authenticated, to_hex,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPayload {
    pub name: String,
    pub url: String,
    pub config: OutputConfig,
    pub monitoring_url: Option<String>,
}

impl OutputPayload {
    pub fn encoding_string(&self) -> String {
        self.config.to_encoding_string()
    }
}

pub fn is_supported_output_url(url: &str) -> bool {
    OutputUrlScheme::from_url(url).is_supported_output()
}

pub const OUTPUT_URL_SCHEME_ERROR: &str = "Invalid URL scheme. Supported schemes are rtmp://, rtmps://, srt://, hls://, http://, and https://";
pub const MONITORING_URL_SCHEME_ERROR: &str =
    "Invalid monitoring URL scheme. Supported schemes are http://, https://, and srt://";
pub const CUSTOM_OUTPUT_ENCODING_ERROR: &str =
    "Custom output encoding is not available yet; choose source or a preset encoding";

pub fn normalize_monitoring_url(url: Option<&str>) -> Option<String> {
    let trimmed = url.unwrap_or_default().trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn is_supported_monitoring_url(url: &str) -> bool {
    OutputUrlScheme::from_url(url).supports_monitoring()
}

#[derive(Deserialize)]
pub struct YoutubeMonitoringStatusQuery {
    pub url: String,
}

#[derive(Serialize)]
pub struct YoutubeMonitoringStatusResponse {
    pub canonical_watch_url: String,
    pub live_now: bool,
    pub live_content: bool,
    pub upcoming: bool,
    pub title: Option<String>,
}

pub fn normalize_youtube_watch_url(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed
        .host_str()?
        .trim_start_matches("www.")
        .to_ascii_lowercase();
    let path_parts = parsed
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    let video_id = if host == "youtu.be" {
        path_parts
            .first()
            .copied()
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    } else if host.ends_with("youtube.com") {
        parsed
            .query_pairs()
            .find(|(key, _)| key == "v")
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                if matches!(
                    path_parts.first().copied(),
                    Some("live" | "embed" | "shorts")
                ) {
                    path_parts.get(1).map(|value| (*value).to_string())
                } else {
                    None
                }
            })
    } else {
        None
    }?;
    Some(format!("https://www.youtube.com/watch?v={video_id}"))
}

pub fn youtube_watch_page_contains_flag(html: &str, flag: &str) -> bool {
    html.contains(flag)
}

pub fn extract_html_title(html: &str) -> Option<String> {
    let start = html.find("<title>")?;
    let rest = &html[start + "<title>".len()..];
    let end = rest.find("</title>")?;
    let title = rest[..end].trim();
    (!title.is_empty()).then(|| {
        title
            .replace("&amp;", "&")
            .replace("&#39;", "'")
            .replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
    })
}

pub fn parse_youtube_monitoring_status(
    canonical_watch_url: String,
    html: &str,
) -> YoutubeMonitoringStatusResponse {
    YoutubeMonitoringStatusResponse {
        canonical_watch_url,
        live_now: youtube_watch_page_contains_flag(html, "\"isLiveNow\":true"),
        live_content: youtube_watch_page_contains_flag(html, "\"isLiveContent\":true"),
        upcoming: youtube_watch_page_contains_flag(html, "\"isUpcoming\":true"),
        title: extract_html_title(html).map(|title| title.replace(" - YouTube", "")),
    }
}

pub async fn youtube_monitoring_status_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<YoutubeMonitoringStatusQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    if let Some(response) = check_field_len("url", &query.url, MAX_URL_LEN) {
        return response;
    }
    let canonical_watch_url = match normalize_youtube_watch_url(query.url.trim()) {
        Some(url) => url,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Expected a YouTube monitoring URL"})),
            )
                .into_response();
        }
    };

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36")
        .build();
    let Ok(client) = client else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let response = match client.get(&canonical_watch_url).send().await {
        Ok(response) => response,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "Failed to fetch YouTube metadata"})),
            )
                .into_response();
        }
    };
    let html = match response.text().await {
        Ok(html) => html,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "Failed to read YouTube metadata"})),
            )
                .into_response();
        }
    };
    Json(parse_youtube_monitoring_status(canonical_watch_url, &html)).into_response()
}

pub async fn outputs_create_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
    Json(payload): Json<OutputPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    if let Some(r) = check_field_len("name", &payload.name, MAX_NAME_LEN) {
        return Ok(r);
    }
    if let Some(r) = check_field_len("url", &payload.url, MAX_URL_LEN) {
        return Ok(r);
    }
    let output_config = payload.config.clone();
    let output_encoding = payload.encoding_string();
    if let Some(r) = check_field_len("config", &output_encoding, MAX_ENCODING_LEN) {
        return Ok(r);
    }
    if let Some(monitoring_url) = payload.monitoring_url.as_deref()
        && let Some(r) = check_field_len("monitoring_url", monitoring_url, MAX_URL_LEN)
    {
        return Ok(r);
    }
    if output_config.is_custom_output() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": CUSTOM_OUTPUT_ENCODING_ERROR
            })),
        )
            .into_response());
    }
    let url = payload.url.trim();
    if !is_supported_output_url(url) {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": OUTPUT_URL_SCHEME_ERROR
            })),
        )
            .into_response());
    }
    let monitoring_url = normalize_monitoring_url(payload.monitoring_url.as_deref());
    if let Some(ref url) = monitoring_url
        && !is_supported_monitoring_url(url)
    {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": MONITORING_URL_SCHEME_ERROR
            })),
        )
            .into_response());
    }

    let id = format!("output_{}", to_hex(&rand::random::<[u8; 8]>()));

    let output = state
        .output_service
        .create_output(
            &id,
            &pipeline_id,
            &payload.name,
            &payload.url,
            monitoring_url.as_deref(),
            DesiredOutputState::Stopped.as_str(),
            &output_config,
        )
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "message": "Output created",
            "output": api_view_models::output_response_json(&output)
        })),
    )
        .into_response())
}

pub async fn outputs_update_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, output_id)): Path<(String, String)>,
    Json(payload): Json<OutputPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    if let Some(r) = check_field_len("name", &payload.name, MAX_NAME_LEN) {
        return Ok(r);
    }
    if let Some(r) = check_field_len("url", &payload.url, MAX_URL_LEN) {
        return Ok(r);
    }
    let output_config = payload.config.clone();
    let output_encoding = payload.encoding_string();
    if let Some(r) = check_field_len("config", &output_encoding, MAX_ENCODING_LEN) {
        return Ok(r);
    }
    if let Some(monitoring_url) = payload.monitoring_url.as_deref()
        && let Some(r) = check_field_len("monitoring_url", monitoring_url, MAX_URL_LEN)
    {
        return Ok(r);
    }
    if output_config.is_custom_output() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": CUSTOM_OUTPUT_ENCODING_ERROR
            })),
        )
            .into_response());
    }
    let url = payload.url.trim();
    if !is_supported_output_url(url) {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": OUTPUT_URL_SCHEME_ERROR
            })),
        )
            .into_response());
    }
    let monitoring_url = normalize_monitoring_url(payload.monitoring_url.as_deref());
    if let Some(ref url) = monitoring_url
        && !is_supported_monitoring_url(url)
    {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": MONITORING_URL_SCHEME_ERROR
            })),
        )
            .into_response());
    }
    let existing = state
        .output_service
        .get_by_id(&pipeline_id, &output_id)
        .await?;
    if existing.desired_state == "running"
        && (existing.url != payload.url || existing.config != output_config)
    {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Cannot change output transport URL or config while the output is running"
            })),
        )
            .into_response());
    }

    let updated = state
        .output_service
        .update_output(
            &pipeline_id,
            &output_id,
            &payload.name,
            &payload.url,
            monitoring_url.as_deref(),
            &output_config,
        )
        .await?;

    Ok(Json(serde_json::json!({
        "message": "Output updated",
        "output": api_view_models::output_response_json(&updated)
    }))
    .into_response())
}

pub async fn outputs_delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, output_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    state.engine.unregister_egress(&output_id).await;
    state
        .output_service
        .delete_output(&pipeline_id, &output_id)
        .await?;
    Ok(Json(serde_json::json!({"message": "Output deleted"})).into_response())
}

pub async fn outputs_start_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, output_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let output = state
        .output_service
        .request_start(&pipeline_id, &output_id)
        .await?;
    Ok(Json(serde_json::json!({
        "message": "Output started",
        "desiredState": "running",
        "output": api_view_models::output_response_json(&output)
    }))
    .into_response())
}

pub async fn outputs_stop_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, output_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let output = state
        .output_service
        .request_stop(&pipeline_id, &output_id)
        .await?;
    Ok(Json(serde_json::json!({
        "message": "Output stopped",
        "desiredState": "stopped",
        "output": api_view_models::output_response_json(&output)
    }))
    .into_response())
}

pub async fn output_status_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((_pipeline_id, output_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    match crate::api_runtime_views::output_status(&state.engine, &output_id).await {
        Some(status) => Json(status).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "output not active"})),
        )
            .into_response(),
    }
}
