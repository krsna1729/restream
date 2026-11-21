//! Application service wrapper for dashboard auth persistence.
//!
//! This module keeps the auth-specific meta keys and session operations on
//! the application boundary so HTTP handlers do not each need to remember how
//! dashboard password/session records are named or grouped.

use crate::db::DeadlineDb;

use super::error::{ServiceError, ServiceResult};

const DASHBOARD_PASSWORD_HASH_META_KEY: &str = "dashboardPasswordHash";

fn store_error<E: std::fmt::Display>(context: &str) -> impl Fn(E) -> ServiceError + '_ {
    move |err| ServiceError::internal(format!("{context}: {err}"))
}

/// Application service that owns dashboard password-hash persistence and
/// session mutations so HTTP handlers do not need to coordinate those
/// records directly.
pub struct AuthService {
    db: DeadlineDb,
}

impl AuthService {
    pub fn new(db: DeadlineDb) -> Self {
        Self { db }
    }

    /// Reads the persisted dashboard password hash used for interactive login
    /// verification.
    pub async fn get_password_hash(&self) -> ServiceResult<Option<String>> {
        self.db
            .read(|pool| crate::db::get_meta(pool, DASHBOARD_PASSWORD_HASH_META_KEY))
            .await
            .map_err(store_error("get password hash"))
    }

    /// Seeds the initial dashboard password hash only when bootstrap has not
    /// already stored one.
    pub async fn ensure_password_hash(&self, hash: &str) -> ServiceResult<()> {
        // Bootstrap flows should only seed an initial password; later changes
        // must go through the explicit password-update path.
        if self.get_password_hash().await?.is_none() {
            self.set_password_hash(hash).await?;
        }
        Ok(())
    }

    /// Persists the dashboard password hash using the shared auth-meta write
    /// path used by other auth settings.
    pub async fn set_password_hash(&self, hash: &str) -> ServiceResult<()> {
        self.set_meta(DASHBOARD_PASSWORD_HASH_META_KEY, hash).await
    }

    /// Persists one auth-specific meta entry.
    pub async fn set_meta(&self, key: &str, value: &str) -> ServiceResult<()> {
        let (key, value) = (key.to_owned(), value.to_owned());
        self.db
            .write(move |pool| async move { crate::db::set_meta(&pool, &key, &value).await })
            .await
            .map(|_| ())
            .map_err(store_error("set auth meta"))
    }

    /// Creates a new dashboard session token with its creation timestamp.
    pub async fn create_session(&self, token: &str, ts: i64) -> ServiceResult<()> {
        let token = token.to_owned();
        self.db
            .write(move |pool| async move { crate::db::create_session(&pool, &token, ts).await })
            .await
            .map_err(store_error("create session"))
    }

    /// Deletes one dashboard session token.
    pub async fn delete_session(&self, token: &str) -> ServiceResult<()> {
        let token = token.to_owned();
        self.db
            .write(move |pool| async move { crate::db::delete_session(&pool, &token).await })
            .await
            .map_err(store_error("delete session"))
    }

    /// Deletes all dashboard sessions except the caller-selected token, which
    /// is used after password changes and similar security events.
    pub async fn delete_sessions_except(&self, token: &str) -> ServiceResult<()> {
        let token = token.to_owned();
        self.db
            .write(
                move |pool| async move { crate::db::delete_sessions_except(&pool, &token).await },
            )
            .await
            .map_err(store_error("delete other sessions"))
    }

    /// Reads the creation timestamp for one session token so callers can apply
    /// session-age and timeout policy.
    pub async fn get_session_created_at(&self, token: &str) -> ServiceResult<Option<i64>> {
        self.db
            .read(|pool| crate::db::get_session_created_at(pool, token))
            .await
            .map_err(store_error("get session created_at"))
    }

    /// Prunes expired sessions according to the configured maximum age.
    pub async fn prune_expired_sessions(&self, max_age_ms: i64) -> ServiceResult<()> {
        self.db
            .write(move |pool| async move {
                crate::db::prune_expired_sessions(&pool, max_age_ms).await
            })
            .await
            .map_err(store_error("prune sessions"))
    }

    /// Lists active dashboard session tokens for audit and housekeeping flows.
    pub async fn list_sessions(&self) -> ServiceResult<Vec<String>> {
        self.db
            .read(crate::db::list_sessions)
            .await
            .map_err(store_error("list sessions"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn service() -> AuthService {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        AuthService::new(DeadlineDb::api(pool))
    }

    #[tokio::test]
    async fn sessions_are_created_listed_and_deleted() {
        let service = service().await;

        service.create_session("token-a", 100).await.unwrap();
        service.create_session("token-b", 200).await.unwrap();
        service.delete_session("token-a").await.unwrap();

        assert_eq!(service.list_sessions().await.unwrap(), vec!["token-b"]);
        assert_eq!(
            service.get_session_created_at("token-b").await.unwrap(),
            Some(200)
        );
    }

    #[tokio::test]
    async fn ensure_password_hash_preserves_existing_hash() {
        let service = service().await;

        service.ensure_password_hash("first").await.unwrap();
        service.ensure_password_hash("second").await.unwrap();

        assert_eq!(
            service.get_password_hash().await.unwrap().as_deref(),
            Some("first")
        );
    }

    #[tokio::test]
    async fn list_sessions_returns_created_sessions() {
        let service = service().await;

        service.create_session("token-a", 100).await.unwrap();
        service.create_session("token-b", 200).await.unwrap();

        let mut sessions = service.list_sessions().await.unwrap();
        sessions.sort();
        assert_eq!(sessions, vec!["token-a".to_string(), "token-b".to_string()]);
    }
}
