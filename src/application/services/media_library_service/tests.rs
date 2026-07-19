use super::*;
use crate::application::ports::{
    IngestCatalogFuture, IngestDeleteFuture, IngestLookup, IngestLookupFuture, IngestUpdateFuture,
    IngestWriteError, IngestWriteFuture, IngestWriter, MetaLookupError, MetaStoreWriter,
    MetaWriteFuture,
};
use crate::domain::ids::RecordingId;
use crate::infrastructure::service_wiring::SqliteServiceFactory;
use crate::infrastructure::sqlite_ports::{SqliteMetaStore, SqliteRecordingStore};
use std::sync::Mutex;

fn sqlite_pipeline_service(pool: &sqlx::SqlitePool) -> PipelineService {
    SqliteServiceFactory::new(pool).pipeline_service()
}

fn sqlite_ingest_service(pool: &sqlx::SqlitePool) -> IngestService {
    SqliteServiceFactory::new(pool).ingest_service()
}

fn sqlite_media_library_service(pool: &sqlx::SqlitePool) -> MediaLibraryService {
    let factory = SqliteServiceFactory::new(pool);
    factory.media_library_service(factory.pipeline_service(), factory.ingest_service())
}

async fn service_with_pipeline() -> MediaLibraryService {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    crate::db::create_pipeline(&pool, "pipe-1", "Pipeline", "key-1", None, None)
        .await
        .unwrap();
    crate::db::create_recording(
        &pool,
        &RecordingId::from("rec-1"),
        "pipe-1",
        "2026-07-09T00:00:00Z",
        Some("/tmp/recording_1.ts"),
        Some("h264/aac"),
    )
    .await
    .unwrap();
    crate::db::finalize_recording(
        &pool,
        &RecordingId::from("rec-1"),
        "2026-07-09T00:01:00Z",
        "/media/finished.mp4",
    )
    .await
    .unwrap();

    sqlite_media_library_service(&pool)
}

struct RenameRollbackIngestStore {
    ingests: Mutex<Vec<Ingest>>,
    fail_id: String,
    fail_filename: String,
}

struct FailingMetaWriter;

impl MetaStoreWriter for FailingMetaWriter {
    fn set_meta<'a>(&'a self, _key: &'a str, _value: &'a str) -> MetaWriteFuture<'a> {
        Box::pin(async move { Err(MetaLookupError::new("injected meta write failure")) })
    }
}

impl RenameRollbackIngestStore {
    fn new(ingests: Vec<Ingest>, fail_id: &str, fail_filename: &str) -> Self {
        Self {
            ingests: Mutex::new(ingests),
            fail_id: fail_id.to_string(),
            fail_filename: fail_filename.to_string(),
        }
    }

    fn snapshot(&self) -> Vec<Ingest> {
        self.ingests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl IngestLookup for RenameRollbackIngestStore {
    fn get_ingest<'a>(&'a self, id: &'a str) -> IngestLookupFuture<'a> {
        Box::pin(async move {
            Ok(self
                .ingests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .find(|ingest| ingest.id == id)
                .cloned())
        })
    }

    fn get_ingest_by_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestLookupFuture<'a> {
        Box::pin(async move {
            Ok(self
                .ingests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .find(|ingest| ingest.stream_key == stream_key)
                .cloned())
        })
    }

    fn list_ingests<'a>(&'a self) -> IngestCatalogFuture<'a> {
        Box::pin(async move { Ok(self.snapshot()) })
    }

    fn list_ingests_for_filename<'a>(&'a self, filename: &'a str) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            Ok(self
                .snapshot()
                .into_iter()
                .filter(|ingest| ingest.filename == filename)
                .collect())
        })
    }

    fn list_ingests_for_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            Ok(self
                .snapshot()
                .into_iter()
                .filter(|ingest| ingest.stream_key == stream_key)
                .collect())
        })
    }
}

impl IngestWriter for RenameRollbackIngestStore {
    fn create_ingest<'a>(
        &'a self,
        _id: &'a str,
        _filename: &'a str,
        _stream_key: &'a str,
        _loop_flag: bool,
        _start_time: &'a str,
        _live_optimized: bool,
        _target_gop_seconds: u32,
    ) -> IngestWriteFuture<'a> {
        Box::pin(async move { Err(IngestWriteError::new("not implemented")) })
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
            if id == self.fail_id && filename == self.fail_filename {
                return Err(IngestWriteError::new("injected update failure"));
            }
            let mut ingests = self
                .ingests
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(ingest) = ingests.iter_mut().find(|ingest| ingest.id == id) else {
                return Ok(None);
            };
            ingest.filename = filename.to_string();
            ingest.stream_key = stream_key.to_string();
            ingest.loop_flag = loop_flag;
            ingest.start_time = start_time.to_string();
            ingest.live_optimized = live_optimized;
            ingest.target_gop_seconds = target_gop_seconds;
            Ok(Some(ingest.clone()))
        })
    }

    fn update_ingest_filename<'a>(
        &'a self,
        id: &'a str,
        filename: &'a str,
    ) -> IngestUpdateFuture<'a> {
        Box::pin(async move {
            if id == self.fail_id && filename == self.fail_filename {
                return Err(IngestWriteError::new("injected update failure"));
            }
            let mut ingests = self
                .ingests
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(ingest) = ingests.iter_mut().find(|ingest| ingest.id == id) else {
                return Ok(None);
            };
            ingest.filename = filename.to_string();
            Ok(Some(ingest.clone()))
        })
    }

    fn delete_ingest<'a>(&'a self, _id: &'a str) -> IngestDeleteFuture<'a> {
        Box::pin(async move { Ok(false) })
    }
}

#[tokio::test]
async fn recording_metadata_by_filename_matches_final_and_temp_basenames() {
    let service = service_with_pipeline().await;
    let metadata = service
        .recording_metadata_by_filename(vec![
            "finished.mp4".to_string(),
            "recording_1.ts".to_string(),
            "other.mp4".to_string(),
        ])
        .await
        .unwrap();

    assert_eq!(metadata["finished.mp4"].recording_id, "rec-1");
    assert_eq!(metadata["recording_1.ts"].pipeline_id, "pipe-1");
    assert!(!metadata.contains_key("other.mp4"));
}

#[tokio::test]
async fn list_media_files_groups_recording_companions_and_ingest_counts() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    crate::db::create_ingest(
        &pool,
        "ing-list",
        "recording_20260709T010203_demo.ts",
        "stream-key",
        false,
        "",
        false,
        crate::application::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
    )
    .await
    .unwrap();
    let service = sqlite_media_library_service(&pool);
    let temp_dir = tempfile_dir("media-list-service");
    let source = temp_dir.join("recording_20260709T010203_demo.ts");
    let converted = temp_dir.join("recording_20260709T010203_demo.mp4");
    let state = temp_dir.join("recording_20260709T010203_demo.ts.conversion.json");
    std::fs::write(&source, b"source").unwrap();
    std::fs::write(&converted, b"converted").unwrap();
    std::fs::write(
        &state,
        serde_json::to_vec(&crate::media::recording::RecordingConversionState {
            status: crate::media::recording::RecordingConversionStatus::Ready,
            updated_at: "2026-07-09T01:02:03Z".to_string(),
            error: None,
        })
        .unwrap(),
    )
    .unwrap();

    let files = service.list_media_files(temp_dir.to_str().unwrap()).await;

    assert_eq!(files.len(), 1);
    let file = &files[0];
    assert_eq!(file.name, "recording_20260709T010203_demo.ts");
    assert_eq!(file.kind, "recording");
    assert_eq!(file.ingest_count, 1);
    assert_eq!(
        file.converted_name.as_deref(),
        Some("recording_20260709T010203_demo.mp4")
    );
    assert_eq!(
        file.play_name.as_deref(),
        Some("recording_20260709T010203_demo.mp4")
    );
    assert_eq!(file.conversion_status.as_deref(), Some("ready"));
    assert_eq!(
        file.size,
        b"source".len() as u64 + b"converted".len() as u64
    );
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn delete_paths_for_media_includes_recording_companions() {
    let service = service_with_pipeline().await;
    let temp_dir = tempfile_dir("media-delete-plan");
    let source = temp_dir.join("recording_20260709T010203_demo.ts");
    let converted = temp_dir.join("recording_20260709T010203_demo.mp4");
    let state = temp_dir.join("recording_20260709T010203_demo.ts.conversion.json");
    std::fs::write(&source, b"source").unwrap();
    std::fs::write(&converted, b"converted").unwrap();
    std::fs::write(&state, b"state").unwrap();

    let paths = service.delete_paths_for_media(
        "recording_20260709T010203_demo.ts",
        &std::fs::canonicalize(&source).unwrap(),
    );

    assert_eq!(paths.len(), 3);
    assert!(paths.iter().any(|path| path.ends_with(&converted)));
    assert!(paths.iter().any(|path| path.ends_with(&state)));
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn rename_pairs_for_media_includes_recording_companions() {
    let service = service_with_pipeline().await;
    let temp_dir = tempfile_dir("media-rename-plan");
    let source = temp_dir.join("recording_20260709T010203_demo.ts");
    let converted = temp_dir.join("recording_20260709T010203_demo.mp4");
    let state = temp_dir.join("recording_20260709T010203_demo.ts.conversion.json");
    let destination = temp_dir.join("recording_20260709T010203_renamed.ts");
    std::fs::write(&source, b"source").unwrap();
    std::fs::write(&converted, b"converted").unwrap();
    std::fs::write(&state, b"state").unwrap();

    let pairs = service
        .rename_pairs_for_media("recording_20260709T010203_demo.ts", &source, &destination)
        .unwrap();

    assert_eq!(pairs.len(), 3);
    assert!(
        pairs
            .iter()
            .any(|(_, to)| { to.ends_with("recording_20260709T010203_renamed.mp4") })
    );
    assert!(
        pairs.iter().any(|(_, to)| {
            to.ends_with("recording_20260709T010203_renamed.ts.conversion.json")
        })
    );
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn rename_pairs_for_media_reports_companion_conflict() {
    let service = service_with_pipeline().await;
    let temp_dir = tempfile_dir("media-rename-conflict");
    let source = temp_dir.join("recording_20260709T010203_demo.ts");
    let converted = temp_dir.join("recording_20260709T010203_demo.mp4");
    let destination = temp_dir.join("recording_20260709T010203_renamed.ts");
    let destination_converted = temp_dir.join("recording_20260709T010203_renamed.mp4");
    std::fs::write(&source, b"source").unwrap();
    std::fs::write(&converted, b"converted").unwrap();
    std::fs::write(&destination_converted, b"existing").unwrap();

    let err = service
        .rename_pairs_for_media("recording_20260709T010203_demo.ts", &source, &destination)
        .unwrap_err();

    assert_eq!(err, MediaRenamePlanError::ConvertedExists);
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn rename_media_file_moves_companions_and_updates_ingests() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    crate::db::create_ingest(
        &pool,
        "ing-rename",
        "recording_20260709T010203_demo.ts",
        "stream-key",
        true,
        "00:00:01",
        true,
        crate::application::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
    )
    .await
    .unwrap();
    let service = sqlite_media_library_service(&pool);
    let temp_dir = tempfile_dir("media-rename-exec");
    let source = temp_dir.join("recording_20260709T010203_demo.ts");
    let converted = temp_dir.join("recording_20260709T010203_demo.mp4");
    let state = temp_dir.join("recording_20260709T010203_demo.ts.conversion.json");
    let destination = temp_dir.join("recording_20260709T010203_renamed.ts");
    std::fs::write(&source, b"source").unwrap();
    std::fs::write(&converted, b"converted").unwrap();
    std::fs::write(&state, b"state").unwrap();

    let updated = service
        .rename_media_file(
            "recording_20260709T010203_demo.ts",
            "recording_20260709T010203_renamed.ts",
            &std::fs::canonicalize(&source).unwrap(),
            &destination,
        )
        .await
        .unwrap();

    assert_eq!(updated, 1);
    assert!(!source.exists());
    assert!(!converted.exists());
    assert!(!state.exists());
    assert!(destination.exists());
    assert!(
        temp_dir
            .join("recording_20260709T010203_renamed.mp4")
            .exists()
    );
    assert!(
        temp_dir
            .join("recording_20260709T010203_renamed.ts.conversion.json")
            .exists()
    );
    let renamed_ingests =
        crate::db::list_ingests_for_filename(&pool, "recording_20260709T010203_renamed.ts")
            .await
            .unwrap();
    assert_eq!(renamed_ingests.len(), 1);
    assert_eq!(renamed_ingests[0].id, "ing-rename");
    assert!(renamed_ingests[0].loop_flag);
    assert!(renamed_ingests[0].live_optimized);
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn rename_media_file_rolls_back_prior_ingest_updates_on_later_failure() {
    let old_name = "source.ts";
    let new_name = "renamed.ts";
    let first = Ingest {
        id: "ing-1".to_string(),
        filename: old_name.to_string(),
        stream_key: "stream-key-1".to_string(),
        loop_flag: true,
        start_time: "00:00:01".to_string(),
        live_optimized: true,
        target_gop_seconds: 2,
    };
    let second = Ingest {
        id: "ing-2".to_string(),
        filename: old_name.to_string(),
        stream_key: "stream-key-2".to_string(),
        loop_flag: false,
        start_time: String::new(),
        live_optimized: false,
        target_gop_seconds: 4,
    };
    let ingest_store = Arc::new(RenameRollbackIngestStore::new(
        vec![first.clone(), second.clone()],
        "ing-2",
        new_name,
    ));
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    let service = MediaLibraryService::with_stores(
        Arc::new(SqliteMetaStore::new(pool.clone())),
        Arc::new(SqliteMetaStore::new(pool.clone())),
        Arc::new(SqliteRecordingStore::new(pool.clone())),
        sqlite_pipeline_service(&pool),
        IngestService::with_ports(ingest_store.clone(), ingest_store.clone()),
    );
    let temp_dir = tempfile_dir("media-rename-ingest-rollback");
    let source = temp_dir.join(old_name);
    let destination = temp_dir.join(new_name);
    std::fs::write(&source, b"source").unwrap();

    let err = service
        .rename_media_file(
            old_name,
            new_name,
            &std::fs::canonicalize(&source).unwrap(),
            &destination,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, MediaRenameError::IngestUpdate(_)));
    assert!(source.exists());
    assert!(!destination.exists());
    let restored = ingest_store.snapshot();
    assert_eq!(restored.len(), 2);
    assert_eq!(restored[0].id, first.id);
    assert_eq!(restored[0].filename, first.filename);
    assert_eq!(restored[0].stream_key, first.stream_key);
    assert_eq!(restored[0].loop_flag, first.loop_flag);
    assert_eq!(restored[0].start_time, first.start_time);
    assert_eq!(restored[0].live_optimized, first.live_optimized);
    assert_eq!(restored[0].target_gop_seconds, first.target_gop_seconds);
    assert_eq!(restored[1].id, second.id);
    assert_eq!(restored[1].filename, second.filename);
    assert_eq!(restored[1].stream_key, second.stream_key);
    assert_eq!(restored[1].loop_flag, second.loop_flag);
    assert_eq!(restored[1].start_time, second.start_time);
    assert_eq!(restored[1].live_optimized, second.live_optimized);
    assert_eq!(restored[1].target_gop_seconds, second.target_gop_seconds);
    let _ = std::fs::remove_dir_all(temp_dir);
}

struct ConcurrentWriteIngestStore {
    ingests: Mutex<Vec<Ingest>>,
    concurrent_stream_key: String,
}

impl ConcurrentWriteIngestStore {
    fn new(ingest: Ingest, concurrent_stream_key: &str) -> Self {
        Self {
            ingests: Mutex::new(vec![ingest]),
            concurrent_stream_key: concurrent_stream_key.to_string(),
        }
    }

    fn snapshot(&self) -> Vec<Ingest> {
        self.ingests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl IngestLookup for ConcurrentWriteIngestStore {
    fn get_ingest<'a>(&'a self, id: &'a str) -> IngestLookupFuture<'a> {
        Box::pin(async move { Ok(self.snapshot().into_iter().find(|ingest| ingest.id == id)) })
    }

    fn get_ingest_by_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestLookupFuture<'a> {
        Box::pin(async move {
            Ok(self
                .snapshot()
                .into_iter()
                .find(|ingest| ingest.stream_key == stream_key))
        })
    }

    fn list_ingests<'a>(&'a self) -> IngestCatalogFuture<'a> {
        Box::pin(async move { Ok(self.snapshot()) })
    }

    fn list_ingests_for_filename<'a>(&'a self, filename: &'a str) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            // Snapshot what a caller of this lookup sees, then simulate a
            // concurrent request rotating the stream key in the window
            // between this snapshot and whatever the caller does with it.
            let snapshot = self
                .snapshot()
                .into_iter()
                .filter(|ingest| ingest.filename == filename)
                .collect::<Vec<_>>();
            let mut ingests = self
                .ingests
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for ingest in ingests.iter_mut() {
                ingest.stream_key = self.concurrent_stream_key.clone();
            }
            drop(ingests);
            Ok(snapshot)
        })
    }

    fn list_ingests_for_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            Ok(self
                .snapshot()
                .into_iter()
                .filter(|ingest| ingest.stream_key == stream_key)
                .collect())
        })
    }
}

impl IngestWriter for ConcurrentWriteIngestStore {
    fn create_ingest<'a>(
        &'a self,
        _id: &'a str,
        _filename: &'a str,
        _stream_key: &'a str,
        _loop_flag: bool,
        _start_time: &'a str,
        _live_optimized: bool,
        _target_gop_seconds: u32,
    ) -> IngestWriteFuture<'a> {
        Box::pin(async move { Err(IngestWriteError::new("not implemented")) })
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
            let mut ingests = self
                .ingests
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(ingest) = ingests.iter_mut().find(|ingest| ingest.id == id) else {
                return Ok(None);
            };
            ingest.filename = filename.to_string();
            ingest.stream_key = stream_key.to_string();
            ingest.loop_flag = loop_flag;
            ingest.start_time = start_time.to_string();
            ingest.live_optimized = live_optimized;
            ingest.target_gop_seconds = target_gop_seconds;
            Ok(Some(ingest.clone()))
        })
    }

    fn update_ingest_filename<'a>(
        &'a self,
        id: &'a str,
        filename: &'a str,
    ) -> IngestUpdateFuture<'a> {
        Box::pin(async move {
            let mut ingests = self
                .ingests
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(ingest) = ingests.iter_mut().find(|ingest| ingest.id == id) else {
                return Ok(None);
            };
            ingest.filename = filename.to_string();
            Ok(Some(ingest.clone()))
        })
    }

    fn delete_ingest<'a>(&'a self, _id: &'a str) -> IngestDeleteFuture<'a> {
        Box::pin(async move { Ok(false) })
    }
}

#[tokio::test]
async fn rename_media_file_does_not_revert_concurrent_ingest_field_changes() {
    // Regression test: rename_media_file fetches an Ingest snapshot via
    // list_for_filename, then updates the renamed ingest from that
    // snapshot. If that update wrote every field back from the stale
    // snapshot (as it did before this fix, via the shared full-row
    // update_ingest), a concurrent stream-key rotation landing in the
    // window between the snapshot and the write would be silently
    // reverted -- reviving a possibly-leaked stream key.
    let old_name = "source.ts";
    let new_name = "renamed.ts";
    let ingest = Ingest {
        id: "ing-1".to_string(),
        filename: old_name.to_string(),
        stream_key: "sk-original".to_string(),
        loop_flag: true,
        start_time: "00:00:01".to_string(),
        live_optimized: true,
        target_gop_seconds: 2,
    };
    let ingest_store = Arc::new(ConcurrentWriteIngestStore::new(ingest, "sk-rotated"));
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    let service = MediaLibraryService::with_stores(
        Arc::new(SqliteMetaStore::new(pool.clone())),
        Arc::new(SqliteMetaStore::new(pool.clone())),
        Arc::new(SqliteRecordingStore::new(pool.clone())),
        sqlite_pipeline_service(&pool),
        IngestService::with_ports(ingest_store.clone(), ingest_store.clone()),
    );
    let temp_dir = tempfile_dir("media-rename-concurrent-write");
    let source = temp_dir.join(old_name);
    let destination = temp_dir.join(new_name);
    std::fs::write(&source, b"source").unwrap();

    let updated = service
        .rename_media_file(
            old_name,
            new_name,
            &std::fs::canonicalize(&source).unwrap(),
            &destination,
        )
        .await
        .unwrap();

    assert_eq!(updated, 1);
    let after = ingest_store.snapshot();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].filename, new_name);
    assert_eq!(after[0].stream_key, "sk-rotated");
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn recording_start_does_not_touch_runtime_when_persistence_fails() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    let service = MediaLibraryService::with_stores(
        Arc::new(SqliteMetaStore::new(pool.clone())),
        Arc::new(FailingMetaWriter),
        Arc::new(SqliteRecordingStore::new(pool.clone())),
        sqlite_pipeline_service(&pool),
        sqlite_ingest_service(&pool),
    );
    let engine = Arc::new(MediaEngine::new());
    let _registration = engine
        .try_register_ingest("pipe-recording", "stream-key", "rtmp")
        .await
        .unwrap();
    let temp_dir = tempfile_dir("recording-start-persist-fail");

    let err = service
        .recording_start(
            &engine,
            "pipe-recording",
            "Pipeline".to_string(),
            None,
            temp_dir.to_str().unwrap(),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, ServiceError::Internal(_)));
    assert!(!engine.is_recording_active("pipe-recording").await);
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn recording_stop_does_not_touch_runtime_when_persistence_fails() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    let service = MediaLibraryService::with_stores(
        Arc::new(SqliteMetaStore::new(pool.clone())),
        Arc::new(FailingMetaWriter),
        Arc::new(SqliteRecordingStore::new(pool.clone())),
        sqlite_pipeline_service(&pool),
        sqlite_ingest_service(&pool),
    );
    let engine = Arc::new(MediaEngine::new());
    let _token = engine.register_recording("pipe-recording").await;

    let err = service
        .recording_stop(&engine, "pipe-recording")
        .await
        .unwrap_err();

    assert!(matches!(err, ServiceError::Internal(_)));
    assert!(engine.is_recording_active("pipe-recording").await);
    engine.unregister_recording("pipe-recording").await;
}

#[tokio::test]
async fn analyze_media_file_surfaces_worker_analysis_errors() {
    let service = service_with_pipeline().await;
    let missing = PathBuf::from("/nonexistent/restream-media-analysis-missing.ts");

    let error = service.analyze_media_file(missing).await.unwrap_err();

    assert!(error.contains("Failed to open media file"));
}

#[tokio::test]
async fn delete_media_file_removes_recording_companions() {
    let service = service_with_pipeline().await;
    let temp_dir = tempfile_dir("media-delete-exec");
    let source = temp_dir.join("recording_20260709T010203_demo.ts");
    let converted = temp_dir.join("recording_20260709T010203_demo.mp4");
    let state = temp_dir.join("recording_20260709T010203_demo.ts.conversion.json");
    std::fs::write(&source, b"source").unwrap();
    std::fs::write(&converted, b"converted").unwrap();
    std::fs::write(&state, b"state").unwrap();

    service
        .delete_media_file(
            "recording_20260709T010203_demo.ts",
            &std::fs::canonicalize(&source).unwrap(),
        )
        .await
        .unwrap();

    assert!(!source.exists());
    assert!(!converted.exists());
    assert!(!state.exists());
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn delete_media_file_rejects_configured_ingests() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    crate::db::create_ingest(
        &pool,
        "ing-1",
        "clip.mp4",
        "stream-key",
        false,
        "",
        false,
        crate::application::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
    )
    .await
    .unwrap();
    let service = sqlite_media_library_service(&pool);
    let temp_dir = tempfile_dir("media-delete-ingest");
    let file = temp_dir.join("clip.mp4");
    std::fs::write(&file, b"source").unwrap();

    let err = service
        .delete_media_file("clip.mp4", &std::fs::canonicalize(&file).unwrap())
        .await
        .unwrap_err();

    assert_eq!(err, MediaDeleteError::HasConfiguredIngests);
    assert!(file.exists());
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn delete_media_file_preserves_file_when_ingest_lookup_fails() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    let service = sqlite_media_library_service(&pool);
    let temp_dir = tempfile_dir("media-delete-lookup-failure");
    let file = temp_dir.join("clip.mp4");
    std::fs::write(&file, b"source").unwrap();
    let canonical = std::fs::canonicalize(&file).unwrap();
    pool.close().await;

    let err = service
        .delete_media_file("clip.mp4", &canonical)
        .await
        .unwrap_err();

    assert!(matches!(err, MediaDeleteError::Dependency(_)));
    assert!(file.exists());
    let _ = std::fs::remove_dir_all(temp_dir);
}

fn tempfile_dir(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "restream-{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
