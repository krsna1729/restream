use super::*;
use std::sync::Arc;

use crate::infrastructure::bootstrap::test_support::memory_pool;
use sqlx::SqlitePool;

async fn seed_ingest(pool: &SqlitePool, id: &str) -> Ingest {
    db::create_ingest(
        pool,
        id,
        "clip.mp4",
        "stream-key",
        true,
        "00:00:05",
        true,
        4,
    )
    .await
    .unwrap()
    .into()
}

async fn seed_pipeline(pool: &SqlitePool, input_source: Option<&str>) -> Pipeline {
    db::create_pipeline(
        pool,
        "pipeline-1",
        "Pipeline",
        "stream-key",
        input_source,
        None,
    )
    .await
    .unwrap()
    .into()
}

/// Simulate old catalog data with multiple file ingests for one stream key.
async fn allow_legacy_duplicates(pool: &SqlitePool) {
    sqlx::query("DROP INDEX idx_ingests_stream_key_unique")
        .execute(pool)
        .await
        .unwrap();
}

fn replacement_config() -> FileIngestConfig {
    FileIngestConfig {
        filename: "updated.mp4".to_string(),
        loop_flag: false,
        start_time: "00:00:10".to_string(),
        live_optimized: false,
        target_gop_seconds: 2,
        playlist: Vec::new(),
    }
}

#[tokio::test]
async fn resolve_file_ingest_context_returns_none_for_missing_ingest() {
    let db = DeadlineDb::api(memory_pool().await);
    let result = resolve_file_ingest_context(&db, "missing").await.unwrap();

    assert!(result.is_none());
}

#[tokio::test]
async fn resolve_file_ingest_context_surfaces_missing_pipeline() {
    let pool = memory_pool().await;
    seed_ingest(&pool, "ingest-1").await;
    let result = resolve_file_ingest_context(&DeadlineDb::api(pool), "ingest-1").await;

    assert!(matches!(
        result,
        Err(ResolveFileIngestError::MissingPipelineForStreamKey(stream_key))
            if stream_key == "stream-key"
    ));
}

#[tokio::test]
async fn resolve_file_ingest_context_returns_ingest_and_pipeline() {
    let pool = memory_pool().await;
    seed_pipeline(&pool, None).await;
    seed_ingest(&pool, "ingest-1").await;
    let result = resolve_file_ingest_context(&DeadlineDb::api(pool), "ingest-1")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(result.ingest.id, "ingest-1");
    assert_eq!(result.pipeline.id, "pipeline-1");
}

#[tokio::test]
async fn load_pipeline_file_ingest_state_returns_latest_ingest_and_running_flag() {
    let pool = memory_pool().await;
    let pipeline = seed_pipeline(&pool, None).await;
    let ingest = seed_ingest(&pool, "ingest-1").await;
    let engine = MediaEngine::new();
    engine.mark_file_ingest_running(&ingest.id).await;
    let state = load_pipeline_file_ingest_state(&DeadlineDb::api(pool), &engine, &pipeline)
        .await
        .unwrap();

    assert_eq!(state.ingest.unwrap().id, "ingest-1");
    assert!(state.running);
}

#[tokio::test]
async fn clear_stream_key_file_ingests_unregisters_pipeline_and_clears_running_state() {
    let pool = memory_pool().await;
    let pipeline = seed_pipeline(&pool, None).await;
    let ingest = seed_ingest(&pool, "ingest-1").await;
    let engine = Arc::new(MediaEngine::new());
    let _registration = engine
        .try_register_ingest(&pipeline.id, &pipeline.stream_key, "file")
        .await
        .expect("pipeline should register");
    engine.mark_file_ingest_running(&ingest.id).await;

    clear_stream_key_file_ingests(&DeadlineDb::api(pool), &engine, &pipeline.stream_key)
        .await
        .unwrap();

    assert!(!engine.has_active_ingest(&pipeline.id).await);
    assert!(!engine.is_file_ingest_running(&ingest.id).await);
}

#[tokio::test]
async fn persist_pipeline_file_ingest_updates_pipeline_and_deletes_stale_ingests() {
    let pool = memory_pool().await;
    let pipeline = seed_pipeline(&pool, None).await;
    allow_legacy_duplicates(&pool).await;
    seed_ingest(&pool, "ingest-stale").await;
    seed_ingest(&pool, "ingest-current").await;

    let saved = persist_pipeline_file_ingest(
        &DeadlineDb::api(pool.clone()),
        &pipeline,
        replacement_config(),
        || panic!("updating an existing ingest must not generate a new ID"),
    )
    .await
    .unwrap();

    assert_eq!(saved.id, "ingest-current");
    assert_eq!(saved.filename, "updated.mp4");
    let ingests = db::list_ingests_for_stream_key(&pool, &pipeline.stream_key)
        .await
        .unwrap();
    assert_eq!(ingests.len(), 1);
    assert_eq!(ingests[0].id, saved.id);
    assert_eq!(ingests[0].filename, saved.filename);
    assert_eq!(
        db::get_pipeline(&pool, &pipeline.id)
            .await
            .unwrap()
            .unwrap()
            .input_source
            .as_deref(),
        Some("file:updated.mp4")
    );
}

#[tokio::test]
async fn resolve_file_ingest_context_surfaces_ingest_lookup_error() {
    let pool = memory_pool().await;
    pool.close().await;
    let result = resolve_file_ingest_context(&DeadlineDb::api(pool), "ingest-1").await;

    assert!(matches!(
        result,
        Err(ResolveFileIngestError::IngestLookup(_))
    ));
}

#[tokio::test]
async fn persist_pipeline_file_ingest_creates_new_ingest_when_none_exists() {
    let pool = memory_pool().await;
    let pipeline = seed_pipeline(&pool, None).await;

    let saved = persist_pipeline_file_ingest(
        &DeadlineDb::api(pool.clone()),
        &pipeline,
        FileIngestConfig {
            filename: "new.mp4".to_string(),
            loop_flag: true,
            start_time: "00:00:00".to_string(),
            live_optimized: true,
            target_gop_seconds: 4,
            playlist: Vec::new(),
        },
        || "generated-id".to_string(),
    )
    .await
    .unwrap();

    assert_eq!(saved.id, "generated-id");
    assert_eq!(saved.filename, "new.mp4");
    let ingests = db::list_ingests(&pool).await.unwrap();
    assert_eq!(ingests.len(), 1);
    assert_eq!(ingests[0].id, saved.id);
    assert_eq!(ingests[0].filename, saved.filename);
}

#[tokio::test]
async fn persist_pipeline_file_ingest_surfaces_race_when_update_target_disappears() {
    let pool = memory_pool().await;
    let pipeline = seed_pipeline(&pool, None).await;
    seed_ingest(&pool, "ingest-current").await;
    // Remove the row after the lookup but before the UPDATE changes it.
    // IGNORE abandons the outer update without rolling back the deletion.
    sqlx::query(
        "CREATE TRIGGER delete_ingest_before_update BEFORE UPDATE ON ingests
         BEGIN
             DELETE FROM ingests WHERE id = OLD.id;
             SELECT RAISE(IGNORE);
         END",
    )
    .execute(&pool)
    .await
    .unwrap();

    let result = persist_pipeline_file_ingest(
        &DeadlineDb::api(pool.clone()),
        &pipeline,
        replacement_config(),
        || panic!("a vanished update target must not be recreated"),
    )
    .await;

    assert!(matches!(
        result,
        Err(PersistFileIngestError::IngestWrite(
            sqlx::Error::RowNotFound
        ))
    ));
    assert!(db::list_ingests(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn remove_pipeline_file_ingest_deletes_all_ingests_and_clears_input_source() {
    let pool = memory_pool().await;
    let pipeline = seed_pipeline(&pool, Some("file:clip.mp4")).await;
    allow_legacy_duplicates(&pool).await;
    seed_ingest(&pool, "ingest-1").await;
    seed_ingest(&pool, "ingest-2").await;

    remove_pipeline_file_ingest(&DeadlineDb::api(pool.clone()), &pipeline)
        .await
        .unwrap();

    assert!(db::list_ingests(&pool).await.unwrap().is_empty());
    assert!(
        db::get_pipeline(&pool, &pipeline.id)
            .await
            .unwrap()
            .unwrap()
            .input_source
            .is_none()
    );
}

#[tokio::test]
async fn remove_file_ingest_does_not_revert_concurrent_pipeline_rename_or_key_rotation() {
    let pool = memory_pool().await;
    let stale = seed_pipeline(&pool, Some("file:clip.mp4")).await;
    db::update_pipeline(
        &pool,
        &stale.id,
        "Renamed",
        "rotated",
        Some("file:clip.mp4"),
        None,
    )
    .await
    .unwrap();

    remove_pipeline_file_ingest(&DeadlineDb::api(pool.clone()), &stale)
        .await
        .unwrap();

    let after = db::get_pipeline(&pool, &stale.id).await.unwrap().unwrap();
    assert_eq!(after.name, "Renamed");
    assert_eq!(after.stream_key, "rotated");
    assert!(after.input_source.is_none());
}
