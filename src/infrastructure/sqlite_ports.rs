//! SQLite-backed implementations of application storage ports.

mod notify;
pub(super) mod records;

pub use notify::SqliteNotifyStore;

use self::records::{ingest_model, job_model, pipeline_model};
use crate::application::ports::*;
use crate::db::DeadlineDb;
use restream_domain::models::Pipeline;

/// A read that misses the adapter's [`DeadlineDb`] deadline fails as a
/// store error; writes retain their actual outcome without a read deadline.
impl From<crate::db::DeadlineError> for StoreError {
    fn from(error: crate::db::DeadlineError) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Clone)]
pub struct SqlitePipelineStore {
    db: DeadlineDb,
}

#[derive(Clone)]
pub struct SqliteIngestLookup {
    db: DeadlineDb,
}

#[derive(Clone)]
pub struct SqliteMetaStore {
    db: DeadlineDb,
}

#[derive(Clone)]
pub struct SqliteSessionStore {
    db: DeadlineDb,
}

#[derive(Clone)]
pub struct SqliteJobStore {
    db: DeadlineDb,
}

#[derive(Clone)]
pub struct SqliteRecordingStore {
    db: DeadlineDb,
}

impl SqlitePipelineStore {
    pub fn new(db: DeadlineDb) -> Self {
        Self { db }
    }
}

impl SqliteIngestLookup {
    pub fn new(db: DeadlineDb) -> Self {
        Self { db }
    }
}

impl SqliteMetaStore {
    pub fn new(db: DeadlineDb) -> Self {
        Self { db }
    }
}

impl SqliteSessionStore {
    pub fn new(db: DeadlineDb) -> Self {
        Self { db }
    }
}

impl SqliteJobStore {
    pub fn new(db: DeadlineDb) -> Self {
        Self { db }
    }
}

impl SqliteRecordingStore {
    pub fn new(db: DeadlineDb) -> Self {
        Self { db }
    }
}

impl PipelineStore for SqlitePipelineStore {
    fn get_pipeline<'a>(&'a self, id: &'a str) -> PipelineLookupFuture<'a> {
        Box::pin(async move {
            self.db
                .read(|pool| crate::db::get_pipeline(pool, id))
                .await
                .map(|record| record.map(pipeline_model))
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn get_pipeline_by_stream_key<'a>(&'a self, stream_key: &'a str) -> PipelineLookupFuture<'a> {
        Box::pin(async move {
            self.db
                .read(|pool| crate::db::get_pipeline_by_stream_key(pool, stream_key))
                .await
                .map(|record| record.map(pipeline_model))
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn list_pipelines<'a>(&'a self) -> PipelineListFuture<'a> {
        Box::pin(async move {
            self.db
                .read(crate::db::list_pipelines)
                .await
                .map(|records| records.into_iter().map(pipeline_model).collect())
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn create_pipeline<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        stream_key: &'a str,
        input_source: Option<&'a str>,
        srt_ingest_policy: Option<&'a str>,
    ) -> PipelineCreateFuture<'a> {
        Box::pin(async move {
            let (id, name, stream_key) = (id.to_owned(), name.to_owned(), stream_key.to_owned());
            let input_source = input_source.map(str::to_owned);
            let srt_ingest_policy = srt_ingest_policy.map(str::to_owned);
            self.db
                .write(move |pool| async move {
                    crate::db::create_pipeline(
                        &pool,
                        &id,
                        &name,
                        &stream_key,
                        input_source.as_deref(),
                        srt_ingest_policy.as_deref(),
                    )
                    .await
                })
                .await
                .map(pipeline_model)
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn update_pipeline<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        stream_key: &'a str,
        input_source: Option<&'a str>,
        srt_ingest_policy: Option<&'a str>,
    ) -> PipelineUpdateFuture<'a> {
        Box::pin(async move {
            let (id, name, stream_key) = (id.to_owned(), name.to_owned(), stream_key.to_owned());
            let input_source = input_source.map(str::to_owned);
            let srt_ingest_policy = srt_ingest_policy.map(str::to_owned);
            self.db
                .write(move |pool| async move {
                    crate::db::update_pipeline(
                        &pool,
                        &id,
                        &name,
                        &stream_key,
                        input_source.as_deref(),
                        srt_ingest_policy.as_deref(),
                    )
                    .await
                })
                .await
                .map(|record| record.map(pipeline_model))
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn delete_pipeline<'a>(&'a self, id: &'a str) -> PipelineDeleteFuture<'a> {
        Box::pin(async move {
            let id = id.to_owned();
            self.db
                .write(move |pool| async move { crate::db::delete_pipeline(&pool, &id).await })
                .await
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn get_ingest_host<'a>(&'a self) -> PipelineIngestHostFuture<'a> {
        Box::pin(async move {
            self.db
                .read(crate::db::get_ingest_host)
                .await
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn update_pipeline_input_source<'a>(
        &'a self,
        pipeline: &'a Pipeline,
        input_source: Option<&'a str>,
    ) -> PipelineUpdateFuture<'a> {
        Box::pin(async move {
            let id = pipeline.id.clone();
            let input_source = input_source.map(str::to_owned);
            self.db
                .write(move |pool| async move {
                    crate::db::update_pipeline_input_source(&pool, &id, input_source.as_deref())
                        .await
                })
                .await
                .map(|record| record.map(pipeline_model))
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }
}

impl IngestLookup for SqliteIngestLookup {
    fn get_ingest<'a>(&'a self, id: &'a str) -> IngestLookupFuture<'a> {
        Box::pin(async move {
            self.db
                .read(|pool| crate::db::get_ingest(pool, id))
                .await
                .map(|record| record.map(ingest_model))
                .map_err(|err| IngestLookupError::new(err.to_string()))
        })
    }

    fn get_ingest_by_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestLookupFuture<'a> {
        Box::pin(async move {
            self.db
                .read(|pool| crate::db::get_ingest_by_stream_key(pool, stream_key))
                .await
                .map(|record| record.map(ingest_model))
                .map_err(|err| IngestLookupError::new(err.to_string()))
        })
    }

    fn list_ingests<'a>(&'a self) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            self.db
                .read(crate::db::list_ingests)
                .await
                .map(|records| records.into_iter().map(ingest_model).collect())
                .map_err(|err| IngestLookupError::new(err.to_string()))
        })
    }

    fn list_ingests_for_filename<'a>(&'a self, filename: &'a str) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            self.db
                .read(|pool| crate::db::list_ingests_for_filename(pool, filename))
                .await
                .map(|records| records.into_iter().map(ingest_model).collect())
                .map_err(|err| IngestLookupError::new(err.to_string()))
        })
    }

    fn list_ingests_for_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            self.db
                .read(|pool| crate::db::list_ingests_for_stream_key(pool, stream_key))
                .await
                .map(|records| records.into_iter().map(ingest_model).collect())
                .map_err(|err| IngestLookupError::new(err.to_string()))
        })
    }
}

impl IngestWriter for SqliteIngestLookup {
    fn create_ingest<'a>(
        &'a self,
        id: &'a str,
        filename: &'a str,
        stream_key: &'a str,
        loop_flag: bool,
        start_time: &'a str,
        live_optimized: bool,
        target_gop_seconds: u32,
    ) -> IngestWriteFuture<'a> {
        Box::pin(async move {
            let (id, filename, stream_key, start_time) = (
                id.to_owned(),
                filename.to_owned(),
                stream_key.to_owned(),
                start_time.to_owned(),
            );
            self.db
                .write(move |pool| async move {
                    crate::db::create_ingest(
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
                .map(ingest_model)
                .map_err(|err| IngestWriteError::new(err.to_string()))
        })
    }

    fn update_ingest<'a>(
        &'a self,
        id: &'a str,
        filename: &'a str,
        stream_key: &'a str,
        loop_flag: bool,
        start_time: &'a str,
        live_optimized: bool,
        target_gop_seconds: u32,
    ) -> IngestUpdateFuture<'a> {
        Box::pin(async move {
            let (id, filename, stream_key, start_time) = (
                id.to_owned(),
                filename.to_owned(),
                stream_key.to_owned(),
                start_time.to_owned(),
            );
            self.db
                .write(move |pool| async move {
                    crate::db::update_ingest(
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
                .map(|record| record.map(ingest_model))
                .map_err(|err| IngestWriteError::new(err.to_string()))
        })
    }

    fn update_ingest_filename<'a>(
        &'a self,
        id: &'a str,
        filename: &'a str,
    ) -> IngestUpdateFuture<'a> {
        Box::pin(async move {
            let (id, filename) = (id.to_owned(), filename.to_owned());
            self.db
                .write(move |pool| async move {
                    crate::db::update_ingest_filename(&pool, &id, &filename).await
                })
                .await
                .map(|record| record.map(ingest_model))
                .map_err(|err| IngestWriteError::new(err.to_string()))
        })
    }

    fn set_ingest_playlist<'a>(
        &'a self,
        id: &'a str,
        playlist: &'a [String],
    ) -> IngestUpdateFuture<'a> {
        Box::pin(async move {
            let json = (!playlist.is_empty())
                .then(|| serde_json::to_string(playlist))
                .transpose()
                .map_err(|err| IngestWriteError::new(err.to_string()))?;
            let id = id.to_owned();
            self.db
                .write(move |pool| async move {
                    crate::db::set_ingest_playlist(&pool, &id, json.as_deref()).await
                })
                .await
                .map(|record| record.map(ingest_model))
                .map_err(|err| IngestWriteError::new(err.to_string()))
        })
    }

    fn rename_ingest_file<'a>(
        &'a self,
        id: &'a str,
        from: &'a str,
        to: &'a str,
    ) -> IngestUpdateFuture<'a> {
        Box::pin(async move {
            let (id, from, to) = (id.to_owned(), from.to_owned(), to.to_owned());
            self.db
                .write(move |pool| async move {
                    crate::db::rename_ingest_file(&pool, &id, &from, &to).await
                })
                .await
                .map(|record| record.map(ingest_model))
                .map_err(|err| IngestWriteError::new(err.to_string()))
        })
    }

    fn delete_ingest<'a>(&'a self, id: &'a str) -> IngestDeleteFuture<'a> {
        Box::pin(async move {
            let id = id.to_owned();
            self.db
                .write(move |pool| async move { crate::db::delete_ingest(&pool, &id).await })
                .await
                .map_err(|err| IngestWriteError::new(err.to_string()))
        })
    }
}

impl MetaStore for SqliteMetaStore {
    fn get_meta<'a>(&'a self, key: &'a str) -> MetaLookupFuture<'a> {
        Box::pin(async move {
            self.db
                .read(|pool| crate::db::get_meta(pool, key))
                .await
                .map_err(|err| MetaLookupError::new(err.to_string()))
        })
    }
}

impl MetaStoreWriter for SqliteMetaStore {
    fn set_meta<'a>(&'a self, key: &'a str, value: &'a str) -> MetaWriteFuture<'a> {
        Box::pin(async move {
            let (key, value) = (key.to_owned(), value.to_owned());
            self.db
                .write(move |pool| async move { crate::db::set_meta(&pool, &key, &value).await })
                .await
                .map_err(|err| MetaLookupError::new(err.to_string()))
        })
    }
}

impl IngestHostStore for SqliteMetaStore {
    fn get_ingest_host<'a>(&'a self) -> MetaLookupFuture<'a> {
        Box::pin(async move {
            self.db
                .read(crate::db::get_ingest_host)
                .await
                .map_err(|err| MetaLookupError::new(err.to_string()))
        })
    }

    fn set_ingest_host<'a>(&'a self, host: &'a str) -> MetaWriteFuture<'a> {
        Box::pin(async move {
            let host = host.to_owned();
            self.db
                .write(move |pool| async move { crate::db::set_ingest_host(&pool, &host).await })
                .await
                .map_err(|err| MetaLookupError::new(err.to_string()))
        })
    }
}

impl SessionStore for SqliteSessionStore {
    fn create_session<'a>(&'a self, token: &'a str, ts: i64) -> SessionWriteFuture<'a> {
        Box::pin(async move {
            let token = token.to_owned();
            self.db
                .write(
                    move |pool| async move { crate::db::create_session(&pool, &token, ts).await },
                )
                .await
                .map_err(|err| SessionStoreError::new(err.to_string()))
        })
    }

    fn delete_session<'a>(&'a self, token: &'a str) -> SessionWriteFuture<'a> {
        Box::pin(async move {
            let token = token.to_owned();
            self.db
                .write(move |pool| async move { crate::db::delete_session(&pool, &token).await })
                .await
                .map_err(|err| SessionStoreError::new(err.to_string()))
        })
    }

    fn delete_sessions_except<'a>(&'a self, token: &'a str) -> SessionWriteFuture<'a> {
        Box::pin(async move {
            let token = token.to_owned();
            self.db.write(move |pool| async move {
                crate::db::delete_sessions_except(&pool, &token).await
            })
                .await
                .map_err(|err| SessionStoreError::new(err.to_string()))
        })
    }

    fn get_session_created_at<'a>(&'a self, token: &'a str) -> SessionLookupFuture<'a> {
        Box::pin(async move {
            self.db
                .read(|pool| crate::db::get_session_created_at(pool, token))
                .await
                .map_err(|err| SessionStoreError::new(err.to_string()))
        })
    }

    fn prune_expired_sessions<'a>(&'a self, max_age_ms: i64) -> SessionWriteFuture<'a> {
        Box::pin(async move {
            self.db
                .write(move |pool| async move {
                    crate::db::prune_expired_sessions(&pool, max_age_ms).await
                })
                .await
                .map_err(|err| SessionStoreError::new(err.to_string()))
        })
    }

    fn list_sessions<'a>(&'a self) -> SessionListFuture<'a> {
        Box::pin(async move {
            self.db
                .read(crate::db::list_sessions)
                .await
                .map_err(|err| SessionStoreError::new(err.to_string()))
        })
    }
}

impl JobStore for SqliteJobStore {
    fn list_jobs<'a>(&'a self) -> JobListFuture<'a> {
        Box::pin(async move {
            self.db
                .read(crate::db::list_jobs)
                .await
                .map(|records| records.into_iter().map(job_model).collect())
                .map_err(|err| JobStoreError::new(err.to_string()))
        })
    }
}

impl RecordingStore for SqliteRecordingStore {
    fn list_recordings<'a>(&'a self) -> RecordingListFuture<'a> {
        Box::pin(async move {
            self.db
                .read(crate::db::list_recordings)
                .await
                .map(|rows| rows.into_iter().map(recording_catalog_row).collect())
                .map_err(|err| RecordingStoreError::new(err.to_string()))
        })
    }
}

fn recording_catalog_row(row: crate::db::RecordingRow) -> RecordingCatalogRow {
    RecordingCatalogRow {
        recording_id: row.recording_id,
        pipeline_id: row.pipeline_id,
        started_at: row.started_at,
        ended_at: row.ended_at,
        status: row.status,
        temp_path: row.temp_path,
        final_path: row.final_path,
        codec_summary: row.codec_summary,
        error: row.error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    async fn test_pool() -> SqlitePool {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn sqlite_pipeline_store_returns_pipeline_for_stream_key() {
        let pool = test_pool().await;
        crate::db::create_pipeline(&pool, "p1", "Pipeline", "stream-key", None, None)
            .await
            .unwrap();
        let store = SqlitePipelineStore::new(DeadlineDb::api(pool.clone()));

        let pipeline = store
            .get_pipeline_by_stream_key("stream-key")
            .await
            .unwrap();

        assert_eq!(pipeline.unwrap().id, "p1");
    }

    #[tokio::test]
    async fn sqlite_pipeline_store_returns_none_for_missing_stream_key() {
        let pool = test_pool().await;
        let store = SqlitePipelineStore::new(DeadlineDb::api(pool.clone()));

        let pipeline = store.get_pipeline_by_stream_key("missing").await.unwrap();

        assert!(pipeline.is_none());
    }

    #[tokio::test]
    async fn update_pipeline_input_source_does_not_revert_concurrent_rename_or_key_rotation() {
        // Regression test: file-ingest apply/remove (src/application/ingest.rs)
        // fetch a `Pipeline` snapshot once, then call
        // update_pipeline_input_source with it after several `.await` points.
        // If that call rewrote name/stream_key/srt_ingest_policy from the
        // stale snapshot (as it did before this fix, via the shared
        // full-row update_pipeline), a concurrent rename or stream-key
        // rotation landing in that window would be silently reverted —
        // e.g. reviving a just-rotated, potentially-leaked stream key.
        let pool = test_pool().await;
        crate::db::create_pipeline(&pool, "p1", "Original Name", "sk_original", None, None)
            .await
            .unwrap();
        let store = SqlitePipelineStore::new(DeadlineDb::api(pool.clone()));
        let stale_snapshot = store.get_pipeline("p1").await.unwrap().unwrap();

        // Simulate a concurrent request rotating the stream key and renaming
        // the pipeline after the snapshot above was taken, but before the
        // stale caller's update_pipeline_input_source call below runs.
        crate::db::update_pipeline(&pool, "p1", "Renamed", "sk_rotated", None, None)
            .await
            .unwrap();

        store
            .update_pipeline_input_source(&stale_snapshot, Some("file:clip.ts"))
            .await
            .unwrap();

        let after = store.get_pipeline("p1").await.unwrap().unwrap();
        assert_eq!(after.name, "Renamed");
        assert_eq!(after.stream_key, "sk_rotated");
        assert_eq!(after.input_source.as_deref(), Some("file:clip.ts"));
    }

    #[tokio::test]
    async fn sqlite_pipeline_store_lists_pipelines() {
        let pool = test_pool().await;
        crate::db::create_pipeline(&pool, "p1", "Pipeline One", "stream-one", None, None)
            .await
            .unwrap();
        crate::db::create_pipeline(&pool, "p2", "Pipeline Two", "stream-two", None, None)
            .await
            .unwrap();
        let store = SqlitePipelineStore::new(DeadlineDb::api(pool.clone()));

        let pipelines = store.list_pipelines().await.unwrap();

        assert_eq!(pipelines.len(), 2);
        assert!(pipelines.iter().any(|pipeline| pipeline.id == "p1"));
        assert!(pipelines.iter().any(|pipeline| pipeline.id == "p2"));
    }

    #[tokio::test]
    async fn sqlite_ingest_lookup_reads_ingest_by_id_and_stream_key() {
        let pool = test_pool().await;
        crate::db::create_ingest(
            &pool,
            "i1",
            "clip.mp4",
            "stream-key",
            true,
            "00:00:05",
            true,
            4,
        )
        .await
        .unwrap();
        let duplicate = crate::db::create_ingest(
            &pool,
            "i2",
            "clip-latest.mp4",
            "stream-key",
            false,
            "00:00:10",
            false,
            2,
        )
        .await;
        let lookup = SqliteIngestLookup::new(DeadlineDb::api(pool.clone()));

        let by_id = lookup.get_ingest("i1").await.unwrap();
        let by_stream_key = lookup.get_ingest_by_stream_key("stream-key").await.unwrap();

        assert!(duplicate.is_err());
        assert_eq!(by_id.as_ref().map(|ingest| ingest.id.as_str()), Some("i1"));
        assert_eq!(
            by_stream_key.as_ref().map(|ingest| ingest.id.as_str()),
            Some("i1")
        );
    }

    #[tokio::test]
    async fn sqlite_ingest_lookup_lists_ingests_for_stream_key() {
        let pool = test_pool().await;
        crate::db::create_ingest(&pool, "i1", "clip.mp4", "stream-key", true, "", false, 2)
            .await
            .unwrap();
        crate::db::create_ingest(&pool, "i2", "clip-2.mp4", "other-key", false, "", false, 2)
            .await
            .unwrap();
        let duplicate =
            crate::db::create_ingest(&pool, "i3", "clip-3.mp4", "stream-key", false, "", false, 2)
                .await;
        let lookup = SqliteIngestLookup::new(DeadlineDb::api(pool.clone()));

        let ingests = lookup
            .list_ingests_for_stream_key("stream-key")
            .await
            .unwrap();

        assert!(duplicate.is_err());
        assert_eq!(ingests.len(), 1);
        assert_eq!(ingests[0].id, "i1");
    }

    #[tokio::test]
    async fn sqlite_meta_store_returns_meta_value() {
        let pool = test_pool().await;
        crate::db::set_meta(&pool, "test-key", "test-value")
            .await
            .unwrap();
        let store = SqliteMetaStore::new(DeadlineDb::api(pool.clone()));

        let value = store.get_meta("test-key").await.unwrap();

        assert_eq!(value.as_deref(), Some("test-value"));
    }

    #[tokio::test]
    async fn sqlite_meta_store_writes_meta_value() {
        let pool = test_pool().await;
        let store = SqliteMetaStore::new(DeadlineDb::api(pool.clone()));

        store.set_meta("test-key", "test-value").await.unwrap();

        let value = crate::db::get_meta(&pool, "test-key").await.unwrap();
        assert_eq!(value.as_deref(), Some("test-value"));
    }

    #[tokio::test]
    async fn sqlite_recording_store_lists_recordings() {
        let pool = test_pool().await;
        crate::db::create_pipeline(&pool, "pipe-1", "Pipeline", "stream-key", None, None)
            .await
            .unwrap();
        crate::db::create_recording(
            &pool,
            &restream_domain::domain::ids::RecordingId::from("rec-1"),
            "pipe-1",
            "2026-07-09T00:00:00Z",
            Some("/media/recording_1.ts"),
            Some("h264/aac"),
        )
        .await
        .unwrap();
        let store = SqliteRecordingStore::new(DeadlineDb::api(pool.clone()));

        let rows = store.list_recordings().await.unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].recording_id, "rec-1");
        assert_eq!(rows[0].pipeline_id, "pipe-1");
        assert_eq!(rows[0].temp_path.as_deref(), Some("/media/recording_1.ts"));
    }
}
