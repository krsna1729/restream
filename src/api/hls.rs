//! HLS preview HTTP handlers expose playlists and media segments while keeping
//! access checks and transport-specific error mapping at the API boundary.
//! The preview service remains responsible for reading playlist/segment data.

use axum::{
    Json,
    extract::{OriginalUri, Path, State},
    http::{HeaderMap, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{future::Future, sync::Arc};

use super::state::{AppState, require_hls_access};
use crate::application::hls_preview::{self, HlsPreviewReadError};

/// Serves the single-playlist HLS view for one pipeline.
pub async fn hls_playlist_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    playlist_with_hls_access(&state, &headers, &uri, || async {
        hls_preview::primary_playlist(state.engine.clone(), &pipeline_id).await
    })
    .await
}

/// Serves the multivariant/master HLS playlist for one pipeline.
pub async fn hls_master_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    playlist_with_hls_access(&state, &headers, &uri, || async {
        hls_preview::master_playlist(state.engine.clone(), &pipeline_id).await
    })
    .await
}

/// Serves the video media playlist for one pipeline's HLS preview.
pub async fn hls_video_playlist_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    playlist_with_hls_access(&state, &headers, &uri, || async {
        hls_preview::video_playlist(state.engine.clone(), &pipeline_id).await
    })
    .await
}

/// Serves one audio-track media playlist for a pipeline's HLS preview.
pub async fn hls_audio_playlist_handler(
    State(state): State<Arc<AppState>>,
    Path((pipeline_id, track_index)): Path<(String, u32)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    playlist_with_hls_access(&state, &headers, &uri, || async {
        hls_preview::audio_playlist(state.engine.clone(), &pipeline_id, track_index).await
    })
    .await
}

/// Backward-compatible alias for the default video segment route.
pub async fn hls_segment_handler(
    State(state): State<Arc<AppState>>,
    Path((pipeline_id, segment)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    media_with_hls_access(&state, &headers, &uri, "video/mp4", || async {
        hls_preview::video_segment(state.engine.clone(), &pipeline_id, &segment).await
    })
    .await
}

/// Serves one video media segment from the HLS preview surface.
pub async fn hls_video_segment_handler(
    State(state): State<Arc<AppState>>,
    Path((pipeline_id, segment)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    media_with_hls_access(&state, &headers, &uri, "video/mp4", || async {
        hls_preview::video_segment(state.engine.clone(), &pipeline_id, &segment).await
    })
    .await
}

/// Serves the MP4 init segment for the pipeline's video rendition.
pub async fn hls_video_init_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    media_with_hls_access(&state, &headers, &uri, "video/mp4", || async {
        hls_preview::video_init_segment(state.engine.clone(), &pipeline_id).await
    })
    .await
}

/// Serves one audio media segment for the selected HLS audio track.
pub async fn hls_audio_segment_handler(
    State(state): State<Arc<AppState>>,
    Path((pipeline_id, track_index, segment)): Path<(String, u32, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    media_with_hls_access(&state, &headers, &uri, "audio/mp4", || async {
        hls_preview::audio_segment(state.engine.clone(), &pipeline_id, track_index, &segment).await
    })
    .await
}

/// Serves the MP4 init segment for one HLS audio rendition.
pub async fn hls_audio_init_handler(
    State(state): State<Arc<AppState>>,
    Path((pipeline_id, track_index)): Path<(String, u32)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    media_with_hls_access(&state, &headers, &uri, "audio/mp4", || async {
        hls_preview::audio_init_segment(state.engine.clone(), &pipeline_id, track_index).await
    })
    .await
}

pub async fn input_hls_master_handler(
    State(state): State<Arc<AppState>>,
    Path(input_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    playlist_with_hls_access(&state, &headers, &uri, || async {
        hls_preview::input_master_playlist(state.engine.clone(), &input_id).await
    })
    .await
}

pub async fn input_hls_video_playlist_handler(
    State(state): State<Arc<AppState>>,
    Path(input_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    playlist_with_hls_access(&state, &headers, &uri, || async {
        hls_preview::input_video_playlist(state.engine.clone(), &input_id).await
    })
    .await
}

pub async fn input_hls_video_init_handler(
    State(state): State<Arc<AppState>>,
    Path(input_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    media_with_hls_access(&state, &headers, &uri, "video/mp4", || async {
        hls_preview::input_video_init_segment(state.engine.clone(), &input_id).await
    })
    .await
}

pub async fn input_hls_video_segment_handler(
    State(state): State<Arc<AppState>>,
    Path((input_id, segment)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    media_with_hls_access(&state, &headers, &uri, "video/mp4", || async {
        hls_preview::input_video_segment(state.engine.clone(), &input_id, &segment).await
    })
    .await
}

pub async fn input_hls_audio_playlist_handler(
    State(state): State<Arc<AppState>>,
    Path((input_id, track_index)): Path<(String, u32)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    playlist_with_hls_access(&state, &headers, &uri, || async {
        hls_preview::input_audio_playlist(state.engine.clone(), &input_id, track_index).await
    })
    .await
}

pub async fn input_hls_audio_init_handler(
    State(state): State<Arc<AppState>>,
    Path((input_id, track_index)): Path<(String, u32)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    media_with_hls_access(&state, &headers, &uri, "audio/mp4", || async {
        hls_preview::input_audio_init_segment(state.engine.clone(), &input_id, track_index).await
    })
    .await
}

pub async fn input_hls_audio_segment_handler(
    State(state): State<Arc<AppState>>,
    Path((input_id, track_index, segment)): Path<(String, u32, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    media_with_hls_access(&state, &headers, &uri, "audio/mp4", || async {
        hls_preview::input_audio_segment(state.engine.clone(), &input_id, track_index, &segment)
            .await
    })
    .await
}

// Keep the auth gate next to transport response shaping so HLS handlers only
// describe which playlist source they expose.
async fn playlist_with_hls_access<F, Fut>(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    uri: &Uri,
    load_playlist: F,
) -> Response
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, HlsPreviewReadError>>,
{
    if let Some(response) = require_hls_access(state, headers, uri).await {
        return response;
    }

    playlist_response(load_playlist().await)
}

// Media endpoints share the same access gate and error envelope; only the
// underlying preview read and content type vary by route.
async fn media_with_hls_access<F, Fut>(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    uri: &Uri,
    content_type: &'static str,
    load_media: F,
) -> Response
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<bytes::Bytes, HlsPreviewReadError>>,
{
    if let Some(response) = require_hls_access(state, headers, uri).await {
        return response;
    }

    media_response(load_media().await, content_type)
}

// Playlist endpoints always return the Apple playlist content type and map the
// preview-layer read error into the shared HLS JSON error contract.
fn playlist_response(result: Result<String, HlsPreviewReadError>) -> Response {
    match result {
        Ok(playlist) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
            playlist,
        )
            .into_response(),
        Err(err) => hls_preview_error_response(err),
    }
}

// Segment/init endpoints vary only by content type; the preview error envelope
// stays identical across the HLS media routes.
fn media_response(
    result: Result<bytes::Bytes, HlsPreviewReadError>,
    content_type: &'static str,
) -> Response {
    match result {
        Ok(data) => (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], data).into_response(),
        Err(err) => hls_preview_error_response(err),
    }
}

// Keep the HLS preview read errors mapped here so route handlers stay focused
// on which preview artifact they expose rather than transport error shaping.
fn hls_preview_error_response(err: HlsPreviewReadError) -> Response {
    match err {
        HlsPreviewReadError::NoStream => {
            json_error_response(StatusCode::NOT_FOUND, "hlsNoStream", "No HLS stream", None)
        }
        HlsPreviewReadError::NoSegments { blocked_by } => {
            if let Some(cause) = blocked_by {
                // Bubble the blocked stage back to the dashboard so operators can
                // tell whether preview is waiting on an upstream video phase.
                json_error_response(
                    StatusCode::NOT_FOUND,
                    "hlsNoSegments",
                    format!(
                        "No segments yet: blocked by video stage: {} (phase: {})",
                        cause.stage, cause.phase
                    ),
                    Some(json!({
                        "blockedBy": {
                            "stage": cause.stage.to_string(),
                            "phase": cause.phase.to_string(),
                        }
                    })),
                )
            } else {
                json_error_response(
                    StatusCode::NOT_FOUND,
                    "hlsNoSegments",
                    "No segments yet",
                    None,
                )
            }
        }
        HlsPreviewReadError::AudioTrackNotFound => json_error_response(
            StatusCode::NOT_FOUND,
            "hlsAudioTrackNotFound",
            "Audio track not found",
            None,
        ),
        HlsPreviewReadError::InvalidSegmentName => json_error_response(
            StatusCode::BAD_REQUEST,
            "hlsInvalidSegmentName",
            "Invalid segment name",
            None,
        ),
        HlsPreviewReadError::SegmentNotFound => json_error_response(
            StatusCode::NOT_FOUND,
            "hlsSegmentNotFound",
            "Segment not found",
            None,
        ),
        HlsPreviewReadError::InitSegmentNotFound => json_error_response(
            StatusCode::NOT_FOUND,
            "hlsInitSegmentNotFound",
            "No init segment",
            None,
        ),
    }
}

fn json_error_response(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
    extra: Option<Value>,
) -> Response {
    // Keep HLS errors on the same JSON envelope as the rest of the dashboard API
    // while still allowing endpoint-specific metadata to be attached when useful.
    let mut body = json!({
        "error": message.into(),
        "status": status.as_u16(),
        "code": code,
    });
    if let (Value::Object(body), Some(Value::Object(extra))) = (&mut body, extra) {
        body.extend(extra);
    }

    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        Json(body),
    )
        .into_response()
}
