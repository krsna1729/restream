use std::sync::Arc;

use crate::application::ports::{LogStore, SqliteLogStore};
use crate::logging::types::{AppLogFilters, AppLogRow};

use super::error::{ApiError, ApiResult};

#[derive(Clone)]
pub struct LogService {
    store: Arc<dyn LogStore>,
}

impl LogService {
    pub fn new(db: sqlx::SqlitePool) -> Self {
        Self {
            store: Arc::new(SqliteLogStore::new(db)),
        }
    }

    pub fn with_store(store: Arc<dyn LogStore>) -> Self {
        Self { store }
    }

    pub async fn list_logs(&self, filters: &AppLogFilters) -> ApiResult<Vec<AppLogRow>> {
        self.store
            .list_app_logs(filters)
            .await
            .map_err(|e| ApiError::internal(format!("list logs: {e}")))
    }

    pub async fn list_stream_backfill(
        &self,
        filters: &AppLogFilters,
        include_restream: bool,
    ) -> ApiResult<Vec<AppLogRow>> {
        let limit = filters.limit.unwrap_or(200).clamp(1, 1000);
        if !include_restream || filters.pipeline_id.is_none() || filters.output_id.is_some() {
            return self.list_logs(filters).await;
        }

        let mut restream_filters = filters.clone();
        restream_filters.scope = Some("restream".to_string());
        restream_filters.pipeline_id = None;
        restream_filters.output_id = None;

        let (pipeline_logs, restream_logs) =
            tokio::join!(self.list_logs(filters), self.list_logs(&restream_filters),);

        let mut merged = std::collections::BTreeMap::new();
        for row in pipeline_logs?.into_iter().chain(restream_logs?) {
            merged.insert(row.id, row);
        }

        Ok(merged.into_values().take(limit as usize).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::{LogListFuture, LogStore, LogStoreError};
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

    #[derive(Clone)]
    struct FakeLogStore {
        rows: Arc<Vec<AppLogRow>>,
    }

    impl FakeLogStore {
        fn row(id: i64, message: &str, pipeline_id: Option<&str>) -> AppLogRow {
            AppLogRow {
                id,
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
    }

    impl LogStore for FakeLogStore {
        fn list_app_logs<'a>(&'a self, filters: &'a AppLogFilters) -> LogListFuture<'a> {
            Box::pin(async move {
                if filters.target.as_deref() == Some("fail") {
                    return Err(LogStoreError::new("log store failed"));
                }
                let mut rows = self
                    .rows
                    .iter()
                    .filter(|row| match filters.scope.as_deref() {
                        Some("restream") => row.pipeline_id.is_none() && row.output_id.is_none(),
                        Some("pipeline") => row.pipeline_id.is_some() && row.output_id.is_none(),
                        Some("output") => row.output_id.is_some(),
                        _ => true,
                    })
                    .filter(|row| {
                        filters
                            .pipeline_id
                            .as_ref()
                            .is_none_or(|pipeline_id| row.pipeline_id.as_ref() == Some(pipeline_id))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                rows.sort_by_key(|row| row.id);
                Ok(rows)
            })
        }
    }

    #[tokio::test]
    async fn stream_backfill_merge_uses_injected_log_store() {
        let service = LogService::with_store(Arc::new(FakeLogStore {
            rows: Arc::new(vec![
                FakeLogStore::row(1, "restream event", None),
                FakeLogStore::row(2, "pipeline event", Some("pipe-1")),
                FakeLogStore::row(3, "other pipeline event", Some("pipe-2")),
            ]),
        }));

        let backfill = service
            .list_stream_backfill(&filters_for_pipeline("pipe-1", 10), true)
            .await
            .unwrap();

        let messages = backfill
            .iter()
            .map(|row| row.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["restream event", "pipeline event"]);
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
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
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
        crate::db::append_app_log_batch(&pool, &entries)
            .await
            .unwrap();

        let service = LogService::new(pool);
        let mut filters = filters_for_pipeline("pipe-1", 200);
        let mut cursor = 0;
        let mut received_ids = Vec::new();
        loop {
            filters.after_id = Some(cursor);
            let page = service.list_stream_backfill(&filters, false).await.unwrap();
            if page.is_empty() {
                break;
            }
            cursor = page.iter().map(|row| row.id).max().unwrap();
            received_ids.extend(page.into_iter().map(|row| row.id));
        }

        assert_eq!(received_ids, (1..=250).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn stream_backfill_distinguishes_store_failure_from_empty_page() {
        let service = LogService::with_store(Arc::new(FakeLogStore {
            rows: Arc::new(Vec::new()),
        }));
        let empty = service
            .list_stream_backfill(&filters_for_pipeline("pipe-1", 10), false)
            .await
            .unwrap();
        assert!(empty.is_empty());

        let mut failing_filters = filters_for_pipeline("pipe-1", 10);
        failing_filters.target = Some("fail".to_string());
        let error = service
            .list_stream_backfill(&failing_filters, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("log store failed"));
    }
}
