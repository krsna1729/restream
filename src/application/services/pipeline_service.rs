use std::sync::Arc;

use crate::application::ports::{PipelineStore, SqlitePipelineStore};
use crate::types::Pipeline;

use super::error::{ApiError, ApiResult};

/// Application service for pipeline CRUD and read operations.
///
/// Depends on `PipelineStore` — a port trait — rather than `SqlitePool`
/// directly. The default constructor wires it through `SqlitePipelineStore`;
/// tests can inject any implementation.
#[derive(Clone)]
pub struct PipelineService {
    store: Arc<dyn PipelineStore>,
}

impl PipelineService {
    /// Create a service backed by SQLite via `SqlitePipelineStore`.
    pub fn new(db: sqlx::SqlitePool) -> Self {
        Self {
            store: Arc::new(SqlitePipelineStore::new(db)),
        }
    }

    /// Create a service backed by any `PipelineStore` implementation.
    /// Useful for tests with an in-memory or mock store.
    pub fn with_store(store: Arc<dyn PipelineStore>) -> Self {
        Self { store }
    }

    pub async fn list_pipelines(&self) -> ApiResult<Vec<Pipeline>> {
        self.store
            .list_pipelines()
            .await
            .map_err(|e| ApiError::internal(format!("list pipelines: {e}")))
    }

    pub async fn get_by_id(&self, id: &str) -> ApiResult<Pipeline> {
        self.store
            .get_pipeline(id)
            .await
            .map_err(|e| ApiError::internal(format!("get pipeline: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("pipeline {id} not found")))
    }

    pub async fn get_by_stream_key(&self, stream_key: &str) -> ApiResult<Option<Pipeline>> {
        self.store
            .get_pipeline_by_stream_key(stream_key)
            .await
            .map_err(|e| ApiError::internal(format!("get pipeline by stream key: {e}")))
    }

    pub async fn create_pipeline(
        &self,
        id: &str,
        name: &str,
        stream_key: &str,
        input_source: Option<&str>,
        srt_ingest_policy: Option<&str>,
    ) -> ApiResult<Pipeline> {
        self.store
            .create_pipeline(id, name, stream_key, input_source, srt_ingest_policy)
            .await
            .map_err(|e| ApiError::internal(format!("create pipeline: {e}")))
    }

    pub async fn update_pipeline(
        &self,
        id: &str,
        name: &str,
        stream_key: &str,
        input_source: Option<&str>,
        srt_ingest_policy: Option<&str>,
    ) -> ApiResult<Pipeline> {
        self.store
            .update_pipeline(id, name, stream_key, input_source, srt_ingest_policy)
            .await
            .map_err(|e| ApiError::internal(format!("update pipeline: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("pipeline {id} not found")))
    }

    pub async fn delete_pipeline(&self, id: &str) -> ApiResult<bool> {
        self.store
            .delete_pipeline(id)
            .await
            .map_err(|e| ApiError::internal(format!("delete pipeline: {e}")))
    }

    pub async fn set_input_source(
        &self,
        id: &str,
        input_source: Option<&str>,
    ) -> ApiResult<Pipeline> {
        let pipeline = self.get_by_id(id).await?;
        self.store
            .update_pipeline(
                id,
                &pipeline.name,
                &pipeline.stream_key,
                input_source,
                pipeline.srt_ingest_policy.as_deref(),
            )
            .await
            .map_err(|e| ApiError::internal(format!("set input source: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("pipeline {id} not found")))
    }

    /// List all pipeline IDs (used by health and settings).
    pub async fn list_pipeline_ids(&self) -> ApiResult<Vec<String>> {
        let pipelines = self.list_pipelines().await?;
        Ok(pipelines.into_iter().map(|p| p.id).collect())
    }

    /// Return the configured ingest host, or the default.
    pub async fn get_ingest_host(&self) -> String {
        self.store
            .get_ingest_host()
            .await
            .ok()
            .flatten()
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| "localhost".to_string())
    }
}
