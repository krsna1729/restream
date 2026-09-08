use super::*;
use crate::application::ports::{MetaLookupError, MetaStoreWriter, MetaWriteFuture};
use crate::domain::ids::RecordingId;
use crate::infrastructure::service_wiring::SqliteServiceFactory;
use crate::infrastructure::sqlite_ports::{SqliteMetaStore, SqliteRecordingStore};

fn sqlite_pipeline_service(pool: &sqlx::SqlitePool) -> PipelineService {
    SqliteServiceFactory::new(pool).pipeline_service()
}

fn sqlite_media_library_service(pool: &sqlx::SqlitePool) -> MediaLibraryService {
    let factory = SqliteServiceFactory::new(pool);
    factory.media_library_service(factory.pipeline_service())
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

struct FailingMetaWriter;

impl MetaStoreWriter for FailingMetaWriter {
    fn set_meta<'a>(&'a self, _key: &'a str, _value: &'a str) -> MetaWriteFuture<'a> {
        Box::pin(async move { Err(MetaLookupError::new("injected meta write failure")) })
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
    // After the first ingest filename update succeeds, a SQLite trigger aborts
    // the second rename so rename_media_file must roll back the first ingest
    // write and the filesystem rename.
    let old_name = "source.ts";
    let new_name = "renamed.ts";
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    crate::db::create_ingest(
        &pool,
        "ing-1",
        old_name,
        "stream-key-1",
        true,
        "00:00:01",
        true,
        2,
    )
    .await
    .unwrap();
    crate::db::create_ingest(
        &pool,
        "ing-2",
        old_name,
        "stream-key-2",
        false,
        "",
        false,
        4,
    )
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_second_ingest_rename
         AFTER UPDATE OF filename ON ingests
         WHEN NEW.filename = 'renamed.ts'
           AND (SELECT COUNT(*) FROM ingests WHERE filename = 'renamed.ts') >= 2
         BEGIN
           SELECT RAISE(ABORT, 'injected second rename failure');
         END;",
    )
    .execute(&pool)
    .await
    .unwrap();

    let service = MediaLibraryService::with_stores(
        Arc::new(SqliteMetaStore::new(pool.clone())),
        Arc::new(SqliteMetaStore::new(pool.clone())),
        Arc::new(SqliteRecordingStore::new(pool.clone())),
        sqlite_pipeline_service(&pool),
        pool.clone(),
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
    let restored = crate::db::list_ingests(&pool).await.unwrap();
    assert_eq!(restored.len(), 2);
    for ingest in &restored {
        assert_eq!(ingest.filename, old_name);
    }
    let first = restored.iter().find(|row| row.id == "ing-1").unwrap();
    assert_eq!(first.stream_key, "stream-key-1");
    assert!(first.loop_flag);
    assert_eq!(first.start_time, "00:00:01");
    assert!(first.live_optimized);
    assert_eq!(first.target_gop_seconds, 2);
    let second = restored.iter().find(|row| row.id == "ing-2").unwrap();
    assert_eq!(second.stream_key, "stream-key-2");
    assert!(!second.loop_flag);
    assert!(second.start_time.is_empty());
    assert!(!second.live_optimized);
    assert_eq!(second.target_gop_seconds, 4);
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn rename_media_file_does_not_revert_concurrent_ingest_field_changes() {
    // Regression: rename must update only the filename column. A concurrent
    // stream-key rotation that lands before the rename write must survive.
    let old_name = "source.ts";
    let new_name = "renamed.ts";
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    crate::db::create_ingest(
        &pool,
        "ing-1",
        old_name,
        "sk-original",
        true,
        "00:00:01",
        true,
        2,
    )
    .await
    .unwrap();
    crate::db::update_ingest(
        &pool,
        "ing-1",
        old_name,
        "sk-rotated",
        true,
        "00:00:01",
        true,
        2,
    )
    .await
    .unwrap();

    let service = MediaLibraryService::with_stores(
        Arc::new(SqliteMetaStore::new(pool.clone())),
        Arc::new(SqliteMetaStore::new(pool.clone())),
        Arc::new(SqliteRecordingStore::new(pool.clone())),
        sqlite_pipeline_service(&pool),
        pool.clone(),
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
    let after = crate::db::get_ingest(&pool, "ing-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.filename, new_name);
    assert_eq!(after.stream_key, "sk-rotated");
    assert!(after.loop_flag);
    assert_eq!(after.start_time, "00:00:01");
    assert!(after.live_optimized);
    assert_eq!(after.target_gop_seconds, 2);
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
        pool.clone(),
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
        pool.clone(),
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
