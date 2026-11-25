//! Public watch-link operations on the API state.

use super::AppState;

impl AppState {
    /// Lists the watch links of one pipeline.
    pub(crate) async fn share_links_for_pipeline(
        &self,
        pipeline_id: &str,
    ) -> crate::application::services::ServiceResult<Vec<crate::application::share::ShareLink>>
    {
        self.share_links.for_pipeline(pipeline_id).await
    }

    /// Creates one watch link and returns it.
    pub(crate) async fn create_share_link(
        &self,
        id: String,
        token: String,
        pipeline_id: &str,
        label: &str,
        lifetime_hours: Option<i64>,
        now_ms: i64,
    ) -> crate::application::services::ServiceResult<crate::application::share::ShareLink> {
        self.share_links
            .create(id, token, pipeline_id, label, lifetime_hours, now_ms)
            .await
    }

    /// Revokes one watch link.
    pub(crate) async fn revoke_share_link(
        &self,
        id: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.share_links.revoke(id).await
    }

    /// Resolves a live watch credential.
    pub(crate) async fn resolve_share_link(
        &self,
        token: &str,
        now_ms: i64,
    ) -> crate::application::services::ServiceResult<Option<crate::application::share::ShareLink>>
    {
        self.share_links.resolve(token, now_ms).await
    }

    /// Records one best-effort watch view.
    pub(crate) fn count_share_link_view(
        &self,
        id: String,
    ) -> Option<tokio::task::JoinHandle<crate::application::services::ServiceResult<()>>> {
        self.share_links.count_view(id)
    }

    /// Drops every watch link of a deleted pipeline.
    pub(crate) async fn forget_pipeline_share_links(&self, pipeline_id: &str) {
        self.share_links.forget_pipeline(pipeline_id).await;
    }

    /// Returns whether a share credential admits one HLS pipeline.
    pub(crate) async fn share_link_admits(
        &self,
        token: &str,
        pipeline_id: &str,
        now_ms: i64,
    ) -> crate::application::services::ServiceResult<bool> {
        self.share_links.admits(token, pipeline_id, now_ms).await
    }
}
