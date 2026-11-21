use super::*;
use crate::application::catalog_auth::CatalogAuth;
use sqlx::SqlitePool;

fn service(pool: SqlitePool) -> FileIngestService {
    let db = DeadlineDb::api(pool);
    FileIngestService::new(
        db.clone(),
        PipelineService::new(db.clone(), CatalogAuth::new(db)),
    )
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "restream-file-ingest-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn setup_ingest(pool: &SqlitePool, ingest_id: &str) {
    crate::db::setup_database_schema(pool).await.unwrap();
    crate::db::create_pipeline(pool, "pipe-1", "Pipeline", "stream-key", None, None)
        .await
        .unwrap();
    crate::db::create_ingest(
        pool,
        ingest_id,
        "clip.mp4",
        "stream-key",
        false,
        "",
        false,
        restream_domain::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
    )
    .await
    .unwrap();
}

async fn fail_persist(pool: &SqlitePool, trigger_sql: &'static str) {
    sqlx::query(trigger_sql).execute(pool).await.unwrap();
}

fn replacement_payload() -> Option<Option<FileIngestConfigInput>> {
    Some(Some(FileIngestConfigInput {
        filename: "replacement.mp4".to_string(),
        loop_flag: false,
        start_time: String::new(),
        live_optimized: false,
        target_gop_seconds: 2,
        playlist: Vec::new(),
    }))
}

#[tokio::test]
async fn apply_file_ingest_payload_surfaces_persist_failure() {
    let pool = crate::infrastructure::bootstrap::test_support::memory_pool().await;
    let pipeline = db::create_pipeline(&pool, "pipe-1", "Pipeline", "stream-key", None, None)
        .await
        .unwrap()
        .into();
    fail_persist(
        &pool,
        "CREATE TRIGGER fail_ingest_insert BEFORE INSERT ON ingests
         BEGIN SELECT RAISE(ABORT, 'injected ingest insert failure'); END",
    )
    .await;
    let service = service(pool.clone());
    let engine = Arc::new(MediaEngine::new());

    let err = service
        .apply_file_ingest_payload(&engine, &pipeline, None, replacement_payload())
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        ServiceError::Internal(message) if message == "persist pipeline file ingest"
    ));
    assert!(db::list_ingests(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn apply_file_ingest_payload_preserves_runtime_when_persist_fails() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    setup_ingest(&pool, "ing-1").await;
    fail_persist(
        &pool,
        "CREATE TRIGGER fail_ingest_update BEFORE UPDATE ON ingests
         BEGIN SELECT RAISE(ABORT, 'injected ingest update failure'); END",
    )
    .await;
    let service = service(pool.clone());
    let pipeline = service.get_pipeline("pipe-1").await.unwrap();
    let engine = Arc::new(MediaEngine::new());
    let _registration = engine
        .try_register_ingest(&pipeline.id, &pipeline.stream_key, "file")
        .await
        .expect("pipeline should register");
    engine.mark_file_ingest_running("ing-1").await;

    let err = service
        .apply_file_ingest_payload(&engine, &pipeline, None, replacement_payload())
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        ServiceError::Internal(message) if message == "persist pipeline file ingest"
    ));
    assert!(engine.has_active_ingest(&pipeline.id).await);
    assert!(engine.is_file_ingest_running("ing-1").await);
    assert_eq!(
        db::get_ingest(&pool, "ing-1")
            .await
            .unwrap()
            .unwrap()
            .filename,
        "clip.mp4"
    );
}

#[test]
fn resolve_media_file_path_accepts_existing_relative_file() {
    let media_dir = temp_dir("resolve-ok");
    let file = media_dir.join("clip.mp4");
    std::fs::write(&file, b"clip").unwrap();

    let resolved = FileIngestService::resolve_media_file_path(&media_dir, "clip.mp4").unwrap();

    assert_eq!(resolved, file.canonicalize().unwrap());
    let _ = std::fs::remove_dir_all(media_dir);
}

#[test]
fn resolve_media_file_path_rejects_parent_traversal() {
    let media_dir = temp_dir("resolve-parent");
    let outside_dir = temp_dir("resolve-parent-outside");
    let outside = outside_dir.join("clip.mp4");
    std::fs::write(&outside, b"clip").unwrap();

    let err = FileIngestService::resolve_media_file_path(
        &media_dir,
        "../resolve-parent-outside/clip.mp4",
    )
    .unwrap_err();

    assert_eq!(err, FileIngestStartError::InvalidMediaPath);
    let _ = std::fs::remove_dir_all(media_dir);
    let _ = std::fs::remove_dir_all(outside_dir);
}

#[test]
fn resolve_media_file_path_rejects_absolute_paths() {
    let media_dir = temp_dir("resolve-absolute");
    let outside_dir = temp_dir("resolve-absolute-outside");
    let outside = outside_dir.join("clip.mp4");
    std::fs::write(&outside, b"clip").unwrap();

    let err = FileIngestService::resolve_media_file_path(
        &media_dir,
        outside.to_str().expect("utf-8 temp path"),
    )
    .unwrap_err();

    assert_eq!(err, FileIngestStartError::InvalidMediaPath);
    let _ = std::fs::remove_dir_all(media_dir);
    let _ = std::fs::remove_dir_all(outside_dir);
}

#[cfg(unix)]
#[test]
fn resolve_media_file_path_rejects_symlink_escape() {
    let media_dir = temp_dir("resolve-symlink");
    let outside_dir = temp_dir("resolve-symlink-outside");
    let outside = outside_dir.join("clip.mp4");
    std::fs::write(&outside, b"clip").unwrap();
    std::os::unix::fs::symlink(&outside, media_dir.join("linked.mp4")).unwrap();

    let err = FileIngestService::resolve_media_file_path(&media_dir, "linked.mp4").unwrap_err();

    assert_eq!(err, FileIngestStartError::InvalidMediaPath);
    let _ = std::fs::remove_dir_all(media_dir);
    let _ = std::fs::remove_dir_all(outside_dir);
}

#[tokio::test]
async fn stop_ingest_with_runtime_cleanup_clears_running_state() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    setup_ingest(&pool, "ing-stop").await;
    let engine = Arc::new(MediaEngine::new());
    engine.mark_file_ingest_running("ing-stop").await;

    let ingest = service(pool)
        .stop_ingest_with_runtime_cleanup(&engine, "ing-stop")
        .await
        .unwrap();

    assert_eq!(ingest.id, "ing-stop");
    assert!(!engine.is_file_ingest_running("ing-stop").await);
}

#[tokio::test]
async fn delete_ingest_with_runtime_cleanup_deletes_and_clears_running_state() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    setup_ingest(&pool, "ing-delete").await;
    let engine = Arc::new(MediaEngine::new());
    engine.mark_file_ingest_running("ing-delete").await;
    let service = service(pool.clone());

    service
        .delete_ingest_with_runtime_cleanup(&engine, "ing-delete")
        .await
        .unwrap();

    assert!(!engine.is_file_ingest_running("ing-delete").await);
    assert!(
        crate::db::get_ingest(&pool, "ing-delete")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn start_ingest_returns_not_found_for_missing_ingest() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    let engine = Arc::new(MediaEngine::new());

    let err = service(pool)
        .start_ingest(engine, std::env::temp_dir().as_path(), "missing")
        .await
        .unwrap_err();

    assert_eq!(err, FileIngestStartError::NotFound);
}

#[tokio::test]
async fn start_ingest_requires_pipeline_for_stream_key() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    crate::db::create_ingest(
        &pool,
        "ing-orphan",
        "clip.mp4",
        "missing-stream-key",
        false,
        "",
        false,
        restream_domain::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
    )
    .await
    .unwrap();
    let engine = Arc::new(MediaEngine::new());

    let err = service(pool)
        .start_ingest(engine, std::env::temp_dir().as_path(), "ing-orphan")
        .await
        .unwrap_err();

    assert_eq!(err, FileIngestStartError::MissingPipelineForStreamKey);
}

#[tokio::test]
async fn start_ingest_rejects_already_running_ingest_before_file_check() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    setup_ingest(&pool, "ing-running").await;
    let engine = Arc::new(MediaEngine::new());
    engine.mark_file_ingest_running("ing-running").await;

    let err = service(pool)
        .start_ingest(engine, std::env::temp_dir().as_path(), "ing-running")
        .await
        .unwrap_err();

    assert_eq!(err, FileIngestStartError::AlreadyRunning);
}

#[tokio::test]
async fn start_ingest_rejects_missing_media_file() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    setup_ingest(&pool, "ing-missing-media").await;
    let engine = Arc::new(MediaEngine::new());

    let err = service(pool)
        .start_ingest(engine, std::env::temp_dir().as_path(), "ing-missing-media")
        .await
        .unwrap_err();

    assert_eq!(err, FileIngestStartError::MediaFileNotFound);
}
