//! Share links: a public watch link to one pipeline's live preview, made
//! by an admin and revocable. The link's token admits HLS reads of that
//! pipeline only, until it expires or is revoked; nothing else.
//!
//! Every preview segment is checked against the links, so they are held in
//! memory and reloaded from SQLite on each change (`db::*`, no port, as for
//! outputs).

use std::collections::HashMap;

use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::application::services::error::{ServiceError, ServiceResult};
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
    pool: SqlitePool,
    /// token → link; `None` until first read.
    by_token: RwLock<Option<HashMap<String, ShareLink>>>,
}

impl ShareLinks {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            by_token: RwLock::new(None),
        }
    }

    async fn reload(&self) -> ServiceResult<HashMap<String, ShareLink>> {
        let links: HashMap<String, ShareLink> = share_repo::list(&self.pool)
            .await
            .map_err(|err| ServiceError::internal(format!("list share links: {err}")))?
            .into_iter()
            .map(|record| (record.token.clone(), ShareLink::from(record)))
            .collect();
        *self.by_token.write().await = Some(links.clone());
        Ok(links)
    }

    async fn snapshot(&self) -> ServiceResult<HashMap<String, ShareLink>> {
        if let Some(links) = self.by_token.read().await.as_ref() {
            return Ok(links.clone());
        }
        self.reload().await
    }

    /// The live link a token names, if any.
    pub async fn resolve(&self, token: &str, now_ms: i64) -> Option<ShareLink> {
        if let Some(links) = self.by_token.read().await.as_ref() {
            return links
                .get(token)
                .filter(|link| link.live_at(now_ms))
                .cloned();
        }
        self.reload()
            .await
            .ok()?
            .remove(token)
            .filter(|link| link.live_at(now_ms))
    }

    /// Whether `token` admits a read of `pipeline_id` now.
    pub async fn admits(&self, token: &str, pipeline_id: &str, now_ms: i64) -> bool {
        self.resolve(token, now_ms)
            .await
            .is_some_and(|link| link.pipeline_id == pipeline_id)
    }

    pub async fn for_pipeline(&self, pipeline_id: &str) -> ServiceResult<Vec<ShareLink>> {
        // Views change on every watch-page load: read them fresh.
        let mut links: Vec<ShareLink> = self
            .reload()
            .await?
            .into_values()
            .filter(|link| link.pipeline_id == pipeline_id)
            .collect();
        links.sort_by(|a, b| a.created_at_ms.cmp(&b.created_at_ms).then(a.id.cmp(&b.id)));
        Ok(links)
    }

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
        share_repo::insert(&self.pool, &record)
            .await
            .map_err(|err| ServiceError::internal(format!("save share link: {err}")))?;
        self.reload().await?;
        Ok(record.into())
    }

    pub async fn revoke(&self, id: &str) -> ServiceResult<()> {
        let deleted = share_repo::delete(&self.pool, id)
            .await
            .map_err(|err| ServiceError::internal(format!("delete share link: {err}")))?;
        self.reload().await?;
        if !deleted {
            return Err(ServiceError::not_found(format!("no share link {id}")));
        }
        Ok(())
    }

    /// Drops every link to a deleted pipeline.
    pub async fn forget_pipeline(&self, pipeline_id: &str) {
        let _ = share_repo::delete_for_pipeline(&self.pool, pipeline_id).await;
        let _ = self.reload().await;
    }

    pub async fn count_view(&self, id: &str) {
        let _ = share_repo::count_view(&self.pool, id).await;
    }

    /// Every link, for admins.
    pub async fn all(&self) -> ServiceResult<Vec<ShareLink>> {
        Ok(self.snapshot().await?.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn links() -> ShareLinks {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        // Links reference their pipeline.
        for id in ["p", "p1"] {
            crate::db::create_pipeline(&pool, id, id, &format!("key-{id}"), None, None)
                .await
                .unwrap();
        }
        ShareLinks::new(pool)
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
        let links = links().await;
        let link = links
            .create("s1".into(), "tok".into(), "p1", "Lobby", Some(2), 0)
            .await
            .unwrap();
        assert!(links.admits("tok", "p1", 1).await);
        assert!(!links.admits("tok", "p2", 1).await, "another pipeline");
        assert!(!links.admits("nope", "p1", 1).await);
        assert!(!links.admits("tok", "p1", 2 * 3_600_000).await, "expired");
        links.revoke(&link.id).await.unwrap();
        assert!(!links.admits("tok", "p1", 1).await, "revoked at once");
    }

    #[tokio::test]
    async fn links_are_validated_and_counted() {
        let links = links().await;
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
        links.count_view(&link.id).await;
        assert_eq!(links.for_pipeline("p").await.unwrap()[0].views, 1);
        links.forget_pipeline("p").await;
        assert!(!links.admits("t", "p", 1).await);
    }
}
