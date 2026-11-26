//! Share links: a public watch link to one pipeline's live preview, made
//! by an admin and revocable. The link's token admits HLS reads of that
//! pipeline only, until it expires or is revoked; nothing else.
//!
//! Preview authorization uses a shared cache. Reloads and mutations serialize
//! cache publication so a stale read cannot restore a revoked credential.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{Mutex, RwLock};

use crate::application::service_error::{ServiceError, ServiceResult};
use crate::db::DeadlineDb;
use crate::db::share_repo::{self, ShareLinkRecord};

/// The longest a link may live.
pub const MAX_LIFETIME_HOURS: i64 = 24 * 365;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareLink {
    pub id: String,
    pub pipeline_id: String,
    pub label: String,
    /// The secret in the link; admins read it to copy the link again.
    pub token: String,
    pub created_at_ms: i64,
    pub expires_at_ms: Option<i64>,
    /// Watch-page loads.
    pub views: i64,
}

impl ShareLink {
    pub fn live_at(&self, now_ms: i64) -> bool {
        self.expires_at_ms.is_none_or(|expires| now_ms < expires)
    }
}

impl From<ShareLinkRecord> for ShareLink {
    fn from(record: ShareLinkRecord) -> Self {
        Self {
            id: record.id,
            pipeline_id: record.pipeline_id,
            label: record.label,
            token: record.token,
            created_at_ms: record.created_at_ms,
            expires_at_ms: record.expires_at_ms,
            views: record.views,
        }
    }
}

/// The pipeline an HLS request path reads: `/hls/{pipeline}/…`. Input
/// previews (`/hls/inputs/…`) are never shared.
pub fn hls_pipeline_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/hls/")?;
    let preview = rest.split('/').next()?;
    let pipeline = restream_engine::engine_hls::preview_pipeline(preview).0;
    (!pipeline.is_empty() && pipeline != "inputs").then_some(pipeline)
}

/// The `t` query parameter.
pub fn token_of(query: Option<&str>) -> Option<&str> {
    query?
        .split('&')
        .find_map(|pair| pair.strip_prefix("t="))
        .filter(|token| !token.is_empty())
}

pub struct ShareLinks {
    db: DeadlineDb,
    /// token → link; `None` until first read.
    by_token: Arc<RwLock<Option<HashMap<String, ShareLink>>>>,
    refreshing: Arc<Mutex<()>>,
}

impl ShareLinks {
    pub fn new(db: DeadlineDb) -> Self {
        Self {
            db,
            by_token: Arc::new(RwLock::new(None)),
            refreshing: Arc::new(Mutex::new(())),
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "lock order: local cache gate -> DB read -> cache publication; read deadline also bounds waiting for the gate; no media locks"
    )]
    async fn load(&self, refresh: bool) -> ServiceResult<()> {
        if !refresh && self.by_token.read().await.is_some() {
            return Ok(());
        }
        self.db
            .bound(async {
                let _guard = self.refreshing.lock().await;
                if !refresh && self.by_token.read().await.is_some() {
                    return Ok(());
                }
                let links = self
                    .db
                    .read(share_repo::list)
                    .await
                    .map_err(|err| ServiceError::internal(format!("list share links: {err}")))?
                    .into_iter()
                    .map(|record| (record.token.clone(), ShareLink::from(record)))
                    .collect();
                *self.by_token.write().await = Some(links);
                Ok(())
            })
            .await
    }

    /// The live link a token names, if any.
    pub async fn resolve(&self, token: &str, now_ms: i64) -> ServiceResult<Option<ShareLink>> {
        if let Some(links) = self.by_token.read().await.as_ref() {
            return Ok(links
                .get(token)
                .filter(|link| link.live_at(now_ms))
                .cloned());
        }
        self.load(false).await?;
        Ok(self
            .by_token
            .read()
            .await
            .as_ref()
            .and_then(|links| links.get(token))
            .filter(|link| link.live_at(now_ms))
            .cloned())
    }

    /// Whether `token` admits a read of `pipeline_id` now.
    pub async fn admits(&self, token: &str, pipeline_id: &str, now_ms: i64) -> ServiceResult<bool> {
        Ok(self
            .resolve(token, now_ms)
            .await?
            .is_some_and(|link| link.pipeline_id == pipeline_id))
    }

    pub async fn for_pipeline(&self, pipeline_id: &str) -> ServiceResult<Vec<ShareLink>> {
        // Views change on every watch-page load: read them fresh.
        self.load(true).await?;
        let mut links: Vec<ShareLink> = self
            .by_token
            .read()
            .await
            .as_ref()
            .into_iter()
            .flat_map(|links| links.values())
            .filter(|link| link.pipeline_id == pipeline_id)
            .cloned()
            .collect();
        links.sort_by(|a, b| a.created_at_ms.cmp(&b.created_at_ms).then(a.id.cmp(&b.id)));
        Ok(links)
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "lock order: local cache gate -> DB write admission -> cache publication; no media locks"
    )]
    pub async fn create(
        &self,
        id: String,
        token: String,
        pipeline_id: &str,
        label: &str,
        lifetime_hours: Option<i64>,
        now_ms: i64,
    ) -> ServiceResult<ShareLink> {
        let label = label.trim();
        if label.is_empty() || label.len() > 80 {
            return Err(ServiceError::conflict(
                "A link needs a label of 1-80 characters.",
            ));
        }
        if lifetime_hours.is_some_and(|hours| !(1..=MAX_LIFETIME_HOURS).contains(&hours)) {
            return Err(ServiceError::conflict(
                "A link lives between 1 hour and a year, or until revoked.",
            ));
        }
        let record = ShareLinkRecord {
            id,
            pipeline_id: pipeline_id.to_string(),
            label: label.to_string(),
            token,
            created_at_ms: now_ms,
            expires_at_ms: lifetime_hours.map(|hours| now_ms + hours * 3_600_000),
            views: 0,
        };
        // Acquire the local gate before the global write permit. Moving it
        // into the write owner keeps publication alive after caller cancellation.
        let guard = self.refreshing.clone().lock_owned().await;
        let cache = self.by_token.clone();
        self.db
            .write(move |pool| async move {
                let _guard = guard;
                share_repo::insert(&pool, &record)
                    .await
                    .map_err(|err| ServiceError::internal(format!("save share link: {err}")))?;
                let link = ShareLink::from(record);
                if let Some(links) = cache.write().await.as_mut() {
                    links.insert(link.token.clone(), link.clone());
                }
                Ok(link)
            })
            .await
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "lock order: local cache gate -> DB write admission -> cache publication; no media locks"
    )]
    pub async fn revoke(&self, id: &str) -> ServiceResult<()> {
        let guard = self.refreshing.clone().lock_owned().await;
        let cache = self.by_token.clone();
        let id = id.to_owned();
        self.db
            .write(move |pool| async move {
                let _guard = guard;
                let deleted = share_repo::delete(&pool, &id)
                    .await
                    .map_err(|err| ServiceError::internal(format!("delete share link: {err}")))?;
                if let Some(links) = cache.write().await.as_mut() {
                    links.retain(|_, link| link.id != id);
                }
                if !deleted {
                    return Err(ServiceError::not_found(format!("no share link {id}")));
                }
                Ok(())
            })
            .await
    }

    /// Drops every link to a deleted pipeline.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "lock order: local cache gate -> DB write admission -> cache publication; no media locks"
    )]
    pub async fn forget_pipeline(&self, pipeline_id: &str) {
        let guard = self.refreshing.clone().lock_owned().await;
        let cache = self.by_token.clone();
        let pipeline_id = pipeline_id.to_owned();
        let _: ServiceResult<()> = self
            .db
            .write(move |pool| async move {
                let _guard = guard;
                let result = share_repo::delete_for_pipeline(&pool, &pipeline_id).await;
                if let Some(links) = cache.write().await.as_mut() {
                    links.retain(|_, link| link.pipeline_id != pipeline_id);
                }
                result.map_err(|err| ServiceError::internal(format!("delete share links: {err}")))
            })
            .await;
    }

    /// Best-effort analytics must not gate playback or queue under pressure.
    pub fn count_view(&self, id: String) -> Option<tokio::task::JoinHandle<ServiceResult<()>>> {
        self.db.try_spawn_write(move |pool| async move {
            share_repo::count_view(&pool, &id)
                .await
                .map_err(|err| ServiceError::internal(format!("count share view: {err}")))
        })
    }

    /// Every link, for admins.
    pub async fn all(&self) -> ServiceResult<Vec<ShareLink>> {
        self.load(false).await?;
        Ok(self
            .by_token
            .read()
            .await
            .as_ref()
            .into_iter()
            .flat_map(|links| links.values().cloned())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    async fn links() -> (ShareLinks, SqlitePool) {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        // Links reference their pipeline.
        for id in ["p", "p1"] {
            crate::db::create_pipeline(&pool, id, id, &format!("key-{id}"), None, None)
                .await
                .unwrap();
        }
        (ShareLinks::new(DeadlineDb::api(pool.clone())), pool)
    }

    #[test]
    fn only_pipeline_previews_are_shareable_paths() {
        assert_eq!(hls_pipeline_of("/hls/p1/master.m3u8"), Some("p1"));
        assert_eq!(hls_pipeline_of("/hls/p1/video/seg3.m4s"), Some("p1"));
        assert_eq!(hls_pipeline_of("/hls/inputs/i1/master.m3u8"), None);
        assert_eq!(hls_pipeline_of("/api/v1/state"), None);
        assert_eq!(token_of(Some("a=1&t=abc")), Some("abc"));
        assert_eq!(token_of(Some("t=")), None);
        assert_eq!(token_of(None), None);
    }

    #[tokio::test]
    async fn a_token_admits_its_pipeline_until_it_expires_or_is_revoked() {
        let (links, _pool) = links().await;
        let link = links
            .create("s1".into(), "tok".into(), "p1", "Lobby", Some(2), 0)
            .await
            .unwrap();
        assert!(links.admits("tok", "p1", 1).await.unwrap());
        assert!(
            !links.admits("tok", "p2", 1).await.unwrap(),
            "another pipeline"
        );
        assert!(!links.admits("nope", "p1", 1).await.unwrap());
        assert!(
            !links.admits("tok", "p1", 2 * 3_600_000).await.unwrap(),
            "expired"
        );
        links.revoke(&link.id).await.unwrap();
        assert!(
            !links.admits("tok", "p1", 1).await.unwrap(),
            "revoked at once"
        );
    }

    #[tokio::test]
    async fn links_are_validated_and_counted() {
        let (links, _pool) = links().await;
        assert!(
            links
                .create("s".into(), "t".into(), "p", " ", None, 0)
                .await
                .is_err()
        );
        assert!(
            links
                .create("s".into(), "t".into(), "p", "x", Some(0), 0)
                .await
                .is_err()
        );
        let link = links
            .create("s".into(), "t".into(), "p", "Forever", None, 0)
            .await
            .unwrap();
        assert!(link.live_at(i64::MAX));
        links.count_view(link.id).unwrap().await.unwrap().unwrap();
        assert_eq!(links.for_pipeline("p").await.unwrap()[0].views, 1);
        links.forget_pipeline("p").await;
        assert!(!links.admits("t", "p", 1).await.unwrap());
    }

    #[tokio::test]
    async fn cancelled_revoke_finishes_cache_invalidation() {
        let (links, pool) = links().await;
        let links = Arc::new(links);
        links
            .create("cancelled".into(), "tok".into(), "p", "Lobby", None, 0)
            .await
            .unwrap();
        assert!(links.admits("tok", "p", 1).await.unwrap());
        let held = pool.acquire().await.unwrap();
        let pending = tokio::spawn({
            let links = links.clone();
            async move { links.revoke("cancelled").await }
        });
        // Once this gate is owned, revoke has handed it to the write task
        // before yielding. Holding the only connection prevents completion.
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while links.refreshing.try_lock().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        pending.abort();
        assert!(pending.await.unwrap_err().is_cancelled());
        drop(held);
        let _complete =
            tokio::time::timeout(std::time::Duration::from_secs(1), links.refreshing.lock())
                .await
                .unwrap();
        drop(_complete);
        assert!(share_repo::list(&pool).await.unwrap().is_empty());
        assert!(!links.admits("tok", "p", 1).await.unwrap());
    }

    #[tokio::test]
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "the test holds the local cache gate across requests to prove the read deadline bounds its wait and warm admission bypasses it"
    )]
    async fn metadata_refresh_waits_for_the_local_gate_only_until_its_read_deadline() {
        let (mut links, pool) = links().await;
        links.db = DeadlineDb::new(pool.clone(), std::time::Duration::from_millis(50));
        links
            .create("gate".into(), "tok".into(), "p", "Lobby", None, 0)
            .await
            .unwrap();
        assert!(links.admits("tok", "p", 1).await.unwrap());
        // A different writer changes metadata after this instance warmed its
        // authorization cache, so recovery must read fresh rows.
        share_repo::count_view(&pool, "gate").await.unwrap();
        let gate = links.refreshing.lock().await;
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            links.for_pipeline("p"),
        )
        .await
        .expect("the read deadline includes the local gate wait");
        assert!(matches!(blocked, Err(ServiceError::Internal(_))));
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(500),
                links.admits("tok", "p", 1),
            )
            .await
            .expect("warm HLS authorization never waits for a metadata refresh")
            .unwrap()
        );
        drop(gate);
        let refreshed = links.for_pipeline("p").await.unwrap();
        assert_eq!(refreshed.len(), 1);
        assert_eq!(refreshed[0].views, 1);
    }

    #[tokio::test]
    async fn a_paused_database_reload_cannot_restore_a_revoked_cached_token() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let dir = std::env::temp_dir().join(format!(
            "restream-share-reload-{}-{}",
            std::process::id(),
            rand::random::<u64>(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let armed = Arc::new(AtomicBool::new(false));
        let (paused_tx, paused_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let pause = Arc::new(std::sync::Mutex::new(Some((paused_tx, resume_rx))));
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(dir.join("share.sqlite"))
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .collation("BINARY", {
                let armed = armed.clone();
                move |left, right| {
                    // Equal timestamps force ORDER BY to compare these ids.
                    // This runs on SQLx's native SQLite worker, not Tokio.
                    let is_pair = (left == "link-a" && right == "link-b")
                        || (left == "link-b" && right == "link-a");
                    if is_pair && armed.swap(false, Ordering::AcqRel) {
                        let control = restream_sync::lock::lock(&pause).take();
                        if let Some((paused, resume)) = control {
                            let _ = paused.send(());
                            // Never panic or wait indefinitely across SQLite's
                            // native callback boundary, even if the test fails.
                            let _ = resume.recv_timeout(Duration::from_secs(5));
                        }
                    }
                    left.cmp(right)
                }
            });
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .min_connections(2)
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        crate::db::create_pipeline(&pool, "p", "Pipeline", "key", None, None)
            .await
            .unwrap();
        let links = Arc::new(ShareLinks::new(DeadlineDb::new(
            pool.clone(),
            Duration::from_secs(3),
        )));
        for (id, token) in [("link-a", "revoked"), ("link-b", "kept")] {
            links
                .create(id.into(), token.into(), "p", id, None, 0)
                .await
                .unwrap();
        }
        // Warm the actual SELECT and cache before arming the comparator,
        // including a read while the other connection is occupied.
        links.load(true).await.unwrap();
        let held = pool.acquire().await.unwrap();
        links.load(true).await.unwrap();
        drop(held);
        assert!(links.admits("revoked", "p", 1).await.unwrap());
        armed.store(true, Ordering::Release);
        let reload = tokio::spawn({
            let links = links.clone();
            async move { links.load(true).await }
        });
        tokio::time::timeout(Duration::from_secs(2), paused_rx)
            .await
            .expect("the real SELECT reaches its id-order comparison")
            .unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut revoke = tokio::spawn({
            let links = links.clone();
            async move {
                let _ = started_tx.send(());
                links.revoke("link-a").await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), started_rx)
            .await
            .unwrap()
            .unwrap();
        // Borrowing the handle observes the block without cancelling revoke.
        let early = tokio::time::timeout(Duration::from_millis(100), &mut revoke).await;
        resume_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), reload)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Ok(result) = early {
            result.unwrap().unwrap();
        } else {
            tokio::time::timeout(Duration::from_secs(3), revoke)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        assert!(
            !links.admits("revoked", "p", 1).await.unwrap(),
            "a pre-revocation SELECT must never republish the revoked credential"
        );
        assert!(links.admits("kept", "p", 1).await.unwrap());
        assert_eq!(
            share_repo::list(&pool)
                .await
                .unwrap()
                .into_iter()
                .map(|link| link.token)
                .collect::<Vec<_>>(),
            ["kept"]
        );
        drop(links);
        pool.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
