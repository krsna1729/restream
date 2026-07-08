use sqlx::SqlitePool;

use crate::db;
use crate::logging::types::{AppLogFilters, AppLogRow};

use super::error::{ApiError, ApiResult};

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
}
