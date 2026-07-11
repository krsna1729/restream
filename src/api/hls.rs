use axum::{
    Json,
    extract::{OriginalUri, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::Arc;

use super::state::{AppState, require_hls_access};
use crate::application::hls_preview::{self, HlsPreviewReadError};

pub async fn hls_playlist_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_hls_access(&state, &headers, &uri).await {
        return response;
    }

    playlist_response(hls_preview::primary_playlist(state.engine.clone(), &pipeline_id).await)
}

pub async fn hls_master_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_hls_access(&state, &headers, &uri).await {
        return response;
    }
    playlist_response(hls_preview::master_playlist(state.engine.clone(), &pipeline_id).await)
}

pub async fn hls_video_playlist_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_hls_access(&state, &headers, &uri).await {
        return response;
    }
    playlist_response(hls_preview::video_playlist(state.engine.clone(), &pipeline_id).await)
}

pub async fn hls_audio_playlist_handler(
    State(state): State<Arc<AppState>>,
    Path((pipeline_id, track_index)): Path<(String, u32)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_hls_access(&state, &headers, &uri).await {
        return response;
    }
    playlist_response(
        hls_preview::audio_playlist(state.engine.clone(), &pipeline_id, track_index).await,
    )
}

pub async fn hls_segment_handler(
    State(state): State<Arc<AppState>>,
    Path((pipeline_id, segment)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_hls_access(&state, &headers, &uri).await {
        return response;
    }

    media_response(
        hls_preview::video_segment(state.engine.clone(), &pipeline_id, &segment).await,
        "video/mp4",
    )
}

pub async fn hls_video_segment_handler(
    State(state): State<Arc<AppState>>,
    Path((pipeline_id, segment)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_hls_access(&state, &headers, &uri).await {
        return response;
    }
    media_response(
        hls_preview::video_segment(state.engine.clone(), &pipeline_id, &segment).await,
        "video/mp4",
    )
}

pub async fn hls_video_init_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_hls_access(&state, &headers, &uri).await {
        return response;
    }
    media_response(
        hls_preview::video_init_segment(state.engine.clone(), &pipeline_id).await,
        "video/mp4",
    )
}

pub async fn hls_audio_segment_handler(
    State(state): State<Arc<AppState>>,
    Path((pipeline_id, track_index, segment)): Path<(String, u32, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_hls_access(&state, &headers, &uri).await {
        return response;
    }
    media_response(
        hls_preview::audio_segment(state.engine.clone(), &pipeline_id, track_index, &segment).await,
        "audio/mp4",
    )
}

pub async fn hls_audio_init_handler(
    State(state): State<Arc<AppState>>,
    Path((pipeline_id, track_index)): Path<(String, u32)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_hls_access(&state, &headers, &uri).await {
        return response;
    }
    media_response(
        hls_preview::audio_init_segment(state.engine.clone(), &pipeline_id, track_index).await,
        "audio/mp4",
    )
}

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

fn media_response(
    result: Result<bytes::Bytes, HlsPreviewReadError>,
    content_type: &'static str,
) -> Response {
    match result {
        Ok(data) => (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], data).into_response(),
        Err(err) => hls_preview_error_response(err),
    }
}

fn hls_preview_error_response(err: HlsPreviewReadError) -> Response {
    match err {
        HlsPreviewReadError::NoStream => {
            json_error_response(StatusCode::NOT_FOUND, "hlsNoStream", "No HLS stream", None)
        }
        HlsPreviewReadError::NoSegments { blocked_by } => {
            if let Some(cause) = blocked_by {
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
