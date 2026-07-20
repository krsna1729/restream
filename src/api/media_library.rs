//! Media-library HTTP handlers own the boundary between dashboard requests and
//! filesystem-backed media operations. They validate filenames, translate path
//! errors into HTTP responses, and delegate the actual media work to services.

use axum::{
    Json,
    body::Body,
    extract::{Multipart, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures_util::stream;
use serde::Deserialize;

use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use std::sync::Arc;

use crate::application::services::media_library_service::{MediaDeleteError, MediaRenameError};

use super::error::ApiError;
use super::state::{AppState, require_authenticated};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRenamePayload {
    pub new_name: String,
}

pub const MAX_MEDIA_UPLOAD_BYTES: usize = 8 * 1024 * 1024 * 1024;

fn media_directory_error_response(status: StatusCode) -> Response {
    match status {
        StatusCode::INTERNAL_SERVER_ERROR => {
            (StatusCode::INTERNAL_SERVER_ERROR, "Media directory error").into_response()
        }
        StatusCode::NOT_FOUND => (StatusCode::NOT_FOUND, "File not found").into_response(),
        _ => (StatusCode::BAD_REQUEST, "Invalid path").into_response(),
    }
}

fn recording_state_response(enabled: bool, active: bool) -> Response {
    Json(serde_json::json!({ "enabled": enabled, "active": active })).into_response()
}

/// Enables recording for one pipeline and reports both the desired flag and
/// the current runtime-active state after the request.
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

    Ok(recording_state_response(true, active))
}

/// Disables recording for one pipeline and reports the cleared desired/runtime
/// state once the request completes.
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

    Ok(recording_state_response(false, false))
}

/// Lists the current media-library rows assembled by the service layer.
pub async fn media_list_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let files = state
        .media_library_service
        .list_media_files(&state.media_dir)
        .await;
    Json(serde_json::json!({ "files": files })).into_response()
}

/// Stream one media file into the configured library without buffering it in
/// memory. The upload route has its own body limit; keeping that larger limit
/// off the general API router prevents ordinary control-plane requests from
/// inheriting media-sized attack surface.
pub async fn media_upload_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let mut uploaded = None;
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(
                        serde_json::json!({"error": format!("Invalid multipart upload: {error}")}),
                    ),
                )
                    .into_response();
            }
        };
        if field.name() != Some("file") || uploaded.is_some() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Upload exactly one file field"})),
            )
                .into_response();
        }
        let Some(filename) = field.file_name().map(str::to_owned) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Uploaded file is missing a filename"})),
            )
                .into_response();
        };
        if validate_media_filename(&filename).is_err() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Unsupported or unsafe media filename"})),
            )
                .into_response();
        }

        let destination = match media_destination_path_under_root(&state.media_dir, &filename) {
            Ok(path) => path,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "Media directory error"})),
                )
                    .into_response();
            }
        };
        if destination.exists() {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "A media file with that name already exists"})),
            )
                .into_response();
        }

        let temporary = destination.with_file_name(format!(
            ".{filename}.upload-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let mut output = match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
        {
            Ok(file) => file,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("Failed to create upload: {error}")})),
                )
                    .into_response();
            }
        };

        let mut size = 0usize;
        let mut write_error = None;
        while let Some(chunk) = match field.chunk().await {
            Ok(chunk) => chunk,
            Err(error) => {
                write_error = Some(format!("Failed to read upload: {error}"));
                None
            }
        } {
            size = size.saturating_add(chunk.len());
            if size > MAX_MEDIA_UPLOAD_BYTES {
                write_error = Some("Uploaded file exceeds the 8 GiB limit".to_string());
                break;
            }
            if let Err(error) = output.write_all(&chunk).await {
                write_error = Some(format!("Failed to write upload: {error}"));
                break;
            }
        }
        if let Err(error) = output.flush().await {
            write_error.get_or_insert_with(|| format!("Failed to finalize upload: {error}"));
        }
        drop(output);

        if let Some(error) = write_error {
            let _ = tokio::fs::remove_file(&temporary).await;
            let status = if error == "Uploaded file exceeds the 8 GiB limit" {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::BAD_REQUEST
            };
            return (status, Json(serde_json::json!({"error": error}))).into_response();
        }

        // hard_link is create-only at the final name, so a concurrent upload
        // cannot replace an existing library file between the earlier check
        // and publication. It is atomic because both paths share media_dir.
        match tokio::fs::hard_link(&temporary, &destination).await {
            Ok(()) => {
                let _ = tokio::fs::remove_file(&temporary).await;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = tokio::fs::remove_file(&temporary).await;
                return (
                    StatusCode::CONFLICT,
                    Json(
                        serde_json::json!({"error": "A media file with that name already exists"}),
                    ),
                )
                    .into_response();
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(&temporary).await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(
                        serde_json::json!({"error": format!("Failed to publish upload: {error}")}),
                    ),
                )
                    .into_response();
            }
        }
        uploaded = Some((filename, size));
    }

    match uploaded {
        Some((name, size)) => Json(serde_json::json!({
            "uploaded": true,
            "name": name,
            "size": size,
        }))
        .into_response(),
        None => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Upload requires one file field"})),
        )
            .into_response(),
    }
}

pub async fn media_analysis_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(filename): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let path = match media_path_under_root(&state.media_dir, &filename) {
        Ok(path) => path,
        Err(status) => return status.into_response(),
    };

    let analysis = match state.media_library_service.analyze_media_file(path).await {
        Ok(analysis) => analysis,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
    };

    Json(analysis).into_response()
}

/// Maps one supported media filename to its HTTP content type for streaming and
/// download responses.
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

/// Returns the lowercase extension for one media filename, if present.
pub fn media_extension(filename: &str) -> Option<String> {
    filename
        .rsplit('.')
        .next()
        .map(|value| value.to_ascii_lowercase())
}

/// Limits the library surface to the media extensions explicitly supported by
/// the dashboard and playback handlers.
pub fn media_filename_is_supported(filename: &str) -> bool {
    matches!(
        media_extension(filename).as_deref(),
        Some("ts" | "mkv" | "mp4" | "mov")
    )
}

/// Rejects nested paths and path-component tricks so callers can only target a
/// plain filename at the media-library boundary.
pub fn is_plain_media_filename(filename: &str) -> bool {
    let path = std::path::Path::new(filename);
    path.components().count() == 1
        && path
            .file_name()
            .is_some_and(|name| name == std::ffi::OsStr::new(filename))
}

/// Validates one user-supplied media filename before any filesystem lookup or
/// destination-path construction happens.
pub fn validate_media_filename(filename: &str) -> Result<(), StatusCode> {
    if filename.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !is_plain_media_filename(filename) || !media_filename_is_supported(filename) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

/// Resolves an existing media filename under the configured library root,
/// rejecting traversal and symlink escapes via canonicalization.
pub fn media_path_under_root(
    media_dir: &str,
    filename: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    // Existing-file lookups canonicalize both root and target so requests
    // cannot escape the configured media directory through symlinks or parents.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaByteRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaRangeParseError;

/// Parses a single RFC 7233 byte-range header into an inclusive byte span for
/// media streaming responses.
pub fn parse_media_range_header(
    range: &str,
    size: u64,
) -> Result<Option<MediaByteRange>, MediaRangeParseError> {
    let Some(spec) = range.strip_prefix("bytes=") else {
        return Ok(None);
    };
    if spec.contains(',') {
        return Err(MediaRangeParseError);
    }
    let Some((start_raw, end_raw)) = spec.split_once('-') else {
        return Err(MediaRangeParseError);
    };
    if start_raw.is_empty() {
        let suffix_len = end_raw.parse::<u64>().map_err(|_| MediaRangeParseError)?;
        if suffix_len == 0 {
            return Err(MediaRangeParseError);
        }
        if size == 0 {
            return Err(MediaRangeParseError);
        }
        let start = size.saturating_sub(suffix_len);
        return Ok(Some(MediaByteRange {
            start,
            end: size - 1,
        }));
    }

    let start = start_raw.parse::<u64>().map_err(|_| MediaRangeParseError)?;
    let end = if end_raw.is_empty() {
        size.checked_sub(1).ok_or(MediaRangeParseError)?
    } else {
        end_raw.parse::<u64>().map_err(|_| MediaRangeParseError)?
    };
    if start >= size || end < start {
        return Err(MediaRangeParseError);
    }
    Ok(Some(MediaByteRange {
        start,
        end: end.min(size - 1),
    }))
}

fn header_value(value: impl AsRef<str>) -> Option<HeaderValue> {
    HeaderValue::from_str(value.as_ref()).ok()
}

fn media_range_not_satisfiable_response(size: u64) -> Response {
    let mut response = (StatusCode::RANGE_NOT_SATISFIABLE, "Range not satisfiable").into_response();
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Some(value) = header_value(format!("bytes */{size}")) {
        response.headers_mut().insert(header::CONTENT_RANGE, value);
    }
    response
}

// Streams a media file with optional byte-range support while keeping range
// validation and HTTP header shaping local to the transport boundary.
async fn media_file_response(
    path: std::path::PathBuf,
    filename: &str,
    range_header: Option<&HeaderValue>,
) -> Result<Response, std::io::Error> {
    let mut file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    let range = match range_header.and_then(|value| value.to_str().ok()) {
        Some(range) => match parse_media_range_header(range, size) {
            Ok(range) => range,
            Err(MediaRangeParseError) => return Ok(media_range_not_satisfiable_response(size)),
        },
        None => None,
    };

    let requested = range.unwrap_or(MediaByteRange {
        start: 0,
        end: size.saturating_sub(1),
    });
    let len = if size == 0 {
        0
    } else {
        requested.end - requested.start + 1
    };
    if requested.start > 0 {
        file.seek(std::io::SeekFrom::Start(requested.start)).await?;
    }
    let reader = file.take(len);
    let body = Body::from_stream(stream::try_unfold(reader, |mut reader| async move {
        let mut chunk = vec![0; 8192];
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok::<_, std::io::Error>(None);
        }
        chunk.truncate(read);
        Ok(Some((Bytes::from(chunk), reader)))
    }));
    let status = if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(media_content_type(filename)),
    );
    if let Some(value) = header_value(len.to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    if range.is_some()
        && let Some(value) = header_value(format!(
            "bytes {}-{}/{}",
            requested.start, requested.end, size
        ))
    {
        headers.insert(header::CONTENT_RANGE, value);
    }
    Ok(response)
}

/// Builds the destination path for a new media-library file without requiring
/// the target file to exist yet.
pub fn media_destination_path_under_root(
    media_dir: &str,
    filename: &str,
) -> Result<std::path::PathBuf, StatusCode> {
    // Destination paths validate the requested filename first, then join under
    // the canonical media root without requiring the target file to exist yet.
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

/// Streams one media file from the library, supporting byte ranges for video
/// playback clients.
pub async fn media_file_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(filename): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let path = match media_path_under_root(&state.media_dir, &filename) {
        Ok(path) => path,
        Err(status) => return status.into_response(),
    };
    match media_file_response(path, &filename, headers.get(header::RANGE)).await {
        Ok(response) => response,
        Err(_) => (StatusCode::NOT_FOUND, "File not found").into_response(),
    }
}

/// Deletes one media file after the service layer confirms it is safe to
/// remove any ingest or recording companions tied to it.
pub async fn media_delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(filename): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let canonical_path = match media_path_under_root(&state.media_dir, &filename) {
        Ok(path) => path,
        Err(status) => return media_directory_error_response(status),
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
        Err(MediaDeleteError::Dependency(error)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                serde_json::json!({"error": format!("Failed to check media references: {error}")}),
            ),
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

/// Renames one media file within the library while preserving extension rules
/// and companion artifact consistency.
pub async fn media_rename_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(filename): Path<String>,
    Json(payload): Json<MediaRenamePayload>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
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
        Err(status) => return media_directory_error_response(status),
    };
    let destination_path = match media_destination_path_under_root(&state.media_dir, new_name) {
        Ok(path) => path,
        Err(status) => return media_directory_error_response(status),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_media_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("restream-api-media-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn relative_media_dir(name: &str) -> String {
        let path = format!(
            "target/tmp/restream-api-media-{name}-{}",
            std::process::id()
        );
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn media_destination_path_uses_canonical_root_for_relative_media_dir() {
        let media_dir = relative_media_dir("relative-rename");
        let canonical_root = std::fs::canonicalize(&media_dir).unwrap();

        let destination = media_destination_path_under_root(&media_dir, "renamed.mp4").unwrap();

        assert_eq!(destination, canonical_root.join("renamed.mp4"));
        let _ = std::fs::remove_dir_all(media_dir);
    }

    #[test]
    fn media_destination_path_rejects_traversal() {
        let media_dir = temp_media_dir("rename-traversal");

        let err = media_destination_path_under_root(media_dir.to_str().unwrap(), "../clip.mp4")
            .unwrap_err();

        assert_eq!(err, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(media_dir);
    }

    #[test]
    fn media_directory_error_response_preserves_not_found_status() {
        let response = media_directory_error_response(StatusCode::NOT_FOUND);

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn recording_state_response_uses_ok_status() {
        let response = recording_state_response(true, false);

        assert_eq!(response.status(), StatusCode::OK);
    }
}
