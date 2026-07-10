use std::sync::Arc;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sqlx::SqlitePool;

use crate::application::ports::{
    MetaStore, MetaStoreWriter, RecordingStore, SqliteMetaStore, SqliteRecordingStore,
};
use crate::application::recording::{
    load_recording_settings, recording_enabled_meta_key, spawn_recording_metadata_reporter,
    spawn_recording_task,
};
use crate::media::engine::MediaEngine;
use crate::media::recording::RecordingMetadataReporter;
use crate::types::{Ingest, Pipeline};

use super::error::{ApiError, ApiResult};
use super::ingest_service::IngestService;
use super::pipeline_service::PipelineService;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRecordingMetadata {
    pub recording_id: String,
    pub pipeline_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: String,
    pub codec_summary: Option<String>,
    pub error: Option<String>,
}

pub struct MediaLibraryService {
    meta_store: Arc<dyn MetaStore>,
    meta_writer: Arc<dyn MetaStoreWriter>,
    recording_store: Arc<dyn RecordingStore>,
    pipeline_service: PipelineService,
    ingest_service: IngestService,
    recording_metadata: Option<RecordingMetadataReporter>,
}

#[derive(Clone)]
struct MediaDirEntry {
    name: String,
    size: u64,
    modified_at: String,
    modified_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaLibraryFile {
    pub name: String,
    pub size: u64,
    pub modified_at: String,
    pub ingest_count: usize,
    pub kind: String,
    pub source_name: String,
    pub source_size: u64,
    pub converted_name: Option<String>,
    pub converted_size: Option<u64>,
    pub play_name: Option<String>,
    pub conversion_status: Option<String>,
    pub conversion_error: Option<String>,
    pub conversion_updated_at: Option<String>,
    pub recording_id: Option<String>,
    pub pipeline_id: Option<String>,
    pub recording_status: Option<String>,
    pub recording_started_at: Option<String>,
    pub recording_ended_at: Option<String>,
    pub recording_codec_summary: Option<String>,
    pub recording_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaRenamePlanError {
    ConvertedExists,
    ConversionStateExists,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaRenameError {
    ConvertedExists,
    ConversionStateExists,
    Io(String),
    IngestUpdate(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaDeleteError {
    HasConfiguredIngests,
    Dependency(String),
    NotFound,
    Io(String),
}

impl MediaLibraryService {
    pub fn new(
        db: SqlitePool,
        pipeline_service: PipelineService,
        ingest_service: IngestService,
    ) -> Self {
        let meta_store = Arc::new(SqliteMetaStore::new(db.clone()));
        let recording_metadata = spawn_recording_metadata_reporter(db.clone());
        Self {
            meta_store: meta_store.clone(),
            meta_writer: meta_store,
            recording_store: Arc::new(SqliteRecordingStore::new(db)),
            pipeline_service,
            ingest_service,
            recording_metadata: Some(recording_metadata),
        }
    }

    pub fn with_stores(
        meta_store: Arc<dyn MetaStore>,
        meta_writer: Arc<dyn MetaStoreWriter>,
        recording_store: Arc<dyn RecordingStore>,
        pipeline_service: PipelineService,
        ingest_service: IngestService,
    ) -> Self {
        Self {
            meta_store,
            meta_writer,
            recording_store,
            pipeline_service,
            ingest_service,
            recording_metadata: None,
        }
    }

    pub async fn get_pipeline(&self, id: &str) -> ApiResult<Pipeline> {
        self.pipeline_service.get_by_id(id).await
    }

    pub async fn list_media_files(&self, media_dir: &str) -> Vec<MediaLibraryFile> {
        let mut entries = HashMap::<String, MediaDirEntry>::new();
        if let Ok(mut media_dir_entries) = tokio::fs::read_dir(media_dir).await {
            while let Ok(Some(entry)) = media_dir_entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                if media_filename_is_supported(&name)
                    && let Ok(metadata) = entry.metadata().await
                {
                    let (modified_at, modified_ms) = entry_modified(&metadata);
                    entries.insert(
                        name.clone(),
                        MediaDirEntry {
                            name,
                            size: metadata.len(),
                            modified_at,
                            modified_ms,
                        },
                    );
                }
            }
        }

        let mut files = Vec::new();
        let mut consumed = HashSet::new();
        let mut names = entries.keys().cloned().collect::<Vec<_>>();
        names.sort();
        let recording_metadata = self
            .recording_metadata_by_filename(names.clone())
            .await
            .unwrap_or_default();

        for name in names {
            if !consumed.insert(name.clone()) {
                continue;
            }
            let Some(entry) = entries.get(&name).cloned() else {
                continue;
            };
            if name.ends_with(".mp4") {
                let companion_source_name = Path::new(&name)
                    .with_extension("ts")
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string());
                if let Some(companion_source_name) = companion_source_name
                    && crate::media::recording::is_recording_source_filename(&companion_source_name)
                    && entries.contains_key(&companion_source_name)
                {
                    continue;
                }
            }

            let ingests = self
                .ingest_service
                .list_for_filename(&name)
                .await
                .unwrap_or_default();
            let lower_name = name.to_ascii_lowercase();
            let recording_meta = recording_metadata.get(&name);
            let kind = if recording_meta.is_some() || lower_name.contains("recording") {
                "recording"
            } else {
                "source"
            };

            if crate::media::recording::is_recording_source_filename(&name) {
                let source_path = Path::new(media_dir).join(&name);
                let converted_name = crate::media::recording::build_mp4_path(&source_path)
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
                    .filter(|candidate| entries.contains_key(candidate));
                let converted_entry = converted_name
                    .as_ref()
                    .and_then(|candidate| entries.get(candidate).cloned());
                if let Some(converted_name) = &converted_name {
                    consumed.insert(converted_name.clone());
                }
                let conversion_state = crate::media::recording::load_conversion_state(&source_path);
                let conversion_status = if converted_entry.is_some() {
                    Some("ready".to_string())
                } else {
                    conversion_state.as_ref().map(|state| match state.status {
                        crate::media::recording::RecordingConversionStatus::Converting => {
                            "converting".to_string()
                        }
                        crate::media::recording::RecordingConversionStatus::Ready => {
                            "ready".to_string()
                        }
                        crate::media::recording::RecordingConversionStatus::Failed => {
                            "failed".to_string()
                        }
                    })
                };
                let conversion_error = conversion_state
                    .as_ref()
                    .and_then(|state| state.error.clone());
                let conversion_updated_at = conversion_state
                    .as_ref()
                    .map(|state| state.updated_at.clone());
                let converted_size = converted_entry.as_ref().map(|value| value.size);
                let total_size = entry.size + converted_size.unwrap_or(0);
                let modified_at = converted_entry
                    .as_ref()
                    .filter(|value| value.modified_ms > entry.modified_ms)
                    .map(|value| value.modified_at.clone())
                    .unwrap_or_else(|| entry.modified_at.clone());

                files.push(MediaLibraryFile {
                    name,
                    size: total_size,
                    modified_at,
                    ingest_count: ingests.len(),
                    kind: kind.to_string(),
                    source_name: entry.name,
                    source_size: entry.size,
                    converted_name: converted_entry.as_ref().map(|value| value.name.clone()),
                    converted_size,
                    play_name: converted_entry.as_ref().map(|value| value.name.clone()),
                    conversion_status,
                    conversion_error,
                    conversion_updated_at,
                    recording_id: recording_meta.map(|row| row.recording_id.clone()),
                    pipeline_id: recording_meta.map(|row| row.pipeline_id.clone()),
                    recording_status: recording_meta.map(|row| row.status.clone()),
                    recording_started_at: recording_meta.map(|row| row.started_at.clone()),
                    recording_ended_at: recording_meta.and_then(|row| row.ended_at.clone()),
                    recording_codec_summary: recording_meta
                        .and_then(|row| row.codec_summary.clone()),
                    recording_error: recording_meta.and_then(|row| row.error.clone()),
                });
                continue;
            }

            files.push(MediaLibraryFile {
                name,
                size: entry.size,
                modified_at: entry.modified_at,
                ingest_count: ingests.len(),
                kind: kind.to_string(),
                source_name: entry.name.clone(),
                source_size: entry.size,
                converted_name: None,
                converted_size: None,
                play_name: Some(entry.name),
                conversion_status: None,
                conversion_error: None,
                conversion_updated_at: None,
                recording_id: recording_meta.map(|row| row.recording_id.clone()),
                pipeline_id: recording_meta.map(|row| row.pipeline_id.clone()),
                recording_status: recording_meta.map(|row| row.status.clone()),
                recording_started_at: recording_meta.map(|row| row.started_at.clone()),
                recording_ended_at: recording_meta.and_then(|row| row.ended_at.clone()),
                recording_codec_summary: recording_meta.and_then(|row| row.codec_summary.clone()),
                recording_error: recording_meta.and_then(|row| row.error.clone()),
            });
        }

        files
    }

    pub async fn recording_start(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline_id: &str,
        pipeline_name: String,
        input_source: Option<String>,
        media_dir: &str,
    ) -> ApiResult<bool> {
        let meta_key = recording_enabled_meta_key(pipeline_id);
        self.meta_writer
            .set_meta(&meta_key, "1")
            .await
            .map_err(|e| ApiError::internal(format!("set recording enabled: {e}")))?;

        let has_ingest = engine.ingests.active.read().await.contains_key(pipeline_id);
        if has_ingest && !engine.is_recording_active(pipeline_id).await {
            let recording_settings = load_recording_settings(self.meta_store.as_ref()).await;
            spawn_recording_task(
                engine.clone(),
                pipeline_name,
                pipeline_id.to_string(),
                input_source,
                media_dir.to_string(),
                recording_settings,
                self.recording_metadata.clone(),
            )
            .await;
        }

        Ok(engine.is_recording_active(pipeline_id).await)
    }

    pub async fn recording_stop(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline_id: &str,
    ) -> ApiResult<()> {
        let meta_key = recording_enabled_meta_key(pipeline_id);
        self.meta_writer
            .set_meta(&meta_key, "0")
            .await
            .map_err(|e| ApiError::internal(format!("set recording disabled: {e}")))?;
        engine.unregister_recording(pipeline_id).await;
        Ok(())
    }

    pub async fn recording_metadata_by_filename(
        &self,
        filenames: impl IntoIterator<Item = String>,
    ) -> ApiResult<HashMap<String, MediaRecordingMetadata>> {
        let requested = filenames.into_iter().collect::<HashSet<_>>();
        if requested.is_empty() {
            return Ok(HashMap::new());
        }

        let rows = self
            .recording_store
            .list_recordings()
            .await
            .map_err(|e| ApiError::internal(format!("list recordings: {e}")))?;
        let mut metadata = HashMap::new();
        for row in rows {
            for path in [row.final_path.as_deref(), row.temp_path.as_deref()]
                .into_iter()
                .flatten()
            {
                let Some(name) = std::path::Path::new(path)
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
                else {
                    continue;
                };
                if requested.contains(&name) && !metadata.contains_key(&name) {
                    metadata.insert(
                        name,
                        MediaRecordingMetadata {
                            recording_id: row.recording_id.clone(),
                            pipeline_id: row.pipeline_id.clone(),
                            started_at: row.started_at.clone(),
                            ended_at: row.ended_at.clone(),
                            status: row.status.clone(),
                            codec_summary: row.codec_summary.clone(),
                            error: row.error.clone(),
                        },
                    );
                }
            }
        }
        Ok(metadata)
    }

    pub fn delete_paths_for_media(&self, filename: &str, canonical_path: &Path) -> Vec<PathBuf> {
        let mut delete_paths = vec![canonical_path.to_path_buf()];
        if crate::media::recording::is_recording_source_filename(filename) {
            let converted_path = crate::media::recording::build_mp4_path(canonical_path);
            if converted_path.exists() {
                delete_paths.push(converted_path);
            }
            let state_path = crate::media::recording::build_conversion_state_path(canonical_path);
            if state_path.exists() {
                delete_paths.push(state_path);
            }
        }
        delete_paths
    }

    pub async fn delete_media_file(
        &self,
        filename: &str,
        canonical_path: &Path,
    ) -> Result<(), MediaDeleteError> {
        let ingests = self
            .ingest_service
            .list_for_filename(filename)
            .await
            .map_err(|error| MediaDeleteError::Dependency(error.to_string()))?;
        if !ingests.is_empty() {
            return Err(MediaDeleteError::HasConfiguredIngests);
        }

        let delete_paths = self.delete_paths_for_media(filename, canonical_path);
        match tokio::fs::remove_file(canonical_path).await {
            Ok(_) => {
                for extra_path in delete_paths.into_iter().skip(1) {
                    let _ = tokio::fs::remove_file(extra_path).await;
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(MediaDeleteError::NotFound)
            }
            Err(error) => Err(MediaDeleteError::Io(error.to_string())),
        }
    }

    pub fn rename_pairs_for_media(
        &self,
        filename: &str,
        source_path: &Path,
        destination_path: &Path,
    ) -> Result<Vec<(PathBuf, PathBuf)>, MediaRenamePlanError> {
        let mut rename_pairs = vec![(source_path.to_path_buf(), destination_path.to_path_buf())];
        if crate::media::recording::is_recording_source_filename(filename) {
            let source_converted = crate::media::recording::build_mp4_path(source_path);
            let destination_converted = crate::media::recording::build_mp4_path(destination_path);
            if source_converted.exists() {
                if destination_converted.exists() {
                    return Err(MediaRenamePlanError::ConvertedExists);
                }
                rename_pairs.push((source_converted, destination_converted));
            }

            let source_state = crate::media::recording::build_conversion_state_path(source_path);
            let destination_state =
                crate::media::recording::build_conversion_state_path(destination_path);
            if source_state.exists() {
                if destination_state.exists() {
                    return Err(MediaRenamePlanError::ConversionStateExists);
                }
                rename_pairs.push((source_state, destination_state));
            }
        }
        Ok(rename_pairs)
    }

    pub async fn rename_media_file(
        &self,
        filename: &str,
        new_name: &str,
        source_path: &Path,
        destination_path: &Path,
    ) -> Result<usize, MediaRenameError> {
        let rename_pairs = self
            .rename_pairs_for_media(filename, source_path, destination_path)
            .map_err(|error| match error {
                MediaRenamePlanError::ConvertedExists => MediaRenameError::ConvertedExists,
                MediaRenamePlanError::ConversionStateExists => {
                    MediaRenameError::ConversionStateExists
                }
            })?;

        let mut completed = Vec::new();
        for (from, to) in &rename_pairs {
            if let Err(error) = tokio::fs::rename(from, to).await {
                rollback_renames(completed).await;
                return Err(MediaRenameError::Io(error.to_string()));
            }
            completed.push((from.clone(), to.clone()));
        }

        let ingests = match self.ingest_service.list_for_filename(filename).await {
            Ok(ingests) => ingests,
            Err(error) => {
                rollback_renames(completed).await;
                return Err(MediaRenameError::IngestUpdate(error.to_string()));
            }
        };
        let mut updated_ingests = Vec::new();
        for ingest in &ingests {
            if let Err(error) = self
                .ingest_service
                .update_ingest(
                    &ingest.id,
                    new_name,
                    &ingest.stream_key,
                    ingest.loop_flag,
                    &ingest.start_time,
                    ingest.live_optimized,
                    ingest.target_gop_seconds,
                )
                .await
            {
                rollback_ingest_updates(&self.ingest_service, updated_ingests).await;
                rollback_renames(completed).await;
                return Err(MediaRenameError::IngestUpdate(error.to_string()));
            }
            updated_ingests.push(ingest.clone());
        }

        Ok(ingests.len())
    }
}

async fn rollback_ingest_updates(ingest_service: &IngestService, updated_ingests: Vec<Ingest>) {
    for ingest in updated_ingests.into_iter().rev() {
        let _ = ingest_service
            .update_ingest(
                &ingest.id,
                &ingest.filename,
                &ingest.stream_key,
                ingest.loop_flag,
                &ingest.start_time,
                ingest.live_optimized,
                ingest.target_gop_seconds,
            )
            .await;
    }
}

async fn rollback_renames(completed: Vec<(PathBuf, PathBuf)>) {
    for (rollback_from, rollback_to) in completed.into_iter().rev() {
        let _ = tokio::fs::rename(rollback_to, rollback_from).await;
    }
}

fn media_filename_is_supported(filename: &str) -> bool {
    matches!(
        filename
            .rsplit('.')
            .next()
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("ts" | "mkv" | "mp4" | "mov")
    )
}

fn entry_modified(metadata: &std::fs::Metadata) -> (String, i64) {
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    let modified_at = chrono::DateTime::from_timestamp_millis(modified_ms)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();
    (modified_at, modified_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::{
        IngestCatalogFuture, IngestDeleteFuture, IngestLookup, IngestLookupFuture,
        IngestUpdateFuture, IngestWriteError, IngestWriteFuture, IngestWriter, MetaLookupError,
        MetaStoreWriter, MetaWriteFuture,
    };
    use crate::domain::ids::RecordingId;
    use std::sync::Mutex;

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

        MediaLibraryService::new(
            pool.clone(),
            PipelineService::new(pool.clone()),
            IngestService::new(pool),
        )
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

        fn list_ingests_for_stream_key<'a>(
            &'a self,
            stream_key: &'a str,
        ) -> IngestCatalogFuture<'a> {
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
            crate::types::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
        )
        .await
        .unwrap();
        let service = MediaLibraryService::new(
            pool.clone(),
            PipelineService::new(pool.clone()),
            IngestService::new(pool),
        );
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
        assert!(pairs.iter().any(|(_, to)| {
            to.ends_with("recording_20260709T010203_renamed.ts.conversion.json")
        }));
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
            crate::types::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
        )
        .await
        .unwrap();
        let service = MediaLibraryService::new(
            pool.clone(),
            PipelineService::new(pool.clone()),
            IngestService::new(pool.clone()),
        );
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
            PipelineService::new(pool.clone()),
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

    #[tokio::test]
    async fn recording_start_does_not_touch_runtime_when_persistence_fails() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        let service = MediaLibraryService::with_stores(
            Arc::new(SqliteMetaStore::new(pool.clone())),
            Arc::new(FailingMetaWriter),
            Arc::new(SqliteRecordingStore::new(pool.clone())),
            PipelineService::new(pool.clone()),
            IngestService::new(pool),
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

        assert!(matches!(err, ApiError::Internal(_)));
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
            PipelineService::new(pool.clone()),
            IngestService::new(pool),
        );
        let engine = Arc::new(MediaEngine::new());
        let _token = engine.register_recording("pipe-recording").await;

        let err = service
            .recording_stop(&engine, "pipe-recording")
            .await
            .unwrap_err();

        assert!(matches!(err, ApiError::Internal(_)));
        assert!(engine.is_recording_active("pipe-recording").await);
        engine.unregister_recording("pipe-recording").await;
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
            crate::types::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
        )
        .await
        .unwrap();
        let service = MediaLibraryService::new(
            pool.clone(),
            PipelineService::new(pool.clone()),
            IngestService::new(pool),
        );
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
        let service = MediaLibraryService::new(
            pool.clone(),
            PipelineService::new(pool.clone()),
            IngestService::new(pool.clone()),
        );
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
}
