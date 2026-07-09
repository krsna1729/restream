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

    pub async fn ensure_password_hash(&self, hash: &str) -> ApiResult<()> {
        if self.get_password_hash().await?.is_none() {
            self.set_password_hash(hash).await?;
        }
        Ok(())
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

    pub async fn prune_expired_sessions(&self, max_age_ms: i64) -> ApiResult<()> {
        db::prune_expired_sessions(&self.db, max_age_ms)
            .await
            .map_err(|e| ApiError::internal(format!("prune sessions: {e}")))
    }

    pub async fn list_sessions(&self) -> ApiResult<Vec<String>> {
        db::list_sessions(&self.db)
            .await
            .map_err(|e| ApiError::internal(format!("list sessions: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ensure_password_hash_preserves_existing_hash() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        let service = AuthService::new(pool);

        service.ensure_password_hash("first").await.unwrap();
        service.ensure_password_hash("second").await.unwrap();

        assert_eq!(
            service.get_password_hash().await.unwrap().as_deref(),
            Some("first")
        );
    }

    #[tokio::test]
    async fn list_sessions_returns_created_sessions() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        let service = AuthService::new(pool);

        service.create_session("token-a", 100).await.unwrap();
        service.create_session("token-b", 200).await.unwrap();

        let mut sessions = service.list_sessions().await.unwrap();
        sessions.sort();
        assert_eq!(sessions, vec!["token-a".to_string(), "token-b".to_string()]);
    }
}
