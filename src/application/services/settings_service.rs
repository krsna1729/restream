use sqlx::SqlitePool;

use crate::application::settings::load_settings_snapshot;
use crate::db;
use crate::media::security::IngestSecurityService;
use crate::types::{Job, Output, Pipeline};

use super::error::{ApiError, ApiResult};
use super::output_service::OutputService;
use super::pipeline_service::PipelineService;

pub struct SettingsService {
    db: SqlitePool,
    pipeline_service: PipelineService,
    output_service: OutputService,
}

impl SettingsService {
    pub fn new(db: SqlitePool) -> Self {
        Self {
            pipeline_service: PipelineService::new(db.clone()),
            output_service: OutputService::new(db.clone()),
            db,
        }
    }

    pub async fn load_snapshot(
        &self,
        security: &IngestSecurityService,
    ) -> ApiResult<crate::application::settings::SettingsSnapshot> {
        load_settings_snapshot(&self.db, security)
            .await
            .map_err(|e| ApiError::internal(format!("load settings: {e}")))
    }

    pub async fn list_pipelines(&self) -> ApiResult<Vec<Pipeline>> {
        self.pipeline_service.list_pipelines().await
    }

    pub async fn list_outputs(&self) -> ApiResult<Vec<Output>> {
        self.output_service.list_outputs().await
    }

    pub async fn list_jobs(&self) -> ApiResult<Vec<Job>> {
        db::list_jobs(&self.db)
            .await
            .map_err(|e| ApiError::internal(format!("list jobs: {e}")))
    }

    pub async fn get_ingest_host_raw(&self) -> ApiResult<String> {
        db::get_ingest_host(&self.db)
            .await
            .map(|h| h.unwrap_or_default())
            .map_err(|e| ApiError::internal(format!("get ingest host: {e}")))
    }

    pub async fn set_server_name(&self, name: &str) -> ApiResult<()> {
        db::set_meta(&self.db, "server_name", name)
            .await
            .map(|_| ())
            .map_err(|e| ApiError::internal(format!("set server name: {e}")))
    }

    pub async fn set_ingest_host(&self, host: &str) -> ApiResult<()> {
        db::set_ingest_host(&self.db, host)
            .await
            .map(|_| ())
            .map_err(|e| ApiError::internal(format!("set ingest host: {e}")))
    }

    pub async fn get_meta(&self, key: &str) -> ApiResult<Option<String>> {
        db::get_meta(&self.db, key)
            .await
            .map_err(|e| ApiError::internal(format!("get meta: {e}")))
    }

    pub async fn set_meta(&self, key: &str, value: &str) -> ApiResult<()> {
        db::set_meta(&self.db, key, value)
            .await
            .map(|_| ())
            .map_err(|e| ApiError::internal(format!("set meta: {e}")))
    }
}
