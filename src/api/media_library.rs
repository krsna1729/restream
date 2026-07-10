use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde::Deserialize;

use std::sync::Arc;

use crate::application::services::{
    ApiError,
    media_library_service::{MediaDeleteError, MediaRenameError},
};

use super::state::{AppState, get_session_token_from_headers, require_authenticated};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRenamePayload {
    pub new_name: String,
}

pub async fn recording_start_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let pipeline = state.pipeline_service.get_by_id(&pipeline_id).await?;

    let active = state
        .media_library_service
        .recording_start(
            &state.engine,
            &pipeline_id,
            pipeline.name.clone(),
            pipeline.input_source.clone(),
            &state.media_dir,
        )
        .await?;

    Ok(Json(serde_json::json!({ "enabled": true, "active": active })).into_response())
}

pub async fn recording_stop_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let _ = state.pipeline_service.get_by_id(&pipeline_id).await?;

    state
        .media_library_service
        .recording_stop(&state.engine, &pipeline_id)
        .await?;

    Ok(Json(serde_json::json!({ "enabled": false, "active": false })).into_response())
}

pub async fn media_list_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let files = state
        .media_library_service
        .list_media_files(&state.media_dir)
        .await;
    Json(serde_json::json!({ "files": files })).into_response()
}

pub async fn media_analysis_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(filename): Path<String>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let path = match media_path_under_root(&state.media_dir, &filename) {
        Ok(path) => path,
        Err(status) => return status.into_response(),
    };

    let analysis = match tokio::task::spawn_blocking(move || {
        crate::media::file_analysis::analyze_media_file(&path)
    })
    .await
    {
        Ok(Ok(analysis)) => analysis,
        Ok(Err(error)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("analysis task failed: {error}") })),
            )
                .into_response();
        }
    };

    Json(analysis).into_response()
}

pub fn media_content_type(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "ts" => "video/mp2t",
        "mkv" => "video/x-matroska",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

pub fn media_extension(filename: &str) -> Option<String> {
    filename
        .rsplit('.')
        .next()
        .map(|value| value.to_ascii_lowercase())
}

pub fn media_filename_is_supported(filename: &str) -> bool {
    matches!(
        media_extension(filename).as_deref(),
        Some("ts" | "mkv" | "mp4" | "mov")
    )
}

pub fn is_plain_media_filename(filename: &str) -> bool {
    let path = std::path::Path::new(filename);
    path.components().count() == 1
        && path
            .file_name()
            .is_some_and(|name| name == std::ffi::OsStr::new(filename))
}

pub fn validate_media_filename(filename: &str) -> Result<(), StatusCode> {
    if filename.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !is_plain_media_filename(filename) || !media_filename_is_supported(filename) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

pub fn media_path_under_root(
    media_dir: &str,
    filename: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    let _ = std::fs::create_dir_all(media_dir);
    let media_root =
        std::fs::canonicalize(media_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let path = std::path::Path::new(media_dir).join(filename);
    let canonical_path = std::fs::canonicalize(&path).map_err(|_| StatusCode::NOT_FOUND)?;
    if !canonical_path.starts_with(&media_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(canonical_path)
}

pub fn media_destination_path_under_root(
    media_dir: &str,
    filename: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    validate_media_filename(filename)?;
    let _ = std::fs::create_dir_all(media_dir);
    let media_root =
        std::fs::canonicalize(media_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let path = media_root.join(filename);
    if let Some(parent) = path.parent()
        && !parent.starts_with(&media_root)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(path)
}

pub async fn media_file_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(filename): Path<String>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let path = match media_path_under_root(&state.media_dir, &filename) {
        Ok(path) => path,
        Err(status) => return status.into_response(),
    };
    match tokio::fs::read(path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, media_content_type(&filename))],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "File not found").into_response(),
    }
}

pub async fn media_delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(filename): Path<String>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let canonical_path = match media_path_under_root(&state.media_dir, &filename) {
        Ok(path) => path,
        Err(StatusCode::INTERNAL_SERVER_ERROR) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Media directory error").into_response();
        }
        Err(StatusCode::NOT_FOUND) => {
            return (StatusCode::NOT_FOUND, "File not found").into_response();
        }
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid path").into_response(),
    };

    match state
        .media_library_service
        .delete_media_file(&filename, &canonical_path)
        .await
    {
        Ok(()) => Json(serde_json::json!({ "deleted": true })).into_response(),
        Err(MediaDeleteError::HasConfiguredIngests) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Cannot delete: file has configured ingests"})),
        )
            .into_response(),
        Err(MediaDeleteError::NotFound) => {
            (StatusCode::NOT_FOUND, "File not found").into_response()
        }
        Err(MediaDeleteError::Io(error)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to delete media file: {error}")})),
        )
            .into_response(),
    }
}

pub async fn media_rename_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(filename): Path<String>,
    Json(payload): Json<MediaRenamePayload>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let new_name = payload.new_name.trim();
    if validate_media_filename(&filename).is_err() || validate_media_filename(new_name).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid media filename"})),
        )
            .into_response();
    }
    if filename == new_name {
        return Json(serde_json::json!({ "renamed": true, "name": filename })).into_response();
    }
    if media_extension(&filename) != media_extension(new_name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Renaming cannot change the file extension"})),
        )
            .into_response();
    }

    let source_path = match media_path_under_root(&state.media_dir, &filename) {
        Ok(path) => path,
        Err(StatusCode::INTERNAL_SERVER_ERROR) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Media directory error").into_response();
        }
        Err(StatusCode::NOT_FOUND) => {
            return (StatusCode::NOT_FOUND, "File not found").into_response();
        }
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid path").into_response(),
    };
    let destination_path = match media_destination_path_under_root(&state.media_dir, new_name) {
        Ok(path) => path,
        Err(StatusCode::INTERNAL_SERVER_ERROR) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Media directory error").into_response();
        }
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid path").into_response(),
    };
    if destination_path.exists() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "A media file with that name already exists"})),
        )
            .into_response();
    }

    let updated_ingests = match state
        .media_library_service
        .rename_media_file(&filename, new_name, &source_path, &destination_path)
        .await
    {
        Ok(updated_ingests) => updated_ingests,
        Err(MediaRenameError::ConvertedExists) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "A converted MP4 with that name already exists"})),
            )
                .into_response();
        }
        Err(MediaRenameError::ConversionStateExists) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "A conversion state file with that name already exists"})),
            )
                .into_response();
        }
        Err(MediaRenameError::Io(error)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to rename media file: {error}")})),
            )
                .into_response();
        }
        Err(MediaRenameError::IngestUpdate(error)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to update ingest references: {error}")})),
            )
                .into_response();
        }
    };

    Json(serde_json::json!({
        "renamed": true,
        "name": new_name,
        "updatedIngests": updated_ingests
    }))
    .into_response()
}
