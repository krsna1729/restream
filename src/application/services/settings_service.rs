use sqlx::SqlitePool;

use crate::db;

use super::error::{ApiError, ApiResult};

pub struct SettingsService {
    db: SqlitePool,
}

impl SettingsService {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    pub async fn get_ingest_host(&self) -> String {
        db::get_ingest_host(&self.db)
            .await
            .ok()
            .flatten()
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| "localhost".to_string())
    }

    pub async fn get_server_name(&self) -> String {
        db::get_meta(&self.db, "server_name")
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
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
}
