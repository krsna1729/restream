//! Application service wrapper for dashboard auth persistence.
//!
//! This module keeps the auth-specific meta keys and session-store operations on
//! the application boundary so HTTP handlers and lower-level stores do not each
//! need to remember how dashboard password/session records are named or grouped.

use std::sync::Arc;

use crate::application::ports::{MetaStore, MetaStoreWriter, SessionStore};

use super::error::{ApiError, ApiResult};

const DASHBOARD_PASSWORD_HASH_META_KEY: &str = "dashboardPasswordHash";

pub struct AuthService {
    meta_store: Arc<dyn MetaStore>,
    meta_writer: Arc<dyn MetaStoreWriter>,
    session_store: Arc<dyn SessionStore>,
}

impl AuthService {
    pub fn with_stores(
        meta_store: Arc<dyn MetaStore>,
        meta_writer: Arc<dyn MetaStoreWriter>,
        session_store: Arc<dyn SessionStore>,
    ) -> Self {
        Self {
            meta_store,
            meta_writer,
            session_store,
        }
    }

    pub async fn get_password_hash(&self) -> ApiResult<Option<String>> {
        self.meta_store
            .get_meta(DASHBOARD_PASSWORD_HASH_META_KEY)
            .await
            .map_err(|e| ApiError::internal(format!("get password hash: {e}")))
    }

    pub async fn ensure_password_hash(&self, hash: &str) -> ApiResult<()> {
        // Bootstrap flows should only seed an initial password; later changes
        // must go through the explicit password-update path.
        if self.get_password_hash().await?.is_none() {
            self.set_password_hash(hash).await?;
        }
        Ok(())
    }

    pub async fn set_password_hash(&self, hash: &str) -> ApiResult<()> {
        self.meta_writer
            .set_meta(DASHBOARD_PASSWORD_HASH_META_KEY, hash)
            .await
            .map(|_| ())
            .map_err(|e| ApiError::internal(format!("set password hash: {e}")))
    }

    pub async fn set_meta(&self, key: &str, value: &str) -> ApiResult<()> {
        self.meta_writer
            .set_meta(key, value)
            .await
            .map(|_| ())
            .map_err(|e| ApiError::internal(format!("set auth meta: {e}")))
    }

    pub async fn create_session(&self, token: &str, ts: i64) -> ApiResult<()> {
        self.session_store
            .create_session(token, ts)
            .await
            .map_err(|e| ApiError::internal(format!("create session: {e}")))
    }

    pub async fn delete_session(&self, token: &str) -> ApiResult<()> {
        self.session_store
            .delete_session(token)
            .await
            .map_err(|e| ApiError::internal(format!("delete session: {e}")))
    }

    pub async fn delete_sessions_except(&self, token: &str) -> ApiResult<()> {
        self.session_store
            .delete_sessions_except(token)
            .await
            .map_err(|e| ApiError::internal(format!("delete other sessions: {e}")))
    }

    pub async fn get_session_created_at(&self, token: &str) -> ApiResult<Option<i64>> {
        self.session_store
            .get_session_created_at(token)
            .await
            .map_err(|e| ApiError::internal(format!("get session created_at: {e}")))
    }

    pub async fn prune_expired_sessions(&self, max_age_ms: i64) -> ApiResult<()> {
        self.session_store
            .prune_expired_sessions(max_age_ms)
            .await
            .map_err(|e| ApiError::internal(format!("prune sessions: {e}")))
    }

    pub async fn list_sessions(&self) -> ApiResult<Vec<String>> {
        self.session_store
            .list_sessions()
            .await
            .map_err(|e| ApiError::internal(format!("list sessions: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use crate::application::ports::{
        MetaLookupError, MetaLookupFuture, MetaWriteFuture, SessionListFuture, SessionLookupFuture,
        SessionStoreError, SessionWriteFuture,
    };

    #[derive(Default)]
    struct FakeAuthStore {
        password_hash: Mutex<Option<String>>,
        sessions: Mutex<BTreeSet<(String, i64)>>,
    }

    impl MetaStore for FakeAuthStore {
        fn get_meta<'a>(&'a self, key: &'a str) -> MetaLookupFuture<'a> {
            Box::pin(async move {
                if key == DASHBOARD_PASSWORD_HASH_META_KEY {
                    Ok(self.password_hash.lock().unwrap().clone())
                } else {
                    Ok(None)
                }
            })
        }
    }

    impl MetaStoreWriter for FakeAuthStore {
        fn set_meta<'a>(&'a self, key: &'a str, value: &'a str) -> MetaWriteFuture<'a> {
            Box::pin(async move {
                if key == DASHBOARD_PASSWORD_HASH_META_KEY {
                    *self.password_hash.lock().unwrap() = Some(value.to_string());
                    Ok(value.to_string())
                } else {
                    Err(MetaLookupError::new(format!("unexpected key {key}")))
                }
            })
        }
    }

    impl SessionStore for FakeAuthStore {
        fn create_session<'a>(&'a self, token: &'a str, ts: i64) -> SessionWriteFuture<'a> {
            Box::pin(async move {
                self.sessions
                    .lock()
                    .unwrap()
                    .insert((token.to_string(), ts));
                Ok(())
            })
        }

        fn delete_session<'a>(&'a self, token: &'a str) -> SessionWriteFuture<'a> {
            Box::pin(async move {
                self.sessions
                    .lock()
                    .unwrap()
                    .retain(|(stored, _)| stored != token);
                Ok(())
            })
        }

        fn delete_sessions_except<'a>(&'a self, token: &'a str) -> SessionWriteFuture<'a> {
            Box::pin(async move {
                self.sessions
                    .lock()
                    .unwrap()
                    .retain(|(stored, _)| stored == token);
                Ok(())
            })
        }

        fn get_session_created_at<'a>(&'a self, token: &'a str) -> SessionLookupFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .sessions
                    .lock()
                    .unwrap()
                    .iter()
                    .find_map(|(stored, ts)| (stored == token).then_some(*ts)))
            })
        }

        fn prune_expired_sessions<'a>(&'a self, max_age_ms: i64) -> SessionWriteFuture<'a> {
            Box::pin(async move {
                if max_age_ms < 0 {
                    return Err(SessionStoreError::new("invalid max age"));
                }
                Ok(())
            })
        }

        fn list_sessions<'a>(&'a self) -> SessionListFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .sessions
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(token, _)| token.clone())
                    .collect::<Vec<_>>())
            })
        }
    }

    #[tokio::test]
    async fn auth_service_uses_injected_stores() {
        let store = Arc::new(FakeAuthStore::default());
        let service = AuthService::with_stores(store.clone(), store.clone(), store);

        service.ensure_password_hash("first").await.unwrap();
        service.ensure_password_hash("second").await.unwrap();
        service.create_session("token-a", 100).await.unwrap();
        service.create_session("token-b", 200).await.unwrap();
        service.delete_session("token-a").await.unwrap();

        assert_eq!(
            service.get_password_hash().await.unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(service.list_sessions().await.unwrap(), vec!["token-b"]);
    }

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
