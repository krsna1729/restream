use axum::{
    extract::{OriginalUri, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
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
        HlsPreviewReadError::NoStream => (StatusCode::NOT_FOUND, "No HLS stream").into_response(),
        HlsPreviewReadError::NoSegments { blocked_by } => {
            if let Some(cause) = blocked_by {
                (
                    StatusCode::NOT_FOUND,
                    format!(
                        "No segments yet: blocked by video stage: {} (phase: {})",
                        cause.stage, cause.phase
                    ),
                )
                    .into_response()
            } else {
                (StatusCode::NOT_FOUND, "No segments yet").into_response()
            }
        }
        HlsPreviewReadError::AudioTrackNotFound => {
            (StatusCode::NOT_FOUND, "Audio track not found").into_response()
        }
        HlsPreviewReadError::InvalidSegmentName => {
            (StatusCode::BAD_REQUEST, "Invalid segment name").into_response()
        }
        HlsPreviewReadError::SegmentNotFound => {
            (StatusCode::NOT_FOUND, "Segment not found").into_response()
        }
        HlsPreviewReadError::InitSegmentNotFound => {
            (StatusCode::NOT_FOUND, "No init segment").into_response()
        }
    }
}
