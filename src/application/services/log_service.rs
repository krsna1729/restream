use sqlx::SqlitePool;

use crate::db;
use crate::logging::types::{AppLogFilters, AppLogRow};

use super::error::{ApiError, ApiResult};

#[derive(Clone)]
pub struct LogService {
    db: SqlitePool,
}

impl LogService {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    pub async fn list_logs(&self, filters: &AppLogFilters) -> ApiResult<Vec<AppLogRow>> {
        db::list_app_logs(&self.db, filters)
            .await
            .map_err(|e| ApiError::internal(format!("list logs: {e}")))
    }

    pub async fn list_stream_backfill(
        &self,
        filters: &AppLogFilters,
        include_restream: bool,
    ) -> Vec<AppLogRow> {
        let limit = filters.limit.unwrap_or(200).clamp(1, 1000);
        if !include_restream || filters.pipeline_id.is_none() || filters.output_id.is_some() {
            return self.list_logs(filters).await.unwrap_or_default();
        }

        let mut restream_filters = filters.clone();
        restream_filters.scope = Some("restream".to_string());
        restream_filters.pipeline_id = None;
        restream_filters.output_id = None;

        let (pipeline_logs, restream_logs) =
            tokio::join!(self.list_logs(filters), self.list_logs(&restream_filters),);

        let mut merged = std::collections::BTreeMap::new();
        for row in pipeline_logs
            .unwrap_or_default()
            .into_iter()
            .chain(restream_logs.unwrap_or_default())
        {
            merged.insert(row.id, row);
        }

        merged.into_values().take(limit as usize).collect()
    }
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

    #[tokio::test]
    async fn stream_backfill_can_merge_pipeline_and_restream_logs() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        crate::db::append_app_log_batch(
            &pool,
            &[
                entry("restream event", None),
                entry("pipeline event", Some("pipe-1")),
                entry("other pipeline event", Some("pipe-2")),
            ],
        )
        .await
        .unwrap();

        let service = LogService::new(pool);
        let backfill = service
            .list_stream_backfill(&filters_for_pipeline("pipe-1", 10), true)
            .await;

        let messages = backfill
            .iter()
            .map(|row| row.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["restream event", "pipeline event"]);
    }

    #[tokio::test]
    async fn stream_backfill_without_restream_scope_keeps_pipeline_only() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        crate::db::append_app_log_batch(
            &pool,
            &[
                entry("restream event", None),
                entry("pipeline event", Some("pipe-1")),
            ],
        )
        .await
        .unwrap();

        let service = LogService::new(pool);
        let backfill = service
            .list_stream_backfill(&filters_for_pipeline("pipe-1", 10), false)
            .await;

        let messages = backfill
            .iter()
            .map(|row| row.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["pipeline event"]);
    }
}
