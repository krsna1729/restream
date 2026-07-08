use axum::{
    extract::{Path, State, OriginalUri},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use crate::media::hls_fmp4::{Fmp4HlsStore, parse_fmp4_segment_name};
use crate::planner::hls_preview::{HlsPreviewGraph, plan_hls_preview};

use super::state::{AppState, require_hls_access};

pub async fn get_or_start_hls_preview_store(
    state: &Arc<AppState>,
    pipeline_id: &str,
) -> Result<Arc<Fmp4HlsStore>, Response> {
    let has_ingest = state
        .engine
        .ingests
        .active
        .read()
        .await
        .contains_key(pipeline_id);
    if has_ingest {
        let (store, already_running) = state.engine.ensure_hls_preview_segmenter(pipeline_id).await;
        if !already_running {
            let engine_c = state.engine.clone();
            let pid = pipeline_id.to_string();
            let cancel_token = state
                .engine
                .get_hls_preview_cancel_token(pipeline_id)
                .await
                .unwrap();
            let graph =
                match plan_hls_preview(state.engine.clone(), pipeline_id, cancel_token.clone())
                    .await
                {
                    Some(g) => g,
                    None => HlsPreviewGraph {
                        video_ring: state.engine.get_or_create_pipeline(pipeline_id).await,
                        audio_ring: None,
                        video_meta: None,
                    },
                };
            let store_c = store.clone();
            tokio::spawn(async move {
                crate::media::hls_fmp4::start_hls_fmp4_segmenter(
                    pid.clone(),
                    store_c,
                    graph.video_ring,
                    graph.audio_ring,
                    engine_c.clone(),
                    cancel_token,
                    graph.video_meta,
                )
                .await;
                engine_c.shutdown_hls_preview_segmenter(&pid).await;
            });
        }
        state.engine.touch_hls_preview(pipeline_id).await;
        return Ok(store);
    }

    let Some(store) = state.engine.get_hls_preview_store(pipeline_id).await else {
        return Err((StatusCode::NOT_FOUND, "No HLS stream").into_response());
    };
    state.engine.touch_hls_preview(pipeline_id).await;
    Ok(store)
}

pub fn quote_hls_attr(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn build_hls_master_playlist(
    video: Option<&crate::media::engine::VideoMeta>,
    audio_tracks: &[crate::media::engine::AudioMeta],
) -> String {
    let mut playlist = "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-INDEPENDENT-SEGMENTS\n".to_string();
    if !audio_tracks.is_empty() {
        for (ordinal, track) in audio_tracks.iter().enumerate() {
            let mut media_attrs = vec![
                "TYPE=AUDIO".to_string(),
                format!("GROUP-ID={}", quote_hls_attr("audio")),
                format!(
                    "NAME={}",
                    quote_hls_attr(&build_hls_audio_track_name(track, ordinal))
                ),
                format!("DEFAULT={}", if ordinal == 0 { "YES" } else { "NO" }),
                "AUTOSELECT=YES".to_string(),
                format!(
                    "URI={}",
                    quote_hls_attr(&format!("audio/{}/index.m3u8", track.track_index))
                ),
            ];
            if let Some(language) = track
                .language
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                media_attrs.push(format!("LANGUAGE={}", quote_hls_attr(language)));
            }
            if track.channels > 0 {
                media_attrs.push(format!(
                    "CHANNELS={}",
                    quote_hls_attr(&track.channels.to_string())
                ));
            }
            playlist.push_str(&format!("#EXT-X-MEDIA:{}\n", media_attrs.join(",")));
        }
    }

    let bandwidth = estimate_hls_master_bandwidth(video, audio_tracks);
    let mut stream_attrs = vec![
        format!("BANDWIDTH={bandwidth}"),
        format!("AVERAGE-BANDWIDTH={bandwidth}"),
    ];
    if let Some(video) = video {
        if video.width > 0 && video.height > 0 {
            stream_attrs.push(format!("RESOLUTION={}x{}", video.width, video.height));
        }
        if video.fps.is_finite() && video.fps > 0.0 {
            stream_attrs.push(format!("FRAME-RATE={:.3}", video.fps));
        }
    }
    if let Some(codecs) = build_hls_codec_list(video, audio_tracks) {
        stream_attrs.push(format!("CODECS={}", quote_hls_attr(&codecs)));
    }
    if !audio_tracks.is_empty() {
        stream_attrs.push(format!("AUDIO={}", quote_hls_attr("audio")));
    }
    playlist.push_str(&format!("#EXT-X-STREAM-INF:{}\n", stream_attrs.join(",")));
    if video.is_some() {
        playlist.push_str("video/index.m3u8\n");
    } else if let Some(track) = audio_tracks.first() {
        playlist.push_str(&format!("audio/{}/index.m3u8\n", track.track_index));
    } else {
        playlist.push_str("index.m3u8\n");
    }
    playlist
}

pub fn build_hls_audio_track_name(track: &crate::media::engine::AudioMeta, ordinal: usize) -> String {
    if let Some(title) = track
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return title.to_string();
    }
    let base = format!("Track {}", ordinal + 1);
    match track
        .language
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(language) => format!("{base} ({language})"),
        None => base,
    }
}

pub fn estimate_hls_master_bandwidth(
    video: Option<&crate::media::engine::VideoMeta>,
    audio_tracks: &[crate::media::engine::AudioMeta],
) -> u64 {
    let video_bw = video
        .and_then(|meta| meta.bw)
        .filter(|bw| bw.is_finite() && *bw > 0.0)
        .map(|bw| bw.round() as u64);
    let audio_bw = audio_tracks
        .iter()
        .map(estimate_audio_bandwidth)
        .sum::<u64>();
    let fallback_bw = 8_000_000u64.saturating_add(audio_bw);
    video_bw
        .map(|bw| bw.saturating_add(audio_bw))
        .unwrap_or(fallback_bw)
        .max(1)
}

pub fn estimate_audio_bandwidth(track: &crate::media::engine::AudioMeta) -> u64 {
    match track.codec.to_ascii_lowercase().as_str() {
        "aac" => match track.channels {
            0 | 1 => 96_000,
            2 => 128_000,
            _ => 192_000,
        },
        "mp3" => 128_000,
        "opus" => match track.channels {
            0 | 1 => 64_000,
            2 => 128_000,
            _ => 160_000,
        },
        _ => 128_000,
    }
}

pub fn build_hls_codec_list(
    video: Option<&crate::media::engine::VideoMeta>,
    audio_tracks: &[crate::media::engine::AudioMeta],
) -> Option<String> {
    let mut codecs = Vec::new();
    if let Some(video) = video.and_then(build_hls_video_codec) {
        codecs.push(video);
    }
    for codec in audio_tracks.iter().filter_map(build_hls_audio_codec) {
        if !codecs.iter().any(|existing| existing == &codec) {
            codecs.push(codec);
        }
    }
    (!codecs.is_empty()).then(|| codecs.join(","))
}

pub fn build_hls_video_codec(video: &crate::media::engine::VideoMeta) -> Option<String> {
    let codec = video.codec.trim().to_ascii_lowercase();
    match codec.as_str() {
        "h264" | "avc" => build_h264_codec_string(video),
        "hevc" | "h265" => Some(build_hevc_codec_string(video)),
        "av1" => Some("av01.0.08M.08".to_string()),
        _ => None,
    }
}

pub fn build_h264_codec_string(video: &crate::media::engine::VideoMeta) -> Option<String> {
    let profile_idc = match video.profile.as_deref().map(str::trim) {
        Some("Baseline") => 66u8,
        Some("Main") => 77u8,
        Some("Extended") => 88u8,
        Some("High") => 100u8,
        Some("High 10") => 110u8,
        Some("High 4:2:2") => 122u8,
        Some("High 4:4:4 Predictive") => 244u8,
        _ => 100u8,
    };
    let level = parse_h264_level_idc(video.level.as_deref())
        .unwrap_or_else(|| estimate_h264_level_idc(video));
    Some(format!("avc1.{profile_idc:02x}00{level:02x}"))
}

pub fn estimate_h264_level_idc(video: &crate::media::engine::VideoMeta) -> u8 {
    let width = video.width.max(1);
    let height = video.height.max(1);
    let fps = if video.fps.is_finite() && video.fps > 0.0 {
        video.fps
    } else {
        30.0
    };
    let macroblocks_per_frame = width.div_ceil(16).saturating_mul(height.div_ceil(16));
    let macroblocks_per_second = macroblocks_per_frame as f64 * fps;

    if width > 1280 || height > 720 || macroblocks_per_second > 216_000.0 {
        40
    } else if macroblocks_per_second > 108_000.0 {
        32
    } else {
        31
    }
}

pub fn parse_h264_level_idc(level: Option<&str>) -> Option<u8> {
    let level = level?.trim();
    if level.is_empty() {
        return None;
    }
    let (major, minor) = match level.split_once('.') {
        Some((major, minor)) => (major.trim(), minor.trim()),
        None => (level, "0"),
    };
    let major: u8 = major.parse().ok()?;
    let minor: u8 = minor.parse().ok()?;
    Some(major.saturating_mul(10).saturating_add(minor))
}

pub fn build_hevc_codec_string(video: &crate::media::engine::VideoMeta) -> String {
    let profile = match video.profile.as_deref().map(str::trim) {
        Some("Main") => 1u8,
        Some("Main 10") => 2u8,
        Some("Main Still Picture") => 3u8,
        _ => 1u8,
    };
    let level_tenths = video
        .level
        .as_deref()
        .and_then(parse_h265_level_tenths)
        .unwrap_or(120);
    let general_level_idc = level_tenths.saturating_mul(3);
    format!("hvc1.{profile}.6.L{general_level_idc}.B0")
}

pub fn parse_h265_level_tenths(level: &str) -> Option<u8> {
    let level = level.trim();
    if level.is_empty() {
        return None;
    }
    let (major, minor) = match level.split_once('.') {
        Some((major, minor)) => (major.trim(), minor.trim()),
        None => (level, "0"),
    };
    let major: u8 = major.parse().ok()?;
    let minor: u8 = minor.parse().ok()?;
    Some(major.saturating_mul(10).saturating_add(minor))
}

pub fn build_hls_audio_codec(track: &crate::media::engine::AudioMeta) -> Option<String> {
    let codec = track.codec.trim().to_ascii_lowercase();
    match codec.as_str() {
        "aac" => Some(match track.profile.as_deref().map(str::trim) {
            Some("Main") => "mp4a.40.1".to_string(),
            Some("SSR") => "mp4a.40.3".to_string(),
            Some("LTP/Reserved") => "mp4a.40.4".to_string(),
            _ => "mp4a.40.2".to_string(),
        }),
        "mp3" => Some("mp4a.40.34".to_string()),
        "opus" => Some("opus".to_string()),
        _ => None,
    }
}

pub async fn hls_playlist_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_hls_access(&state, &headers, &uri).await {
        return response;
    }

    let store = match get_or_start_hls_preview_store(&state, &pipeline_id).await {
        Ok(store) => store,
        Err(response) => return response,
    };
    match store.get_primary_playlist() {
        Some(playlist) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
            playlist,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "No segments yet").into_response(),
    }
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
    let store = match get_or_start_hls_preview_store(&state, &pipeline_id).await {
        Ok(store) => store,
        Err(response) => return response,
    };
    if !store.has_video_playlist() && store.get_primary_playlist().is_none() {
        return (StatusCode::NOT_FOUND, "No segments yet").into_response();
    }
    let (video, audio_tracks) = store.stream_metadata();
    let playlist = build_hls_master_playlist(video.as_ref(), &audio_tracks);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        playlist,
    )
        .into_response()
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
    let store = match get_or_start_hls_preview_store(&state, &pipeline_id).await {
        Ok(store) => store,
        Err(response) => return response,
    };
    match store
        .get_video_playlist()
        .or_else(|| store.get_primary_playlist())
    {
        Some(playlist) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
            playlist,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "No segments yet").into_response(),
    }
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
    let store = match get_or_start_hls_preview_store(&state, &pipeline_id).await {
        Ok(store) => store,
        Err(response) => return response,
    };
    let (_, audio_tracks) = store.stream_metadata();
    if !audio_tracks
        .iter()
        .any(|track| track.track_index == track_index)
    {
        return (StatusCode::NOT_FOUND, "Audio track not found").into_response();
    }
    match store.get_audio_playlist(track_index) {
        Some(playlist) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
            playlist,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "No segments yet").into_response(),
    }
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

    state.engine.touch_hls_preview(&pipeline_id).await;
    let Some(store) = state.engine.get_hls_preview_store(&pipeline_id).await else {
        return (StatusCode::NOT_FOUND, "No HLS stream").into_response();
    };
    let Some(index) = parse_fmp4_segment_name(&segment) else {
        return (StatusCode::BAD_REQUEST, "Invalid segment name").into_response();
    };
    match store.get_video_segment(index) {
        Some(data) => (StatusCode::OK, [(header::CONTENT_TYPE, "video/mp4")], data).into_response(),
        None => (StatusCode::NOT_FOUND, "Segment not found").into_response(),
    }
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
    state.engine.touch_hls_preview(&pipeline_id).await;
    let Some(store) = state.engine.get_hls_preview_store(&pipeline_id).await else {
        return (StatusCode::NOT_FOUND, "No HLS stream").into_response();
    };
    let Some(index) = parse_fmp4_segment_name(&segment) else {
        return (StatusCode::BAD_REQUEST, "Invalid segment name").into_response();
    };
    match store.get_video_segment(index) {
        Some(data) => (StatusCode::OK, [(header::CONTENT_TYPE, "video/mp4")], data).into_response(),
        None => (StatusCode::NOT_FOUND, "Segment not found").into_response(),
    }
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
    state.engine.touch_hls_preview(&pipeline_id).await;
    let Some(store) = state.engine.get_hls_preview_store(&pipeline_id).await else {
        return (StatusCode::NOT_FOUND, "No HLS stream").into_response();
    };
    match store.get_video_init_segment() {
        Some(data) => (StatusCode::OK, [(header::CONTENT_TYPE, "video/mp4")], data).into_response(),
        None => (StatusCode::NOT_FOUND, "No init segment").into_response(),
    }
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
    state.engine.touch_hls_preview(&pipeline_id).await;
    let Some(store) = state.engine.get_hls_preview_store(&pipeline_id).await else {
        return (StatusCode::NOT_FOUND, "No HLS stream").into_response();
    };
    let Some(index) = parse_fmp4_segment_name(&segment) else {
        return (StatusCode::BAD_REQUEST, "Invalid segment name").into_response();
    };
    match store.get_audio_segment(track_index, index) {
        Some(data) => (StatusCode::OK, [(header::CONTENT_TYPE, "audio/mp4")], data).into_response(),
        None => (StatusCode::NOT_FOUND, "Segment not found").into_response(),
    }
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
    state.engine.touch_hls_preview(&pipeline_id).await;
    let Some(store) = state.engine.get_hls_preview_store(&pipeline_id).await else {
        return (StatusCode::NOT_FOUND, "No HLS stream").into_response();
    };
    match store.get_audio_init_segment(track_index) {
        Some(data) => (StatusCode::OK, [(header::CONTENT_TYPE, "audio/mp4")], data).into_response(),
        None => (StatusCode::NOT_FOUND, "No init segment").into_response(),
    }
}
