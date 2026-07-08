use sqlx::SqlitePool;

use crate::db;
use crate::types::Pipeline;

use super::error::{ApiError, ApiResult};

/// Application service for pipeline CRUD and read operations.
///
/// API handlers call this instead of `db::*` directly. In Phase 5
/// this will depend on a repository trait instead of `SqlitePool`.
#[derive(Clone)]
pub struct PipelineService {
    db: SqlitePool,
}

impl PipelineService {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    pub async fn list_pipelines(&self) -> ApiResult<Vec<Pipeline>> {
        db::list_pipelines(&self.db)
            .await
            .map_err(|e| ApiError::internal(format!("list pipelines: {e}")))
    }

    pub async fn get_by_id(&self, id: &str) -> ApiResult<Pipeline> {
        db::get_pipeline(&self.db, id)
            .await
            .map_err(|e| ApiError::internal(format!("get pipeline: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("pipeline {id} not found")))
    }

    pub async fn get_by_stream_key(&self, stream_key: &str) -> ApiResult<Option<Pipeline>> {
        db::get_pipeline_by_stream_key(&self.db, stream_key)
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
        db::create_pipeline(
            &self.db,
            id,
            name,
            stream_key,
            input_source,
            srt_ingest_policy,
        )
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
        db::update_pipeline(
            &self.db,
            id,
            name,
            stream_key,
            input_source,
            srt_ingest_policy,
        )
        .await
        .map_err(|e| ApiError::internal(format!("update pipeline: {e}")))?
        .ok_or_else(|| ApiError::not_found(format!("pipeline {id} not found")))
    }

    pub async fn delete_pipeline(&self, id: &str) -> ApiResult<bool> {
        db::delete_pipeline(&self.db, id)
            .await
            .map_err(|e| ApiError::internal(format!("delete pipeline: {e}")))
    }

    pub async fn set_input_source(
        &self,
        id: &str,
        input_source: Option<&str>,
    ) -> ApiResult<Pipeline> {
        let pipeline = self.get_by_id(id).await?;
        db::update_pipeline(
            &self.db,
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
        db::get_ingest_host(&self.db)
            .await
            .ok()
            .flatten()
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| "localhost".to_string())
    }
}
