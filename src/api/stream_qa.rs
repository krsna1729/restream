//! `GET`/`PUT /api/v1/pipelines/:id/stream-qa`: a pipeline's picture and
//! sound checks, whether they are on, and what they last found.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::error::ApiError;
use super::state::{AppState, require_authenticated};
use crate::application::stream_qa::stream_qa_enabled_meta_key;

#[derive(Debug, Deserialize)]
pub struct StreamQaWrite {
    pub enabled: bool,
}

fn state_response(state: &AppState, pipeline_id: &str, enabled: bool) -> Response {
    Json(serde_json::json!({
        "enabled": enabled,
        "active": state.is_stream_qa_active(pipeline_id),
        "findings": state.stream_qa_findings(pipeline_id),
    }))
    .into_response()
}

pub async fn stream_qa_get_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }
    state.pipeline(&pipeline_id).await?;
    let enabled = state
        .settings_meta(&stream_qa_enabled_meta_key(&pipeline_id))
        .await?
        .is_some_and(|value| value == "1");
    Ok(state_response(&state, &pipeline_id, enabled))
}

/// Turns the checks on or off; on, they start at once if the source is
/// live (the reconcile loop also keeps them in step with the ingest).
pub async fn stream_qa_put_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    headers: HeaderMap,
    Json(write): Json<StreamQaWrite>,
) -> Result<Response, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }
    state.pipeline(&pipeline_id).await?;
    state
        .set_settings_meta(
            &stream_qa_enabled_meta_key(&pipeline_id),
            if write.enabled { "1" } else { "0" },
        )
        .await?;
    if write.enabled {
        state.start_stream_qa(&pipeline_id).await;
    } else {
        state.unregister_stream_qa(&pipeline_id);
    }
    Ok(state_response(&state, &pipeline_id, write.enabled))
}
