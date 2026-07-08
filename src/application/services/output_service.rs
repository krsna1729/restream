use sqlx::SqlitePool;

use crate::db;
use crate::domain::output_spec::OutputConfig;
use crate::types::Output;

use super::error::{ApiError, ApiResult};

/// Application service for output CRUD and lifecycle requests.
///
/// API handlers call this instead of `db::*` directly. In Phase 5
/// this will depend on a repository trait instead of `SqlitePool`.
pub struct OutputService {
    db: SqlitePool,
}

impl OutputService {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    pub async fn list_outputs(&self) -> ApiResult<Vec<Output>> {
        db::list_outputs(&self.db)
            .await
            .map_err(|e| ApiError::internal(format!("list outputs: {e}")))
    }

    pub async fn list_for_pipeline(&self, pipeline_id: &str) -> ApiResult<Vec<Output>> {
        db::list_outputs_for_pipeline(&self.db, pipeline_id)
            .await
            .map_err(|e| ApiError::internal(format!("list outputs for pipeline: {e}")))
    }

    pub async fn get_by_id(&self, pipeline_id: &str, id: &str) -> ApiResult<Output> {
        db::get_output(&self.db, pipeline_id, id)
            .await
            .map_err(|e| ApiError::internal(format!("get output: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("output {id} not found")))
    }

    pub async fn create_output(
        &self,
        id: &str,
        pipeline_id: &str,
        name: &str,
        url: &str,
        monitoring_url: Option<&str>,
        desired_state: &str,
        config: &OutputConfig,
    ) -> ApiResult<Output> {
        db::create_output(
            &self.db,
            id,
            pipeline_id,
            name,
            url,
            monitoring_url,
            desired_state,
            config,
        )
        .await
        .map_err(|e| ApiError::internal(format!("create output: {e}")))
    }

    pub async fn update_output(
        &self,
        pipeline_id: &str,
        id: &str,
        name: &str,
        url: &str,
        monitoring_url: Option<&str>,
        config: &OutputConfig,
    ) -> ApiResult<Output> {
        db::update_output(&self.db, pipeline_id, id, name, url, monitoring_url, config)
            .await
            .map_err(|e| ApiError::internal(format!("update output: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("output {id} not found")))
    }

    pub async fn delete_output(&self, pipeline_id: &str, id: &str) -> ApiResult<bool> {
        db::delete_output(&self.db, pipeline_id, id)
            .await
            .map_err(|e| ApiError::internal(format!("delete output: {e}")))
    }

    pub async fn request_start(&self, pipeline_id: &str, id: &str) -> ApiResult<Output> {
        db::set_output_desired_state(&self.db, pipeline_id, id, "running")
            .await
            .map_err(|e| ApiError::internal(format!("request start: {e}")))
    }

    pub async fn request_stop(&self, pipeline_id: &str, id: &str) -> ApiResult<Output> {
        db::set_output_desired_state(&self.db, pipeline_id, id, "stopped")
            .await
            .map_err(|e| ApiError::internal(format!("request stop: {e}")))
    }
}
