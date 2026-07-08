use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use crate::db;
use crate::application::recording::{recording_enabled_meta_key, load_recording_settings, spawn_recording_task};
use crate::application::ports::SqliteMetaStore;

use super::state::{
    AppState, check_field_len, get_session_token_from_headers, to_hex,
    MAX_NAME_LEN,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRenamePayload {
    pub new_name: String,
}

pub async fn recording_start_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let pipeline = match db::get_pipeline(&state.db, &pipeline_id).await {
        Ok(Some(p)) => p,
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Pipeline not found"})),
            )
                .into_response();
        }
    };

    let meta_key = recording_enabled_meta_key(&pipeline_id);
    let _ = db::set_meta(&state.db, &meta_key, "1").await;

    let has_ingest = state
        .engine
        .ingests
        .active
        .read()
        .await
        .contains_key(&pipeline_id);
    if has_ingest && !state.engine.is_recording_active(&pipeline_id).await {
        let recording_settings = load_recording_settings(
            &SqliteMetaStore::new(state.db.clone()),
        )
        .await;
        spawn_recording_task(
            state.engine.clone(),
            pipeline.name.clone(),
            pipeline_id.clone(),
            pipeline.input_source.clone(),
            state.media_dir.clone(),
            recording_settings,
        )
        .await;
    }

    let active = state.engine.is_recording_active(&pipeline_id).await;
    Json(serde_json::json!({ "enabled": true, "active": active })).into_response()
}

pub async fn recording_stop_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    match db::get_pipeline(&state.db, &pipeline_id).await {
        Ok(Some(_)) => {}
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Pipeline not found"})),
            )
                .into_response();
        }
    };

    let meta_key = recording_enabled_meta_key(&pipeline_id);
    let _ = db::set_meta(&state.db, &meta_key, "0").await;

    state.engine.unregister_recording(&pipeline_id).await;

    Json(serde_json::json!({ "enabled": false, "active": false })).into_response()
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

    #[derive(Clone)]
    struct MediaDirEntry {
        name: String,
        size: u64,
        modified_at: String,
        modified_ms: i64,
    }

    fn entry_modified(metadata: &std::fs::Metadata) -> (String, i64) {
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        let modified_at = chrono::DateTime::from_timestamp_millis(modified_ms)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();
        (modified_at, modified_ms)
    }

    let mut entries = HashMap::<String, MediaDirEntry>::new();
    if let Ok(mut media_dir_entries) = tokio::fs::read_dir(&state.media_dir).await {
        while let Ok(Some(entry)) = media_dir_entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if (name.ends_with(".ts")
                || name.ends_with(".mkv")
                || name.ends_with(".mp4")
                || name.ends_with(".mov"))
                && let Ok(metadata) = entry.metadata().await
            {
                let (modified_at, modified_ms) = entry_modified(&metadata);
                entries.insert(
                    name.clone(),
                    MediaDirEntry {
                        name,
                        size: metadata.len(),
                        modified_at,
                        modified_ms,
                    },
                );
            }
        }
    }

    let mut files = Vec::new();
    let mut consumed = HashSet::new();
    let mut names = entries.keys().cloned().collect::<Vec<_>>();
    names.sort();
    for name in names {
        if !consumed.insert(name.clone()) {
            continue;
        }
        let Some(entry) = entries.get(&name).cloned() else {
            continue;
        };
        if name.ends_with(".mp4") {
            let companion_source_name = std::path::Path::new(&name)
                .with_extension("ts")
                .file_name()
                .map(|value| value.to_string_lossy().to_string());
            if let Some(companion_source_name) = companion_source_name
                && crate::media::recording::is_recording_source_filename(&companion_source_name)
                && entries.contains_key(&companion_source_name)
            {
                continue;
            }
        }
        let ingests = db::list_ingests_for_filename(&state.db, &name)
            .await
            .unwrap_or_default();
        let lower_name = name.to_ascii_lowercase();
        let kind = if lower_name.contains("recording") {
            "recording"
        } else {
            "source"
        };

        if crate::media::recording::is_recording_source_filename(&name) {
            let source_path = std::path::Path::new(&state.media_dir).join(&name);
            let converted_name = crate::media::recording::build_mp4_path(&source_path)
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .filter(|candidate| entries.contains_key(candidate));
            let converted_entry = converted_name
                .as_ref()
                .and_then(|candidate| entries.get(candidate).cloned());
            if let Some(converted_name) = &converted_name {
                consumed.insert(converted_name.clone());
            }
            let conversion_state = crate::media::recording::load_conversion_state(&source_path);
            let conversion_status = if converted_entry.is_some() {
                Some("ready")
            } else {
                conversion_state.as_ref().map(|state| match state.status {
                    crate::media::recording::RecordingConversionStatus::Converting => "converting",
                    crate::media::recording::RecordingConversionStatus::Ready => "ready",
                    crate::media::recording::RecordingConversionStatus::Failed => "failed",
                })
            };
            let conversion_error = conversion_state
                .as_ref()
                .and_then(|state| state.error.as_deref());
            let conversion_updated_at = conversion_state
                .as_ref()
                .map(|state| state.updated_at.as_str());
            let converted_size = converted_entry.as_ref().map(|value| value.size);
            let total_size = entry.size + converted_size.unwrap_or(0);
            let modified = converted_entry
                .as_ref()
                .filter(|value| value.modified_ms > entry.modified_ms)
                .map(|value| value.modified_at.clone())
                .unwrap_or_else(|| entry.modified_at.clone());

            files.push(serde_json::json!({
                "name": name,
                "size": total_size,
                "modifiedAt": modified,
                "ingestCount": ingests.len(),
                "kind": kind,
                "sourceName": entry.name,
                "sourceSize": entry.size,
                "convertedName": converted_entry.as_ref().map(|value| value.name.clone()),
                "convertedSize": converted_size,
                "playName": converted_entry.as_ref().map(|value| value.name.clone()),
                "conversionStatus": conversion_status,
                "conversionError": conversion_error,
                "conversionUpdatedAt": conversion_updated_at,
            }));
            continue;
        }

        files.push(serde_json::json!({
            "name": name,
            "size": entry.size,
            "modifiedAt": entry.modified_at,
            "ingestCount": ingests.len(),
            "kind": kind,
            "sourceName": entry.name,
            "sourceSize": entry.size,
            "convertedName": serde_json::Value::Null,
            "convertedSize": serde_json::Value::Null,
            "playName": entry.name,
            "conversionStatus": serde_json::Value::Null,
            "conversionError": serde_json::Value::Null,
            "conversionUpdatedAt": serde_json::Value::Null,
        }));
    }

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
    let path = std::path::Path::new(media_dir).join(filename);
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

    let ingests = db::list_ingests_for_filename(&state.db, &filename)
        .await
        .unwrap_or_default();
    if !ingests.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Cannot delete: file has configured ingests"})),
        )
            .into_response();
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

    let mut delete_paths = vec![canonical_path.clone()];
    if crate::media::recording::is_recording_source_filename(&filename) {
        let converted_path = crate::media::recording::build_mp4_path(&canonical_path);
        if converted_path.exists() {
            delete_paths.push(converted_path);
        }
        let state_path = crate::media::recording::build_conversion_state_path(&canonical_path);
        if state_path.exists() {
            delete_paths.push(state_path);
        }
    }

    match tokio::fs::remove_file(&canonical_path).await {
        Ok(_) => {
            for extra_path in delete_paths.into_iter().skip(1) {
                let _ = tokio::fs::remove_file(extra_path).await;
            }
            Json(serde_json::json!({ "deleted": true })).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "File not found").into_response(),
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

    let mut rename_pairs = vec![(source_path.clone(), destination_path.clone())];
    if crate::media::recording::is_recording_source_filename(&filename) {
        let source_converted = crate::media::recording::build_mp4_path(&source_path);
        let destination_converted = crate::media::recording::build_mp4_path(&destination_path);
        if source_converted.exists() {
            if destination_converted.exists() {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({"error": "A converted MP4 with that name already exists"})),
                )
                    .into_response();
            }
            rename_pairs.push((source_converted, destination_converted));
        }

        let source_state = crate::media::recording::build_conversion_state_path(&source_path);
        let destination_state =
            crate::media::recording::build_conversion_state_path(&destination_path);
        if source_state.exists() {
            if destination_state.exists() {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({"error": "A conversion state file with that name already exists"})),
                )
                    .into_response();
            }
            rename_pairs.push((source_state, destination_state));
        }
    }

    let mut completed = Vec::new();
    for (from, to) in &rename_pairs {
        if let Err(error) = tokio::fs::rename(from, to).await {
            for (rollback_from, rollback_to) in completed.into_iter().rev() {
                let _ = tokio::fs::rename(rollback_to, rollback_from).await;
            }
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to rename media file: {error}")})),
            )
                .into_response();
        }
        completed.push((from.clone(), to.clone()));
    }

    let ingests = db::list_ingests_for_filename(&state.db, &filename)
        .await
        .unwrap_or_default();
    for ingest in &ingests {
        if let Err(error) = db::update_ingest(
            &state.db,
            &ingest.id,
            new_name,
            &ingest.stream_key,
            ingest.loop_flag,
            &ingest.start_time,
            ingest.live_optimized,
            ingest.target_gop_seconds,
        )
        .await
        {
            for (rollback_from, rollback_to) in completed.into_iter().rev() {
                let _ = tokio::fs::rename(rollback_to, rollback_from).await;
            }
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to update ingest references: {error}")})),
            )
                .into_response();
        }
    }

    Json(serde_json::json!({
        "renamed": true,
        "name": new_name,
        "updatedIngests": ingests.len()
    }))
    .into_response()
}
