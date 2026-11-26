//! Persisted ingest catalog reads and writes.

use crate::db::{self, DeadlineDb};
use restream_domain::models::Ingest;

use crate::application::service_error::{ServiceError, ServiceResult};

#[derive(Clone)]
/// Application service for ingest catalog reads and mutations.
pub struct IngestService {
    db: DeadlineDb,
}

impl IngestService {
    pub fn new(db: DeadlineDb) -> Self {
        Self { db }
    }

    /// Builds the shared not-found error shape used when callers reference an
    /// ingest ID that is missing from the catalog.
    fn ingest_not_found(id: &str) -> ServiceError {
        ServiceError::not_found(format!("ingest {id} not found"))
    }

    /// Lists every persisted ingest record without applying higher-level
    /// transport or runtime filtering.
    pub async fn list_ingests(&self) -> ServiceResult<Vec<Ingest>> {
        self.db
            .read(db::list_ingests)
            .await
            .map(|rows| rows.into_iter().map(Ingest::from).collect())
            .map_err(|e| ServiceError::internal(format!("list ingests: {e}")))
    }

    /// Resolves one ingest by ID and upgrades missing-store results into the
    /// service layer's stable not-found error.
    pub async fn get_by_id(&self, id: &str) -> ServiceResult<Ingest> {
        self.db
            .read(|pool| db::get_ingest(pool, id))
            .await
            .map_err(|e| ServiceError::internal(format!("get ingest: {e}")))?
            .ok_or_else(|| Self::ingest_not_found(id))
            .map(Ingest::from)
    }

    #[allow(clippy::too_many_arguments)]
    /// Persists a new ingest record with the caller-provided media source,
    /// stream key, looping flags, and live-optimization settings.
    pub async fn create_ingest(
        &self,
        id: &str,
        filename: &str,
        stream_key: &str,
        loop_flag: bool,
        start_time: &str,
        live_optimized: bool,
        target_gop_seconds: u32,
    ) -> ServiceResult<Ingest> {
        let (id, filename, stream_key, start_time) = (
            id.to_owned(),
            filename.to_owned(),
            stream_key.to_owned(),
            start_time.to_owned(),
        );
        self.db
            .write(move |pool| async move {
                db::create_ingest(
                    &pool,
                    &id,
                    &filename,
                    &stream_key,
                    loop_flag,
                    &start_time,
                    live_optimized,
                    target_gop_seconds,
                )
                .await
            })
            .await
            .map(Ingest::from)
            .map_err(|e| ServiceError::internal(format!("create ingest: {e}")))
    }

    #[allow(clippy::too_many_arguments)]
    /// Updates one persisted ingest and normalizes a missing row into the
    /// service layer's stable not-found error.
    pub async fn update_ingest(
        &self,
        id: &str,
        filename: &str,
        stream_key: &str,
        loop_flag: bool,
        start_time: &str,
        live_optimized: bool,
        target_gop_seconds: u32,
    ) -> ServiceResult<Ingest> {
        let args = (
            id.to_owned(),
            filename.to_owned(),
            stream_key.to_owned(),
            start_time.to_owned(),
        );
        self.db
            .write(move |pool| async move {
                let (id, filename, stream_key, start_time) = args;
                db::update_ingest(
                    &pool,
                    &id,
                    &filename,
                    &stream_key,
                    loop_flag,
                    &start_time,
                    live_optimized,
                    target_gop_seconds,
                )
                .await
            })
            .await
            .map_err(|e| ServiceError::internal(format!("update ingest: {e}")))?
            .ok_or_else(|| Self::ingest_not_found(id))
            .map(Ingest::from)
    }

    /// Renames one media file wherever this ingest uses it (its filename
    /// and every playlist entry).
    pub async fn rename_ingest_file(
        &self,
        id: &str,
        from: &str,
        to: &str,
    ) -> ServiceResult<Ingest> {
        let args = (id.to_owned(), from.to_owned(), to.to_owned());
        self.db
            .write(move |pool| async move {
                let (id, from, to) = args;
                db::rename_ingest_file(&pool, &id, &from, &to).await
            })
            .await
            .map_err(|e| ServiceError::internal(format!("rename ingest file: {e}")))?
            .ok_or_else(|| Self::ingest_not_found(id))
            .map(Ingest::from)
    }

    /// Lists all ingest records that use one media-library filename, as
    /// their file or in their playlist.
    pub async fn list_for_filename(&self, filename: &str) -> ServiceResult<Vec<Ingest>> {
        self.db
            .read(|pool| db::list_ingests_for_filename(pool, filename))
            .await
            .map(|rows| rows.into_iter().map(Ingest::from).collect())
            .map_err(|e| ServiceError::internal(format!("list ingests for filename: {e}")))
    }

    /// Deletes one persisted ingest record from the catalog.
    pub async fn delete_ingest(&self, id: &str) -> ServiceResult<bool> {
        let id = id.to_owned();
        self.db
            .write(move |pool| async move { db::delete_ingest(&pool, &id).await })
            .await
            .map_err(|e| ServiceError::internal(format!("delete ingest: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::bootstrap::test_support::memory_pool;

    async fn service() -> IngestService {
        IngestService::new(DeadlineDb::api(memory_pool().await))
    }

    #[tokio::test]
    async fn ingest_service_persists_creates_updates_and_deletes() {
        let service = service().await;

        let created = service
            .create_ingest("ing-1", "clip.mp4", "stream-key", true, "now", false, 2)
            .await
            .unwrap();
        assert_eq!(created.filename, "clip.mp4");

        let updated = service
            .update_ingest("ing-1", "clip2.mp4", "stream-key", false, "", true, 4)
            .await
            .unwrap();
        assert_eq!(updated.filename, "clip2.mp4");

        let by_filename = service.list_for_filename("clip2.mp4").await.unwrap();
        assert_eq!(by_filename.len(), 1);

        assert!(service.delete_ingest("ing-1").await.unwrap());
        assert!(service.list_ingests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ingest_service_missing_update_is_not_found() {
        let service = service().await;

        let err = service
            .update_ingest("missing", "clip.mp4", "stream-key", false, "", false, 2)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }
}
