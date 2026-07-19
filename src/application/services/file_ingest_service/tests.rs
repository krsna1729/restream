use super::*;
use crate::application::ports::{
    IngestCatalogFuture, IngestDeleteFuture, IngestLookupError, IngestLookupFuture,
    IngestUpdateFuture, IngestWriteError, IngestWriteFuture, PipelineCreateFuture,
    PipelineDeleteFuture, PipelineIngestHostFuture, PipelineListFuture, PipelineLookupFuture,
    PipelineStoreError, PipelineUpdateFuture,
};
use crate::infrastructure::service_wiring::SqliteServiceFactory;
use sqlx::SqlitePool;

fn ingest_with(live_optimized: bool) -> Ingest {
    Ingest {
        id: "ing-1".to_string(),
        filename: "clip.mp4".to_string(),
        stream_key: "stream-key".to_string(),
        loop_flag: true,
        start_time: "00:00:05".to_string(),
        live_optimized,
        target_gop_seconds: 4,
    }
}

fn service(pool: SqlitePool) -> FileIngestService {
    let factory = SqliteServiceFactory::new(&pool);
    factory.file_ingest_service(factory.pipeline_service())
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
        crate::application::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
    )
    .await
    .unwrap();
}

struct PersistFailingLookup {
    ingest: Ingest,
}

impl IngestLookup for PersistFailingLookup {
    fn get_ingest<'a>(&'a self, _id: &'a str) -> IngestLookupFuture<'a> {
        Box::pin(async move { Ok(Some(self.ingest.clone())) })
    }

    fn get_ingest_by_stream_key<'a>(&'a self, _stream_key: &'a str) -> IngestLookupFuture<'a> {
        Box::pin(async move { Err(IngestLookupError::new("lookup failed during persist")) })
    }

    fn list_ingests<'a>(&'a self) -> IngestCatalogFuture<'a> {
        Box::pin(async move { Ok(vec![self.ingest.clone()]) })
    }

    fn list_ingests_for_filename<'a>(&'a self, _filename: &'a str) -> IngestCatalogFuture<'a> {
        Box::pin(async move { Ok(vec![self.ingest.clone()]) })
    }

    fn list_ingests_for_stream_key<'a>(&'a self, _stream_key: &'a str) -> IngestCatalogFuture<'a> {
        Box::pin(async move { Ok(vec![self.ingest.clone()]) })
    }
}

struct NoopIngestWriter;

impl IngestWriter for NoopIngestWriter {
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
        Box::pin(async move { Err(IngestWriteError::new("unexpected create")) })
    }

    fn update_ingest<'a>(
        &'a self,
        _id: &'a str,
        _filename: &'a str,
        _stream_key: &'a str,
        _loop_flag: bool,
        _start_time: &'a str,
        _live_optimized: bool,
        _target_gop_seconds: u32,
    ) -> IngestUpdateFuture<'a> {
        Box::pin(async move { Err(IngestWriteError::new("unexpected update")) })
    }

    fn update_ingest_filename<'a>(
        &'a self,
        _id: &'a str,
        _filename: &'a str,
    ) -> IngestUpdateFuture<'a> {
        Box::pin(async move { Err(IngestWriteError::new("unexpected update")) })
    }

    fn delete_ingest<'a>(&'a self, _id: &'a str) -> IngestDeleteFuture<'a> {
        Box::pin(async move { Ok(false) })
    }
}

struct StaticPipelineStore {
    pipeline: Pipeline,
}

impl PipelineStore for StaticPipelineStore {
    fn get_pipeline<'a>(&'a self, id: &'a str) -> PipelineLookupFuture<'a> {
        Box::pin(async move { Ok((self.pipeline.id == id).then(|| self.pipeline.clone())) })
    }

    fn get_pipeline_by_stream_key<'a>(&'a self, stream_key: &'a str) -> PipelineLookupFuture<'a> {
        Box::pin(async move {
            Ok((self.pipeline.stream_key == stream_key).then(|| self.pipeline.clone()))
        })
    }

    fn list_pipelines<'a>(&'a self) -> PipelineListFuture<'a> {
        Box::pin(async move { Ok(vec![self.pipeline.clone()]) })
    }

    fn create_pipeline<'a>(
        &'a self,
        _id: &'a str,
        _name: &'a str,
        _stream_key: &'a str,
        _input_source: Option<&'a str>,
        _srt_ingest_policy: Option<&'a str>,
    ) -> PipelineCreateFuture<'a> {
        Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
    }

    fn update_pipeline<'a>(
        &'a self,
        _id: &'a str,
        _name: &'a str,
        _stream_key: &'a str,
        _input_source: Option<&'a str>,
        _srt_ingest_policy: Option<&'a str>,
    ) -> PipelineUpdateFuture<'a> {
        Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
    }

    fn delete_pipeline<'a>(&'a self, _id: &'a str) -> PipelineDeleteFuture<'a> {
        Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
    }

    fn get_ingest_host<'a>(&'a self) -> PipelineIngestHostFuture<'a> {
        Box::pin(async move { Ok(None) })
    }

    fn update_pipeline_input_source<'a>(
        &'a self,
        pipeline: &'a Pipeline,
        input_source: Option<&'a str>,
    ) -> PipelineUpdateFuture<'a> {
        Box::pin(async move {
            let mut updated = pipeline.clone();
            updated.input_source = input_source.map(ToOwned::to_owned);
            Ok(Some(updated))
        })
    }
}

impl PipelineInputLookup for StaticPipelineStore {
    fn get_by_stream_key<'a>(
        &'a self,
        stream_key: &'a str,
    ) -> crate::application::ingest::PipelineInputLookupFuture<'a> {
        Box::pin(async move {
            Ok((self.pipeline.stream_key == stream_key).then(|| {
                crate::domain::pipeline_input::PipelineInput {
                    id: "input-primary".to_string(),
                    pipeline_id: self.pipeline.id.clone(),
                    label: "Primary".to_string(),
                    stream_key: stream_key.to_string(),
                    role: crate::domain::pipeline_input::PipelineInputRole::Primary,
                    enabled: true,
                    selected: true,
                }
            }))
        })
    }
}

#[tokio::test]
async fn apply_file_ingest_payload_surfaces_persist_failure() {
    let pipeline = Pipeline {
        id: "pipe-1".to_string(),
        name: "Pipeline".to_string(),
        stream_key: "stream-key".to_string(),
        input_source: None,
        srt_ingest_policy: None,
    };
    let ingest = ingest_with(false);
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    let pipeline_store = Arc::new(StaticPipelineStore {
        pipeline: pipeline.clone(),
    });
    let service = FileIngestService::with_ports(
        Arc::new(PersistFailingLookup { ingest }),
        Arc::new(NoopIngestWriter),
        pipeline_store.clone(),
        pipeline_store,
        SqliteServiceFactory::new(&pool).pipeline_service(),
    );
    let engine = Arc::new(MediaEngine::new());

    let err = service
        .apply_file_ingest_payload(
            &engine,
            &pipeline,
            None,
            Some(Some(FileIngestConfigInput {
                filename: "replacement.mp4".to_string(),
                loop_flag: false,
                start_time: String::new(),
                live_optimized: false,
                target_gop_seconds: 2,
            })),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        ServiceError::Internal(message) if message == "persist pipeline file ingest"
    ));
}

#[tokio::test]
async fn apply_file_ingest_payload_preserves_runtime_when_persist_fails() {
    let pipeline = Pipeline {
        id: "pipe-1".to_string(),
        name: "Pipeline".to_string(),
        stream_key: "stream-key".to_string(),
        input_source: None,
        srt_ingest_policy: None,
    };
    let ingest = ingest_with(false);
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    let pipeline_store = Arc::new(StaticPipelineStore {
        pipeline: pipeline.clone(),
    });
    let service = FileIngestService::with_ports(
        Arc::new(PersistFailingLookup {
            ingest: ingest.clone(),
        }),
        Arc::new(NoopIngestWriter),
        pipeline_store.clone(),
        pipeline_store,
        SqliteServiceFactory::new(&pool).pipeline_service(),
    );
    let engine = Arc::new(MediaEngine::new());
    let _registration = engine
        .try_register_ingest_attempt(&pipeline.id, &pipeline.stream_key, "file")
        .await
        .expect("pipeline should register");
    engine.mark_file_ingest_running(&ingest.id).await;

    let err = service
        .apply_file_ingest_payload(
            &engine,
            &pipeline,
            None,
            Some(Some(FileIngestConfigInput {
                filename: "replacement.mp4".to_string(),
                loop_flag: false,
                start_time: String::new(),
                live_optimized: false,
                target_gop_seconds: 2,
            })),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        ServiceError::Internal(message) if message == "persist pipeline file ingest"
    ));
    assert!(engine.has_active_ingest(&pipeline.id).await);
    assert!(engine.is_file_ingest_running(&ingest.id).await);
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
        crate::application::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
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
