//! Application-owned log query helpers over concrete SQLite persistence.
//!
//! Handlers map HTTP filters here; merge policy for stream backfill stays on
//! this boundary. Persistence is `db::list_app_logs` — no LogStore port.

use sqlx::SqlitePool;

use crate::application::services::error::{ServiceError, ServiceResult};
use crate::logging::types::{AppLogFilters, AppLogRow};

/// Lists persisted application logs using the caller-supplied filters
/// without applying any stream-specific merge behavior.
pub async fn list_logs(
    pool: &SqlitePool,
    filters: &AppLogFilters,
) -> ServiceResult<Vec<AppLogRow>> {
    crate::db::list_app_logs(pool, filters)
        .await
        .map_err(|e| ServiceError::internal(format!("list logs: {e}")))
}

/// Builds the dashboard stream backfill view, optionally merging pipeline
/// logs with global restream-scope rows when the request shape allows it.
pub async fn list_stream_backfill(
    pool: &SqlitePool,
    filters: &AppLogFilters,
    include_restream: bool,
) -> ServiceResult<Vec<AppLogRow>> {
    let limit = filters.limit.unwrap_or(200).clamp(1, 1000);
    if !should_merge_restream_backfill(filters, include_restream) {
        return list_logs(pool, filters).await;
    }

    let mut restream_filters = filters.clone();
    restream_filters.scope = Some("restream".to_string());
    restream_filters.pipeline_id = None;
    restream_filters.output_id = None;

    let (pipeline_logs, restream_logs) =
        tokio::join!(list_logs(pool, filters), list_logs(pool, &restream_filters),);

    let mut merged = std::collections::BTreeMap::new();
    for row in pipeline_logs?.into_iter().chain(restream_logs?) {
        merged.insert(row.id, row);
    }

    Ok(merged.into_values().take(limit as usize).collect())
}

/// Only merge global restream rows into a backfill request when the caller is
/// looking at one pipeline-wide stream rather than a specific output stream.
fn should_merge_restream_backfill(filters: &AppLogFilters, include_restream: bool) -> bool {
    include_restream && filters.pipeline_id.is_some() && filters.output_id.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::types::AppLogEntry;

    fn entry(message: &str, pipeline_id: Option<&str>) -> AppLogEntry {
        AppLogEntry {
            ts: "2026-07-09T00:00:00Z".to_string(),
            level: "INFO".to_string(),
            target: "restream::tests".to_string(),
            message: message.to_string(),
            fields: None,
            pipeline_id: pipeline_id.map(str::to_string),
            output_id: None,
            event_type: Some("test.event".to_string()),
            event_class: Some("lifecycle".to_string()),
        }
    }

    fn filters_for_pipeline(pipeline_id: &str, limit: i64) -> AppLogFilters {
        AppLogFilters {
            after_id: Some(0),
            level: Some("info".to_string()),
            since: None,
            until: None,
            target: None,
            scope: None,
            pipeline_id: Some(pipeline_id.to_string()),
            output_id: None,
            event_class: None,
            prefix: None,
            limit: Some(limit),
            order: Some("asc".to_string()),
        }
    }

    async fn seed_pool(entries: &[AppLogEntry]) -> SqlitePool {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        crate::db::append_app_log_batch(&pool, entries)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn stream_backfill_can_merge_pipeline_and_restream_logs() {
        let pool = seed_pool(&[
            entry("restream event", None),
            entry("pipeline event", Some("pipe-1")),
            entry("other pipeline event", Some("pipe-2")),
        ])
        .await;

        let backfill = list_stream_backfill(&pool, &filters_for_pipeline("pipe-1", 10), true)
            .await
            .unwrap();

        let messages = backfill
            .iter()
            .map(|row| row.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["restream event", "pipeline event"]);
    }

    #[tokio::test]
    async fn stream_backfill_without_restream_scope_keeps_pipeline_only() {
        let pool = seed_pool(&[
            entry("restream event", None),
            entry("pipeline event", Some("pipe-1")),
        ])
        .await;

        let backfill = list_stream_backfill(&pool, &filters_for_pipeline("pipe-1", 10), false)
            .await
            .unwrap();

        let messages = backfill
            .iter()
            .map(|row| row.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["pipeline event"]);
    }

    #[tokio::test]
    async fn paged_stream_backfill_does_not_skip_ids_when_timestamps_run_backwards() {
        let entries = (1..=250)
            .map(|sequence| {
                let reverse_seconds = 250 - sequence;
                AppLogEntry {
                    ts: format!(
                        "2026-07-09T00:{:02}:{:02}Z",
                        reverse_seconds / 60,
                        reverse_seconds % 60
                    ),
                    level: "INFO".to_string(),
                    target: "restream::tests".to_string(),
                    message: format!("event {sequence}"),
                    fields: None,
                    pipeline_id: Some("pipe-1".to_string()),
                    output_id: None,
                    event_type: Some("test.event".to_string()),
                    event_class: Some("lifecycle".to_string()),
                }
            })
            .collect::<Vec<_>>();
        let pool = seed_pool(&entries).await;

        let mut filters = filters_for_pipeline("pipe-1", 200);
        let mut cursor = 0;
        let mut received_ids = Vec::new();
        loop {
            filters.after_id = Some(cursor);
            let page = list_stream_backfill(&pool, &filters, false).await.unwrap();
            if page.is_empty() {
                break;
            }
            cursor = page.iter().map(|row| row.id).max().unwrap();
            received_ids.extend(page.into_iter().map(|row| row.id));
        }

        assert_eq!(received_ids, (1..=250).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn list_logs_surfaces_store_failures() {
        // Closed pool forces sqlx errors so handlers can distinguish failure
        // from an empty page.
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        pool.close().await;
        let error = list_logs(&pool, &filters_for_pipeline("pipe-1", 10))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("list logs:"));
    }
}
