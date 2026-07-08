use sqlx::SqlitePool;

use crate::db;

use super::error::{ApiError, ApiResult};

pub struct AuthService {
    db: SqlitePool,
}

impl AuthService {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    pub async fn get_password_hash(&self) -> ApiResult<Option<String>> {
        db::get_meta(&self.db, "dashboardPasswordHash")
            .await
            .map_err(|e| ApiError::internal(format!("get password hash: {e}")))
    }

    pub async fn set_password_hash(&self, hash: &str) -> ApiResult<()> {
        db::set_meta(&self.db, "dashboardPasswordHash", hash)
            .await
            .map(|_| ())
            .map_err(|e| ApiError::internal(format!("set password hash: {e}")))
    }

    pub async fn create_session(&self, token: &str, ts: i64) -> ApiResult<()> {
        db::create_session(&self.db, token, ts)
            .await
            .map_err(|e| ApiError::internal(format!("create session: {e}")))
    }

    pub async fn delete_session(&self, token: &str) -> ApiResult<()> {
        db::delete_session(&self.db, token)
            .await
            .map_err(|e| ApiError::internal(format!("delete session: {e}")))
    }
}
