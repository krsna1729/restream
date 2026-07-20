//! Media library service boundary for file-browser and recording helpers.
//!
//! This service joins filesystem state, ingest references, and recording
//! metadata so handlers can expose one media-library view without duplicating
//! cross-store coordination logic.

use std::sync::Arc;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::application::models::{Ingest, Pipeline};
use crate::application::ports::{MetaStore, MetaStoreWriter, RecordingStore};
use crate::application::recording::{
    load_recording_settings, recording_enabled_meta_key, spawn_recording_task,
};
use crate::media::engine::MediaEngine;
use crate::media::recording::RecordingMetadataReporter;

use super::error::{ServiceError, ServiceResult};
use super::ingest_service::IngestService;
use super::pipeline_service::PipelineService;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Recording metadata projected onto a media-library row when a file is backed
/// by a persisted recording session.
pub struct MediaRecordingMetadata {
    pub recording_id: String,
    pub pipeline_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: String,
    pub codec_summary: Option<String>,
    pub error: Option<String>,
}

/// Application service that joins filesystem entries, ingest references, and
/// recording metadata into the dashboard's media-library view.
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
/// Serialized media-library row exposed to the dashboard after source,
/// conversion, ingest, and recording metadata are folded together.
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

#[derive(Default)]
struct RecordingFields {
    recording_id: Option<String>,
    pipeline_id: Option<String>,
    recording_status: Option<String>,
    recording_started_at: Option<String>,
    recording_ended_at: Option<String>,
    recording_codec_summary: Option<String>,
    recording_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Planning failures for rename companion files before any filesystem changes
/// are attempted.
pub enum MediaRenamePlanError {
    ConvertedExists,
    ConversionStateExists,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// User-visible rename failures that distinguish preflight conflicts, I/O
/// issues, and ingest-reference update problems.
pub enum MediaRenameError {
    ConvertedExists,
    ConversionStateExists,
    Io(String),
    IngestUpdate(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Delete failures that distinguish configured ingest dependencies from file
/// removal and lookup problems.
pub enum MediaDeleteError {
    HasConfiguredIngests,
    Dependency(String),
    NotFound,
    Io(String),
}

impl MediaLibraryService {
    /// Builds the service from the persistence ports it coordinates.
    ///
    /// Handlers stay HTTP-focused while this layer owns the cross-store joining
    /// needed for media-library responses and rename/delete workflows.
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

    /// Attaches the optional recording metadata reporter used to enrich media
    /// rows and recording lifecycle actions.
    pub fn with_recording_metadata(
        mut self,
        recording_metadata: RecordingMetadataReporter,
    ) -> Self {
        self.recording_metadata = Some(recording_metadata);
        self
    }

    /// Resolves one pipeline so media-library flows can verify ownership before
    /// starting or stopping recording work.
    pub async fn get_pipeline(&self, id: &str) -> ServiceResult<Pipeline> {
        self.pipeline_service.get_by_id(id).await
    }

    /// Lists visible media files and folds companion recording artifacts into a
    /// single response row when possible.
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
            let RecordingFields {
                recording_id,
                pipeline_id,
                recording_status,
                recording_started_at,
                recording_ended_at,
                recording_codec_summary,
                recording_error,
            } = recording_fields(recording_meta);

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
                    recording_id,
                    pipeline_id,
                    recording_status,
                    recording_started_at,
                    recording_ended_at,
                    recording_codec_summary,
                    recording_error,
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
                recording_id,
                pipeline_id,
                recording_status,
                recording_started_at,
                recording_ended_at,
                recording_codec_summary,
                recording_error,
            });
        }

        files
    }

    /// Runs synchronous FFmpeg-backed file inspection on Tokio's blocking pool
    /// and normalizes both analysis and worker failures for application callers.
    pub async fn analyze_media_file(
        &self,
        path: PathBuf,
    ) -> Result<crate::media::file_analysis::MediaFileAnalysis, String> {
        tokio::task::spawn_blocking(move || crate::media::file_analysis::analyze_media_file(&path))
            .await
            .map_err(|error| format!("analysis task failed: {error}"))?
    }

    /// Enables recording for one pipeline and starts the runtime recording task
    /// immediately when the pipeline is already ingesting live media.
    pub async fn recording_start(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline_id: &str,
        pipeline_name: String,
        input_source: Option<String>,
        media_dir: &str,
    ) -> ServiceResult<bool> {
        let meta_key = recording_enabled_meta_key(pipeline_id);
        self.meta_writer
            .set_meta(&meta_key, "1")
            .await
            .map_err(|e| ServiceError::internal(format!("set recording enabled: {e}")))?;

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

    /// Disables recording for one pipeline and unregisters any active runtime
    /// recorder from the media engine.
    pub async fn recording_stop(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline_id: &str,
    ) -> ServiceResult<()> {
        let meta_key = recording_enabled_meta_key(pipeline_id);
        self.meta_writer
            .set_meta(&meta_key, "0")
            .await
            .map_err(|e| ServiceError::internal(format!("set recording disabled: {e}")))?;
        engine.unregister_recording(pipeline_id).await;
        Ok(())
    }

    /// Indexes persisted recording rows by basename so media browser entries
    /// can match either temporary TS files or finalized MP4 outputs.
    pub async fn recording_metadata_by_filename(
        &self,
        filenames: impl IntoIterator<Item = String>,
    ) -> ServiceResult<HashMap<String, MediaRecordingMetadata>> {
        // Index by basename so the media browser can match either temporary TS
        // files or finalized MP4 outputs without caring which path the recorder
        // persisted.
        let requested = filenames.into_iter().collect::<HashSet<_>>();
        if requested.is_empty() {
            return Ok(HashMap::new());
        }

        let rows = self
            .recording_store
            .list_recordings()
            .await
            .map_err(|e| ServiceError::internal(format!("list recordings: {e}")))?;
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

    /// Computes the set of filesystem paths that should be deleted for one
    /// media entry, including recording companion files when present.
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

    /// Deletes one media entry after confirming that no configured ingests
    /// still reference it.
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

    /// Plans the filesystem rename set for one media entry, including
    /// recording companion files and conversion state when applicable.
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

    /// Renames one media entry and then updates any ingest references that
    /// point at it, rolling back earlier work if a later stage fails.
    pub async fn rename_media_file(
        &self,
        filename: &str,
        new_name: &str,
        source_path: &Path,
        destination_path: &Path,
    ) -> Result<usize, MediaRenameError> {
        // Rename filesystem companions first, then update ingest references. If
        // either stage fails, roll back the earlier step to keep the library
        // and ingest configuration aligned.
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

/// Projects optional recording metadata into the serialized media row fields.
fn recording_fields(recording_meta: Option<&MediaRecordingMetadata>) -> RecordingFields {
    RecordingFields {
        recording_id: recording_meta.map(|row| row.recording_id.clone()),
        pipeline_id: recording_meta.map(|row| row.pipeline_id.clone()),
        recording_status: recording_meta.map(|row| row.status.clone()),
        recording_started_at: recording_meta.map(|row| row.started_at.clone()),
        recording_ended_at: recording_meta.and_then(|row| row.ended_at.clone()),
        recording_codec_summary: recording_meta.and_then(|row| row.codec_summary.clone()),
        recording_error: recording_meta.and_then(|row| row.error.clone()),
    }
}

/// Best-effort rollback for ingest filename updates after a later rename step
/// fails.
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

/// Best-effort filesystem rollback for rename steps that already completed
/// before a later rename or ingest update failed.
async fn rollback_renames(completed: Vec<(PathBuf, PathBuf)>) {
    for (rollback_from, rollback_to) in completed.into_iter().rev() {
        let _ = tokio::fs::rename(rollback_to, rollback_from).await;
    }
}

/// Limits the media browser to the source/recording extensions the dashboard
/// currently knows how to present.
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

/// Normalizes filesystem modification metadata into both RFC3339 text and the
/// millisecond sort key used while building media rows.
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
#[path = "media_library_service/tests.rs"]
mod tests;
