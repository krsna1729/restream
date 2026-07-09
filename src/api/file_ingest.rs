use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use std::sync::Arc;

use crate::api_view_models;
use crate::application::ingest::{
    FileIngestConfig, PersistFileIngestError, clear_stream_key_file_ingests,
    load_pipeline_file_ingest_state, persist_pipeline_file_ingest, remove_pipeline_file_ingest,
};
use crate::application::ports::{SqliteIngestLookup, SqlitePipelineStore};
use crate::application::services::ApiError;

use super::ingests::sanitize_target_gop_seconds;
use super::state::{
    AppState, MAX_FFMPEG_ARGS_LEN, MAX_NAME_LEN, check_field_len, require_authenticated, to_hex,
};

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PipelineFileIngestPayload {
    pub filename: String,
    #[serde(alias = "loop")]
    pub loop_flag: Option<bool>,
    pub start_time: Option<String>,
    pub live_optimized: Option<bool>,
    pub target_gop_seconds: Option<u32>,
}

pub fn validate_pipeline_file_ingest_payload(
    payload: &PipelineFileIngestPayload,
) -> Option<Response> {
    if let Some(r) = check_field_len("filename", &payload.filename, MAX_NAME_LEN) {
        return Some(r);
    }
    if let Some(ref start_time) = payload.start_time
        && let Some(r) = check_field_len("start_time", start_time, 64)
    {
        return Some(r);
    }
    if payload.filename.trim().is_empty() {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Filename cannot be empty"})),
            )
                .into_response(),
        );
    }

    None
}

pub async fn apply_pipeline_file_ingest_payload(
    state: &Arc<AppState>,
    pipeline: &crate::types::Pipeline,
    previous_stream_key: Option<&str>,
    payload: Option<Option<PipelineFileIngestPayload>>,
) -> Result<crate::application::ingest::PipelineFileIngestState, Response> {
    let ingest_store = SqliteIngestLookup::new(state.db.clone());
    let pipeline_store = SqlitePipelineStore::new(state.db.clone());

    if let Some(previous_stream_key) =
        previous_stream_key.filter(|previous| *previous != pipeline.stream_key.as_str())
        && clear_stream_key_file_ingests(
            &pipeline_store,
            &ingest_store,
            &state.engine,
            previous_stream_key,
        )
        .await
        .is_err()
    {
        return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    if let Some(payload) = payload {
        if clear_stream_key_file_ingests(
            &pipeline_store,
            &ingest_store,
            &state.engine,
            &pipeline.stream_key,
        )
        .await
        .is_err()
        {
            return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }

        match payload {
            Some(payload) => {
                let saved = persist_pipeline_file_ingest(
                    &ingest_store,
                    &ingest_store,
                    &pipeline_store,
                    pipeline,
                    &FileIngestConfig {
                        filename: payload.filename,
                        loop_flag: payload.loop_flag.unwrap_or(false),
                        start_time: payload.start_time.unwrap_or_default(),
                        live_optimized: payload.live_optimized.unwrap_or(false),
                        target_gop_seconds: sanitize_target_gop_seconds(payload.target_gop_seconds),
                    },
                    || format!("ingest_{}", to_hex(&rand::random::<[u8; 8]>())),
                )
                .await;
                if matches!(
                    saved,
                    Err(PersistFileIngestError::IngestLookup(_))
                        | Err(PersistFileIngestError::IngestWrite(_))
                        | Err(PersistFileIngestError::PipelineStore(_))
                ) {
                    return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response());
                }
            }
            None => {
                if remove_pipeline_file_ingest(
                    &ingest_store,
                    &ingest_store,
                    &pipeline_store,
                    pipeline,
                )
                .await
                .is_err()
                {
                    return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response());
                }
            }
        }
    }

    load_pipeline_file_ingest_state(&ingest_store, &state.engine, pipeline)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub async fn pipeline_file_ingest_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let pipeline = state.pipeline_service.get_by_id(&pipeline_id).await?;

    let ingest_store = SqliteIngestLookup::new(state.db.clone());
    let file_ingest_state =
        load_pipeline_file_ingest_state(&ingest_store, &state.engine, &pipeline)
            .await
            .map_err(|_| ApiError::internal("load pipeline file ingest state"))?;

    Ok(Json(api_view_models::file_ingest_response(
        file_ingest_state.ingest,
        file_ingest_state.running,
    ))
    .into_response())
}

pub async fn pipeline_file_ingest_put_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
    Json(payload): Json<PipelineFileIngestPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    if let Some(r) = check_field_len("filename", &payload.filename, MAX_NAME_LEN) {
        return Ok(r);
    }
    if let Some(ref start_time) = payload.start_time
        && let Some(r) = check_field_len("start_time", start_time, 64)
    {
        return Ok(r);
    }
    if payload.filename.trim().is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Filename cannot be empty"})),
        )
            .into_response());
    }

    let pipeline = state.pipeline_service.get_by_id(&pipeline_id).await?;

    let ingest_store = SqliteIngestLookup::new(state.db.clone());
    let pipeline_store = SqlitePipelineStore::new(state.db.clone());
    if clear_stream_key_file_ingests(
        &pipeline_store,
        &ingest_store,
        &state.engine,
        &pipeline.stream_key,
    )
    .await
    .is_err()
    {
        return Err(ApiError::internal("clear stream key file ingests"));
    }

    let saved = persist_pipeline_file_ingest(
        &ingest_store,
        &ingest_store,
        &pipeline_store,
        &pipeline,
        &FileIngestConfig {
            filename: payload.filename.clone(),
            loop_flag: payload.loop_flag.unwrap_or(false),
            start_time: payload.start_time.unwrap_or_default(),
            live_optimized: payload.live_optimized.unwrap_or(false),
            target_gop_seconds: sanitize_target_gop_seconds(payload.target_gop_seconds),
        },
        || format!("ingest_{}", to_hex(&rand::random::<[u8; 8]>())),
    )
    .await;

    let saved = match saved {
        Ok(saved) => saved,
        Err(PersistFileIngestError::IngestLookup(_))
        | Err(PersistFileIngestError::IngestWrite(_))
        | Err(PersistFileIngestError::PipelineStore(_)) => {
            return Err(ApiError::internal("persist pipeline file ingest"));
        }
    };

    Ok(Json(api_view_models::file_ingest_response(Some(saved), false)).into_response())
}

pub async fn pipeline_file_ingest_delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let pipeline = state.pipeline_service.get_by_id(&pipeline_id).await?;

    let ingest_store = SqliteIngestLookup::new(state.db.clone());
    let pipeline_store = SqlitePipelineStore::new(state.db.clone());
    if clear_stream_key_file_ingests(
        &pipeline_store,
        &ingest_store,
        &state.engine,
        &pipeline.stream_key,
    )
    .await
    .is_err()
    {
        return Err(ApiError::internal("clear stream key file ingests"));
    }

    if crate::application::ingest::remove_pipeline_file_ingest(
        &ingest_store,
        &ingest_store,
        &pipeline_store,
        &pipeline,
    )
    .await
    .is_err()
    {
        return Err(ApiError::internal("remove pipeline file ingest"));
    }

    Ok(Json(serde_json::json!({"deleted": true})).into_response())
}

pub async fn custom_encoding_get(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let args = state
        .settings_service
        .get_meta("custom_encoding")
        .await
        .unwrap_or(None)
        .unwrap_or_default();
    Ok(Json(serde_json::json!({ "ffmpegArgs": args })).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEncodingPayload {
    pub ffmpeg_args: String,
}

pub async fn custom_encoding_put(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<CustomEncodingPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    if let Some(r) = check_field_len("ffmpeg_args", &payload.ffmpeg_args, MAX_FFMPEG_ARGS_LEN) {
        return Ok(r);
    }
    let _ = state
        .settings_service
        .set_meta("custom_encoding", &payload.ffmpeg_args)
        .await;
    Ok(Json(serde_json::json!({ "ffmpegArgs": payload.ffmpeg_args })).into_response())
}
