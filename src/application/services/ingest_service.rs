use sqlx::SqlitePool;

use crate::db;
use crate::types::Ingest;

use super::error::{ApiError, ApiResult};

pub struct IngestService {
    db: SqlitePool,
}

impl IngestService {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    pub async fn list_ingests(&self) -> ApiResult<Vec<Ingest>> {
        db::list_ingests(&self.db)
            .await
            .map_err(|e| ApiError::internal(format!("list ingests: {e}")))
    }

    pub async fn get_by_id(&self, id: &str) -> ApiResult<Ingest> {
        db::get_ingest(&self.db, id)
            .await
            .map_err(|e| ApiError::internal(format!("get ingest: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("ingest {id} not found")))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_ingest(
        &self,
        id: &str,
        filename: &str,
        stream_key: &str,
        loop_flag: bool,
        start_time: &str,
        live_optimized: bool,
        target_gop_seconds: u32,
    ) -> ApiResult<Ingest> {
        db::create_ingest(
            &self.db,
            id,
            filename,
            stream_key,
            loop_flag,
            start_time,
            live_optimized,
            target_gop_seconds,
        )
        .await
        .map_err(|e| ApiError::internal(format!("create ingest: {e}")))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_ingest(
        &self,
        id: &str,
        filename: &str,
        stream_key: &str,
        loop_flag: bool,
        start_time: &str,
        live_optimized: bool,
        target_gop_seconds: u32,
    ) -> ApiResult<Ingest> {
        db::update_ingest(
            &self.db,
            id,
            filename,
            stream_key,
            loop_flag,
            start_time,
            live_optimized,
            target_gop_seconds,
        )
        .await
        .map_err(|e| ApiError::internal(format!("update ingest: {e}")))?
        .ok_or_else(|| ApiError::not_found(format!("ingest {id} not found")))
    }

    pub async fn list_for_filename(&self, filename: &str) -> ApiResult<Vec<Ingest>> {
        db::list_ingests_for_filename(&self.db, filename)
            .await
            .map_err(|e| ApiError::internal(format!("list ingests for filename: {e}")))
    }

    pub async fn delete_ingest(&self, id: &str) -> ApiResult<bool> {
        db::delete_ingest(&self.db, id)
            .await
            .map_err(|e| ApiError::internal(format!("delete ingest: {e}")))
    }
}
