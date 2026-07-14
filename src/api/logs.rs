//! Log HTTP handlers expose historical and streaming log views at the API
//! boundary. This module translates query parameters into log-service filters
//! and keeps SSE framing/filtering logic close to the transport layer.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::logging::types::AppLogFilters;

use super::state::{AppState, require_authenticated};

#[derive(Deserialize)]
pub struct LogsQuery {
    pub after_id: Option<i64>,
    pub level: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub target: Option<String>,
    pub scope: Option<String>,
    pub pipeline_id: Option<String>,
    pub output_id: Option<String>,
    pub event_class: Option<String>,
    pub prefix: Option<String>,
    pub limit: Option<u32>,
    pub order: Option<String>,
}

#[derive(Deserialize)]
pub struct LogsStreamQuery {
    pub level: Option<String>,
    pub target: Option<String>,
    pub scope: Option<String>,
    pub pipeline_id: Option<String>,
    pub output_id: Option<String>,
    pub event_class: Option<String>,
    pub include_restream: Option<bool>,
    pub prefix: Option<String>,
    pub last_event_id: Option<i64>,
}

fn build_logs_filters(query: LogsQuery) -> AppLogFilters {
    AppLogFilters {
        after_id: query.after_id,
        level: query.level,
        since: query.since,
        until: query.until,
        target: query.target,
        scope: query.scope,
        pipeline_id: query.pipeline_id,
        output_id: query.output_id,
        event_class: query.event_class,
        prefix: query.prefix,
        limit: query.limit.map(|limit| limit as i64),
        order: query.order,
    }
}

pub fn log_stream_scope_matches(
    scope: Option<&str>,
    pipeline_id: Option<&str>,
    output_id: Option<&str>,
) -> bool {
    match scope {
        Some("restream") => pipeline_id.is_none() && output_id.is_none(),
        Some("pipeline") => pipeline_id.is_some() && output_id.is_none(),
        Some("output") => output_id.is_some(),
        _ => true,
    }
}

pub fn log_stream_prefix_matches(prefix: Option<&str>, message: &str) -> bool {
    let Some(prefix) = prefix else {
        return true;
    };

    prefix
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .any(|part| message.starts_with(part))
}

#[allow(clippy::too_many_arguments)]
pub fn log_broadcast_matches_stream_filters(
    entry: &crate::logging::LogBroadcast,
    target: Option<&str>,
    scope: Option<&str>,
    pipeline_id: Option<&str>,
    output_id: Option<&str>,
    event_class: Option<&str>,
    include_restream: bool,
    prefix: Option<&str>,
) -> bool {
    if let Some(target) = target
        && !entry.target.starts_with(target)
    {
        return false;
    }
    if !log_stream_scope_matches(
        scope,
        entry.pipeline_id.as_deref(),
        entry.output_id.as_deref(),
    ) {
        return false;
    }
    if let Some(pipeline_id) = pipeline_id {
        let matches_pipeline = entry.pipeline_id.as_deref() == Some(pipeline_id);
        let matches_restream = include_restream
            && output_id.is_none()
            && entry.pipeline_id.is_none()
            && entry.output_id.is_none();
        if !matches_pipeline && !matches_restream {
            return false;
        }
    }
    if let Some(output_id) = output_id
        && entry.output_id.as_deref() != Some(output_id)
    {
        return false;
    }
    if let Some(event_class) = event_class
        && entry.event_class.as_deref() != Some(event_class)
    {
        return false;
    }
    log_stream_prefix_matches(prefix, &entry.message)
}

fn log_row_sse_frame(row: &crate::logging::AppLogRow) -> String {
    let data = serde_json::to_string(row).unwrap_or_default();
    format!("id: {}\nevent: log\ndata: {}\n\n", row.id, data)
}

fn log_level_passes(min_level: &str, level: &str) -> bool {
    match min_level {
        "error" => level == "ERROR",
        "warn" => matches!(level, "ERROR" | "WARN"),
        "debug" => matches!(level, "ERROR" | "WARN" | "INFO" | "DEBUG"),
        _ => matches!(level, "ERROR" | "WARN" | "INFO"),
    }
}

pub async fn logs_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<LogsQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let filters = build_logs_filters(query);

    let logs = state
        .log_service
        .list_logs(&filters)
        .await
        .unwrap_or_default();
    let has_more = logs.len() >= filters.limit.unwrap_or(200).clamp(1, 1000) as usize;

    Json(serde_json::json!({
        "logs": logs,
        "total": logs.len(),
        "hasMore": has_more,
    }))
    .into_response()
}

pub async fn logs_stream_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<LogsStreamQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let resume_from: Option<i64> = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .or(query.last_event_id);

    let min_level = query.level.unwrap_or_else(|| "info".to_string());
    let filter_target = query.target;
    let filter_scope = query.scope;
    let filter_pipeline = query.pipeline_id;
    let filter_output = query.output_id;
    let filter_event_class = query.event_class;
    let include_restream = query.include_restream.unwrap_or(false)
        && filter_pipeline.is_some()
        && filter_output.is_none();
    let filter_prefix = query.prefix;

    let (tx, rx) = tokio::sync::mpsc::channel::<String>(64);

    let log_service = state.log_service.clone();
    let mut broadcast_rx = state.log_broadcast.subscribe();

    tokio::spawn(async move {
        let mut delivered_through = resume_from.unwrap_or(0);
        if resume_from.is_some() {
            loop {
                let Ok(backfill) = log_service
                    .list_stream_backfill(
                        &AppLogFilters {
                            after_id: Some(delivered_through),
                            level: Some(min_level.clone()),
                            since: None,
                            until: None,
                            target: filter_target.clone(),
                            scope: filter_scope.clone(),
                            pipeline_id: filter_pipeline.clone(),
                            output_id: filter_output.clone(),
                            event_class: filter_event_class.clone(),
                            prefix: filter_prefix.clone(),
                            limit: Some(200),
                            order: Some("asc".to_string()),
                        },
                        include_restream,
                    )
                    .await
                else {
                    // End the response so EventSource reconnects with its prior
                    // cursor instead of silently treating a failed page as empty.
                    return;
                };

                let page_len = backfill.len();
                for row in backfill {
                    delivered_through = delivered_through.max(row.id);
                    if tx.send(log_row_sse_frame(&row)).await.is_err() {
                        return;
                    }
                }
                if page_len < 200 {
                    break;
                }
            }
        }

        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(20));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                entry = broadcast_rx.recv() => {
                    match entry {
                        Ok(e) => {
                            if e.id <= delivered_through { continue; }
                            if !log_level_passes(&min_level, &e.level) { continue; }
                            if !log_broadcast_matches_stream_filters(
                                &e,
                                filter_target.as_deref(),
                                filter_scope.as_deref(),
                                filter_pipeline.as_deref(),
                                filter_output.as_deref(),
                                filter_event_class.as_deref(),
                                include_restream,
                                filter_prefix.as_deref(),
                            ) {
                                continue;
                            }
                            delivered_through = e.id;
                            if tx.send(log_row_sse_frame(&e)).await.is_err() { return; }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            return;
                        }
                        Err(_) => return,
                    }
                }
                _ = heartbeat.tick() => {
                    if tx.send(": ping\n\n".to_string()).await.is_err() { return; }
                }
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = axum::body::Body::from_stream(futures_util::StreamExt::map(stream, |s| {
        Ok::<_, std::convert::Infallible>(s)
    }));

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::{LogsQuery, build_logs_filters, log_level_passes, log_row_sse_frame};

    #[test]
    fn build_logs_filters_preserves_limit_and_scope_fields() {
        let filters = build_logs_filters(LogsQuery {
            after_id: Some(10),
            level: Some("warn".to_string()),
            since: None,
            until: None,
            target: None,
            scope: Some("pipeline".to_string()),
            pipeline_id: Some("pipe-1".to_string()),
            output_id: None,
            event_class: None,
            prefix: None,
            limit: Some(25),
            order: Some("asc".to_string()),
        });

        assert_eq!(filters.after_id, Some(10));
        assert_eq!(filters.scope.as_deref(), Some("pipeline"));
        assert_eq!(filters.limit, Some(25));
    }

    #[test]
    fn log_level_passes_respects_warn_threshold() {
        assert!(log_level_passes("warn", "ERROR"));
        assert!(log_level_passes("warn", "WARN"));
        assert!(!log_level_passes("warn", "INFO"));
    }

    #[test]
    fn sse_frame_preserves_persisted_id_and_lifecycle_metadata() {
        let frame = log_row_sse_frame(&crate::logging::AppLogRow {
            id: 42,
            ts: "2026-07-10T00:00:00Z".to_string(),
            level: "INFO".to_string(),
            target: "restream::test".to_string(),
            message: "connected".to_string(),
            fields: None,
            pipeline_id: Some("pipe-1".to_string()),
            output_id: None,
            event_type: Some("ingest.connected".to_string()),
            event_class: Some("lifecycle".to_string()),
        });

        assert!(frame.starts_with("id: 42\nevent: log\ndata: "));
        assert!(frame.contains("\"eventType\":\"ingest.connected\""));
        assert!(frame.contains("\"eventClass\":\"lifecycle\""));
    }
}
