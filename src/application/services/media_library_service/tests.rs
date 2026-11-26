use super::*;
use crate::infrastructure::bootstrap::test_support::memory_pool;
use crate::infrastructure::service_wiring::ControlPlane;
use restream_domain::domain::ids::RecordingId;

fn sqlite_pipeline_service(pool: &sqlx::SqlitePool) -> PipelineService {
    ControlPlane::on_pool(pool).services.pipeline_service
}

fn sqlite_ingest_service(pool: &sqlx::SqlitePool) -> IngestService {
    ControlPlane::on_pool(pool).services.ingest_service
}

fn sqlite_media_library_service(pool: &sqlx::SqlitePool) -> MediaLibraryService {
    ControlPlane::on_pool(pool).services.media_library_service
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

/// An in-memory database whose meta writes fail, as an unavailable store's
/// would.
async fn failing_meta_writes_pool() -> sqlx::SqlitePool {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    for trigger in [
        "CREATE TRIGGER fail_meta_insert BEFORE INSERT ON meta \
         BEGIN SELECT RAISE(ABORT, 'injected meta write failure'); END",
        "CREATE TRIGGER fail_meta_update BEFORE UPDATE ON meta \
         BEGIN SELECT RAISE(ABORT, 'injected meta write failure'); END",
    ] {
        sqlx::query(trigger).execute(&pool).await.unwrap();
    }
    pool
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
        restream_domain::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
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
        serde_json::to_vec(&restream_engine::recording::RecordingConversionState {
            status: restream_engine::recording::RecordingConversionStatus::Ready,
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
async fn list_media_files_reads_conversion_state_without_a_converted_mp4() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    let service = sqlite_media_library_service(&pool);
    let temp_dir = tempfile_dir("media-list-conversion-state");
    let source = temp_dir.join("recording_20260709T010203_demo.ts");
    let state = temp_dir.join("recording_20260709T010203_demo.ts.conversion.json");
    std::fs::write(&source, b"source").unwrap();
    std::fs::write(
        &state,
        serde_json::to_vec(&restream_engine::recording::RecordingConversionState {
            status: restream_engine::recording::RecordingConversionStatus::Failed,
            updated_at: "2026-07-09T01:02:03Z".to_string(),
            error: Some("ffmpeg exited 1".to_string()),
        })
        .unwrap(),
    )
    .unwrap();

    let files = service.list_media_files(temp_dir.to_str().unwrap()).await;

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].conversion_status.as_deref(), Some("failed"));
    assert_eq!(
        files[0].conversion_error.as_deref(),
        Some("ffmpeg exited 1")
    );
    assert_eq!(
        files[0].conversion_updated_at.as_deref(),
        Some("2026-07-09T01:02:03Z")
    );
    assert!(files[0].converted_name.is_none());
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
        restream_domain::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
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
    let pool = memory_pool().await;
    let first = crate::db::create_ingest(
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
    let second = crate::db::create_ingest(
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
    // Regardless of row order, fail only after one ingest has been renamed.
    sqlx::query(
        "CREATE TRIGGER fail_second_rename BEFORE UPDATE OF filename ON ingests
         WHEN NEW.filename = 'renamed.ts' AND EXISTS (
             SELECT 1 FROM ingests WHERE filename = NEW.filename
         )
         BEGIN SELECT RAISE(ABORT, 'injected second rename failure'); END",
    )
    .execute(&pool)
    .await
    .unwrap();
    let service = MediaLibraryService::new(
        crate::db::DeadlineDb::api(pool.clone()),
        sqlite_pipeline_service(&pool),
        sqlite_ingest_service(&pool),
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

    assert!(matches!(
        err,
        MediaRenameError::IngestUpdate(message) if message.contains("injected second rename failure")
    ));
    assert!(source.exists());
    assert!(!destination.exists());
    let restored = crate::db::list_ingests(&pool).await.unwrap();
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

#[tokio::test]
async fn rename_media_file_does_not_revert_concurrent_ingest_field_changes() {
    // The first rename rotates the key of the other ingest, which the
    // service has already read but not yet updated. A full-row update
    // from that stale snapshot would restore the old key.
    let old_name = "source.ts";
    let new_name = "renamed.ts";
    let pool = memory_pool().await;
    for (id, key) in [("ing-1", "sk-one"), ("ing-2", "sk-two")] {
        crate::db::create_ingest(&pool, id, old_name, key, true, "00:00:01", true, 2)
            .await
            .unwrap();
    }
    sqlx::query(
        "CREATE TRIGGER rotate_other_ingest AFTER UPDATE OF filename ON ingests
         WHEN NEW.filename = 'renamed.ts'
         BEGIN
             UPDATE ingests SET stream_key = stream_key || '-rotated'
             WHERE filename = OLD.filename AND id != NEW.id;
         END",
    )
    .execute(&pool)
    .await
    .unwrap();
    let service = MediaLibraryService::new(
        crate::db::DeadlineDb::api(pool.clone()),
        sqlite_pipeline_service(&pool),
        sqlite_ingest_service(&pool),
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

    assert_eq!(updated, 2);
    let after = crate::db::list_ingests(&pool).await.unwrap();
    assert_eq!(after.len(), 2);
    assert!(after.iter().all(|ingest| ingest.filename == new_name));
    assert_eq!(
        after
            .iter()
            .filter(|ingest| ingest.stream_key.ends_with("-rotated"))
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn recording_start_does_not_touch_runtime_when_persistence_fails() {
    let pool = failing_meta_writes_pool().await;
    let service = MediaLibraryService::new(
        crate::db::DeadlineDb::api(pool.clone()),
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
    let pool = failing_meta_writes_pool().await;
    let service = MediaLibraryService::new(
        crate::db::DeadlineDb::api(pool.clone()),
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
        restream_domain::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
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

/// A media row's `modifiedAt` is the file's mtime in milliseconds, as RFC
/// 3339 with whole digit groups and a numeric UTC offset.
#[test]
fn entry_modified_is_millisecond_rfc3339_with_a_numeric_offset() {
    let temp_dir = tempfile_dir("media-modified");
    let path = temp_dir.join("clip.ts");
    let file = std::fs::File::create(&path).unwrap();
    for (mtime_ns, expected) in [
        (123_456_789, "2026-01-01T08:30:45.123+00:00"),
        (0, "2026-01-01T08:30:45+00:00"),
    ] {
        file.set_modified(
            std::time::UNIX_EPOCH + std::time::Duration::new(1_767_256_245, mtime_ns),
        )
        .unwrap();
        let (modified_at, modified_ms) = entry_modified(&std::fs::metadata(&path).unwrap());
        assert_eq!(modified_at, expected);
        assert_eq!(
            modified_ms,
            1_767_256_245_000 + i64::from(mtime_ns / 1_000_000)
        );
    }
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
