use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::process::{ChildStderr, ChildStdout};
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::application::ingest::{ResolveFileIngestError, resolve_file_ingest_context};
use crate::application::ports::{SqliteIngestLookup, SqlitePipelineStore};
use crate::application::services::{
    ApiError, FileIngestService, file_ingest_service::SpawnedFileIngestChild,
};
use crate::types::{Ingest, Pipeline};

use super::state::{
    AppState, MAX_NAME_LEN, MAX_STREAM_KEY_LEN, check_field_len, get_session_token_from_headers,
    require_authenticated, to_hex,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestPayload {
    pub filename: String,
    pub stream_key: String,
    #[serde(alias = "loop")]
    pub loop_flag: Option<bool>,
    pub start_time: Option<String>,
    pub live_optimized: Option<bool>,
    pub target_gop_seconds: Option<u32>,
}

pub fn sanitize_target_gop_seconds(value: Option<u32>) -> u32 {
    value
        .unwrap_or(crate::types::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS)
        .max(1)
}

async fn pump_file_ingest_stdout(
    state: Arc<AppState>,
    pipeline: Pipeline,
    ring_buffer: Arc<crate::media::ring_buffer::RingBuffer>,
    mut stdout: ChildStdout,
    cancel: CancellationToken,
    timestamps: &mut crate::media::file_ingest::ContinuousTimestampState,
) -> Result<(), String> {
    let (bytes_received, ingest_metrics, cached_keyframe_times) = {
        state
            .engine
            .with_active_ingest(&pipeline.id, |ingest| {
                (
                    ingest.bytes_received.clone(),
                    ingest.metrics.clone(),
                    ingest.keyframe_times.clone(),
                )
            })
            .await
            .ok_or_else(|| format!("Active ingest missing for pipeline {}", pipeline.id))?
    };

    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut packets = Vec::with_capacity(crate::media::MEDIA_PRODUCER_BATCH_PACKETS);
    let mut probe_sent = false;
    let mut buf = vec![0u8; 64 * 1024];

    loop {
        let read = tokio::select! {
            _ = cancel.cancelled() => break,
            res = stdout.read(&mut buf) => res,
        }
        .map_err(|e| format!("Failed to read ffmpeg stdout: {e}"))?;

        if read == 0 {
            break;
        }

        demuxer.feed(&buf[..read]);
        if demuxer.drain_into(&mut packets) > 0 {
            for pkt in &mut packets {
                timestamps.apply(pkt);
                if pkt.media_type == crate::media::ring_buffer::MediaType::Video
                    && let Some(parameter_sets) =
                        crate::media::codec::annexb_parameter_sets(&pkt.payload)
                {
                    ring_buffer.set_video_parameter_sets(parameter_sets);
                }
                if pkt.media_type == crate::media::ring_buffer::MediaType::Video && pkt.is_keyframe
                {
                    let mut times = cached_keyframe_times
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    times.push(pkt.pts);
                    if times.len() > 30 {
                        times.remove(0);
                    }
                }
            }
            ring_buffer.push_drained_batch_capped(&mut packets);
        }

        if !probe_sent && let Some(probe) = demuxer.take_probe() {
            probe_sent = true;
            let first_audio = probe.audio_tracks.first().cloned();
            let video_sequence_header = probe.video_sequence_header.clone();
            let selected_video_track_index = probe.video.as_ref().map(|_| 0);
            state
                .engine
                .update_ingest_meta(&pipeline.id, probe.video, first_audio, None)
                .await;
            if let Some(sequence_header) = video_sequence_header {
                state
                    .engine
                    .cache_sequence_header(&pipeline.id, true, sequence_header)
                    .await;
            }
            state
                .engine
                .update_ingest_video_track_selection(
                    &pipeline.id,
                    probe.video_track_count,
                    selected_video_track_index,
                )
                .await;
            if !probe.audio_tracks.is_empty() {
                state
                    .engine
                    .update_ingest_audio_tracks(&pipeline.id, probe.audio_tracks)
                    .await;
            }
        }

        bytes_received.fetch_add(read as u64, std::sync::atomic::Ordering::Relaxed);
        ingest_metrics.record_in(read as u64);
    }

    Ok(())
}

pub async fn log_file_ingest_stderr(
    ingest_id: &str,
    mut stderr: ChildStderr,
) -> Result<(), std::io::Error> {
    const STDERR_CAP: usize = 64 * 1024;
    let mut buf = [0u8; 4096];
    let mut all = Vec::new();
    let mut truncated = false;

    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let remaining = STDERR_CAP.saturating_sub(all.len());
                if remaining > 0 {
                    all.extend_from_slice(&buf[..n.min(remaining)]);
                } else if !truncated {
                    truncated = true;
                    warn!(ingest_id = %ingest_id, cap = STDERR_CAP, "ffmpeg stderr truncated");
                }
            }
            Err(e) => return Err(e),
        }
    }

    if !all.is_empty() {
        warn!(ingest_id = %ingest_id, stderr = %String::from_utf8_lossy(&all).trim(), "ffmpeg stderr");
    }

    Ok(())
}

pub async fn run_file_ingest_task(
    state: Arc<AppState>,
    ingest: Ingest,
    pipeline: Pipeline,
    file_path: PathBuf,
    ring_buffer: Arc<crate::media::ring_buffer::RingBuffer>,
    registration: crate::media::engine::IngestRegistration,
    mut spawned: SpawnedFileIngestChild,
) {
    let cancel = registration.cancel_token.clone();
    let mut timestamps = crate::media::file_ingest::ContinuousTimestampState::default();
    loop {
        state
            .engine
            .file_ingests
            .children
            .write()
            .await
            .insert(ingest.id.clone(), spawned.child);

        let stderr_id = ingest.id.clone();
        let stdout_fut = pump_file_ingest_stdout(
            state.clone(),
            pipeline.clone(),
            ring_buffer.clone(),
            spawned.stdout,
            cancel.clone(),
            &mut timestamps,
        );
        let stderr_fut = log_file_ingest_stderr(&stderr_id, spawned.stderr);
        let (stdout_res, stderr_res) = tokio::join!(stdout_fut, stderr_fut);

        let mut exit_status = None;
        if let Some(mut child) = state.engine.take_file_ingest_child(&ingest.id).await {
            exit_status = child.wait().await.ok();
        }

        if let Err(err) = stdout_res
            && !cancel.is_cancelled()
        {
            error!(ingest_id = %ingest.id, err = %err, "file-ingest stdout reader failed");
            state
                .engine
                .record_ingest_disconnect_if_current(
                    &pipeline.id,
                    &registration,
                    Some("stdout"),
                    Some(err.to_string()),
                    true,
                )
                .await;
        }
        if let Err(err) = stderr_res
            && !cancel.is_cancelled()
        {
            error!(ingest_id = %ingest.id, err = %err, "file-ingest stderr reader failed");
            state
                .engine
                .record_ingest_disconnect_if_current(
                    &pipeline.id,
                    &registration,
                    Some("stderr"),
                    Some(err.to_string()),
                    true,
                )
                .await;
        }

        if let Some(status) = exit_status
            && !status.success()
            && !cancel.is_cancelled()
        {
            warn!(ingest_id = %ingest.id, status = %status, "ffmpeg exited unsuccessfully");
            state
                .engine
                .record_ingest_disconnect_if_current(
                    &pipeline.id,
                    &registration,
                    Some("exit"),
                    Some(format!("ffmpeg exited with status {status}")),
                    true,
                )
                .await;
        } else if exit_status.is_some() && !cancel.is_cancelled() && !ingest.loop_flag {
            state
                .engine
                .record_ingest_disconnect_if_current(
                    &pipeline.id,
                    &registration,
                    Some("eof"),
                    Some("file ingest reached end of input".to_string()),
                    false,
                )
                .await;
        }

        if cancel.is_cancelled() || !ingest.loop_flag {
            break;
        }

        match FileIngestService::spawn_file_ingest_child(&ingest, &file_path) {
            Ok(next) => spawned = next,
            Err(err) => {
                error!(ingest_id = %ingest.id, err = %err, "file-ingest restart failed");
                break;
            }
        }
    }

    state.engine.clear_file_ingest_running(&ingest.id).await;
    state
        .engine
        .unregister_ingest_if_current(&pipeline.id, &registration)
        .await;
}

pub async fn ingests_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let ingests = state.ingest_service.list_ingests().await?;
    let mut res = Vec::new();
    for i in ingests {
        let running = state.engine.is_file_ingest_running(&i.id).await;
        res.push(serde_json::json!({
            "id": i.id,
            "filename": i.filename,
            "streamKey": i.stream_key,
            "loop": i.loop_flag,
            "startTime": i.start_time,
            "liveOptimized": i.live_optimized,
            "targetGopSeconds": i.target_gop_seconds,
            "running": running
        }));
    }
    Ok(Json(res).into_response())
}

pub async fn ingests_post_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<IngestPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    if let Some(r) = check_field_len("filename", &payload.filename, MAX_NAME_LEN) {
        return Ok(r);
    }
    if let Some(r) = check_field_len("stream_key", &payload.stream_key, MAX_STREAM_KEY_LEN) {
        return Ok(r);
    }
    if let Some(ref s) = payload.start_time
        && let Some(r) = check_field_len("start_time", s, 64)
    {
        return Ok(r);
    }
    let id = format!("ingest_{}", to_hex(&rand::random::<[u8; 8]>()));
    let loop_val = payload.loop_flag.unwrap_or(false);
    let start_time = payload.start_time.unwrap_or_default();
    let live_optimized = payload.live_optimized.unwrap_or(false);
    let target_gop_seconds = sanitize_target_gop_seconds(payload.target_gop_seconds);

    let ingest = state
        .ingest_service
        .create_ingest(
            &id,
            &payload.filename,
            &payload.stream_key,
            loop_val,
            &start_time,
            live_optimized,
            target_gop_seconds,
        )
        .await?;

    Ok(Json(serde_json::json!({
        "id": ingest.id,
        "filename": ingest.filename,
        "streamKey": ingest.stream_key,
        "loop": ingest.loop_flag,
        "startTime": ingest.start_time,
        "liveOptimized": ingest.live_optimized,
        "targetGopSeconds": ingest.target_gop_seconds,
        "running": false
    }))
    .into_response())
}

pub async fn ingests_update_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<IngestPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    if let Some(ref s) = payload.start_time
        && let Some(r) = check_field_len("start_time", s, 64)
    {
        return Ok(r);
    }
    let loop_val = payload.loop_flag.unwrap_or(false);
    let start_time = payload.start_time.unwrap_or_default();
    let live_optimized = payload.live_optimized.unwrap_or(false);
    let target_gop_seconds = sanitize_target_gop_seconds(payload.target_gop_seconds);

    let ingest = state
        .ingest_service
        .update_ingest(
            &id,
            &payload.filename,
            &payload.stream_key,
            loop_val,
            &start_time,
            live_optimized,
            target_gop_seconds,
        )
        .await?;

    let running = state.engine.is_file_ingest_running(&ingest.id).await;
    Ok(Json(serde_json::json!({
        "id": ingest.id,
        "filename": ingest.filename,
        "streamKey": ingest.stream_key,
        "loop": ingest.loop_flag,
        "startTime": ingest.start_time,
        "liveOptimized": ingest.live_optimized,
        "targetGopSeconds": ingest.target_gop_seconds,
        "running": running
    }))
    .into_response())
}

pub async fn ingests_delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    state
        .file_ingest_service
        .delete_ingest_with_runtime_cleanup(&state.engine, &id)
        .await;
    Json(serde_json::json!({"deleted": true})).into_response()
}

pub async fn ingests_start_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let resolved = match resolve_file_ingest_context(
        &SqliteIngestLookup::new(state.db.clone()),
        &SqlitePipelineStore::new(state.db.clone()),
        &id,
    )
    .await
    {
        Ok(Some(context)) => context,
        Ok(None) => return (StatusCode::NOT_FOUND, "Ingest not found").into_response(),
        Err(ResolveFileIngestError::MissingPipelineForStreamKey(_)) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "No pipeline found for stream key"})),
            )
                .into_response();
        }
        Err(ResolveFileIngestError::IngestLookup(_)) => {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(ResolveFileIngestError::PipelineStore(e)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to resolve pipeline: {e}")})),
            )
                .into_response();
        }
    };
    let ingest = resolved.ingest;
    let pipeline = resolved.pipeline;

    if state.engine.is_file_ingest_running(&id).await {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Ingest already running"})),
        )
            .into_response();
    }

    let file_path = FsPath::new(&state.media_dir).join(&ingest.filename);
    if !file_path.exists() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Media file not found"})),
        )
            .into_response();
    }

    let ring_buffer = state.engine.get_or_create_pipeline(&pipeline.id).await;
    let Some(registration) = state
        .engine
        .try_register_ingest_attempt(&pipeline.id, &ingest.stream_key, "file")
        .await
    else {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Pipeline already has an active ingest"})),
        )
            .into_response();
    };

    state.engine.mark_file_ingest_running(&ingest.id).await;

    if crate::media::file_ingest::use_internal_file_ingest(&state.engine.config)
        && !ingest.live_optimized
    {
        if let Err(e) = crate::media::file_ingest::spawn_internal_file_ingest(
            state.engine.clone(),
            tokio::runtime::Handle::current(),
            ingest.id.clone(),
            pipeline.id.clone(),
            file_path,
            ingest.start_time.clone(),
            ingest.loop_flag,
            ring_buffer,
            registration,
        ) {
            state.engine.clear_file_ingest_running(&ingest.id).await;
            state.engine.unregister_ingest(&pipeline.id).await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e})),
            )
                .into_response();
        }
    } else {
        let spawned = match FileIngestService::spawn_file_ingest_child(&ingest, &file_path) {
            Ok(child) => child,
            Err(e) => {
                state.engine.clear_file_ingest_running(&ingest.id).await;
                state.engine.unregister_ingest(&pipeline.id).await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": e})),
                )
                    .into_response();
            }
        };

        tokio::spawn(run_file_ingest_task(
            state.clone(),
            ingest.clone(),
            pipeline,
            file_path,
            ring_buffer,
            registration,
            spawned,
        ));
    }

    Json(serde_json::json!({
        "id": ingest.id,
        "filename": ingest.filename,
        "streamKey": ingest.stream_key,
        "loop": ingest.loop_flag,
        "startTime": ingest.start_time,
        "liveOptimized": ingest.live_optimized,
        "targetGopSeconds": ingest.target_gop_seconds,
        "running": true
    }))
    .into_response()
}

pub async fn ingests_stop_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let ingest = match state
        .file_ingest_service
        .stop_ingest_with_runtime_cleanup(&state.engine, &id)
        .await
    {
        Ok(ingest) => ingest,
        Err(ApiError::NotFound(_)) => {
            return (StatusCode::NOT_FOUND, "Ingest not found").into_response();
        }
        Err(ApiError::Internal(_)) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Err(ApiError::Conflict(_)) => return StatusCode::CONFLICT.into_response(),
    };

    Json(serde_json::json!({
        "id": ingest.id,
        "filename": ingest.filename,
        "streamKey": ingest.stream_key,
        "loop": ingest.loop_flag,
        "startTime": ingest.start_time,
        "liveOptimized": ingest.live_optimized,
        "targetGopSeconds": ingest.target_gop_seconds,
        "running": false
    }))
    .into_response()
}
