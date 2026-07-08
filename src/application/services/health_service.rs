use sqlx::SqlitePool;

use crate::db;

use super::error::{ApiError, ApiResult};

pub struct HealthService {
    db: SqlitePool,
}

impl HealthService {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    pub async fn check_db(&self) -> ApiResult<bool> {
        db::list_pipelines(&self.db)
            .await
            .map(|_| true)
            .map_err(|e| ApiError::internal(format!("db health check: {e}")))
    }
}
