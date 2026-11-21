//! Media library service boundary for file-browser and recording helpers.
//!
//! This service joins filesystem state, ingest references, and recording
//! metadata so handlers can expose one media-library view without duplicating
//! cross-store coordination logic.

use std::sync::Arc;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::application::recording::{
    load_recording_settings, recording_enabled_meta_key, spawn_recording_task,
};
use crate::db::DeadlineDb;
use restream_domain::models::Pipeline;
use restream_engine::engine::MediaEngine;
use restream_engine::recording::RecordingMetadataReporter;

use super::error::{ServiceError, ServiceResult};
use super::ingest_service::IngestService;
use super::pipeline_service::PipelineService;

/// Posters extracted at once; the rest wait their turn.
const POSTER_CONCURRENCY: usize = 2;

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
    db: DeadlineDb,
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
    /// Builds the service over the database it reads recordings and
    /// recording switches from.
    ///
    /// Handlers stay HTTP-focused while this layer owns the cross-store joining
    /// needed for media-library responses and rename/delete workflows.
    pub fn new(
        db: DeadlineDb,
        pipeline_service: PipelineService,
        ingest_service: IngestService,
    ) -> Self {
        Self {
            db,
            pipeline_service,
            ingest_service,
            recording_metadata: None,
        }
    }

    /// Persists one pipeline's recording switch (`"1"` on, `"0"` off).
    async fn set_recording_switch(
        &self,
        pipeline_id: &str,
        value: &'static str,
    ) -> Result<(), String> {
        let key = recording_enabled_meta_key(pipeline_id);
        self.db
            .write(move |pool| async move { crate::db::set_meta(&pool, &key, value).await })
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
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
        let entries = read_media_entries(media_dir).await;
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
            // A recording's MP4 is listed on its `.ts` source's row.
            if name.ends_with(".mp4") && has_recording_source(&name, &entries) {
                continue;
            }

            let ingests = self
                .ingest_service
                .list_for_filename(&name)
                .await
                .unwrap_or_default();
            let recording_meta = recording_metadata.get(&name);
            let kind =
                if recording_meta.is_some() || name.to_ascii_lowercase().contains("recording") {
                    "recording"
                } else {
                    "source"
                };
            let source_modified_ms = entry.modified_ms;
            let mut file = media_file(
                name,
                entry,
                ingests.len(),
                kind,
                recording_fields(recording_meta),
            );
            if restream_engine::recording::is_recording_source_filename(&file.name) {
                apply_conversion(
                    &mut file,
                    source_modified_ms,
                    media_dir,
                    &entries,
                    &mut consumed,
                );
            }
            files.push(file);
        }

        files
    }

    /// Inspects a stored file with the external `ffprobe` (bounded in time;
    /// nothing is parsed inside the engine).
    pub async fn analyze_media_file(
        &self,
        path: PathBuf,
    ) -> Result<restream_engine::file_analysis::MediaFileAnalysis, String> {
        restream_engine::file_analysis::analyze_media_file(&path).await
    }

    /// A PNG poster of one frame of a stored file, made by the external
    /// FFmpeg, at most `POSTER_CONCURRENCY` at a time so a library page
    /// asking for many cannot start a process per file at once; a crash in
    /// FFmpeg is an error for this poster, not the engine.
    pub async fn media_poster(&self, path: PathBuf) -> Result<Vec<u8>, String> {
        static PERMITS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
        let _permit = PERMITS
            .get_or_init(|| tokio::sync::Semaphore::new(POSTER_CONCURRENCY))
            .acquire()
            .await
            .map_err(|error| format!("poster queue closed: {error}"))?;
        restream_engine::poster::extract_poster(&path).await
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
        self.set_recording_switch(pipeline_id, "1")
            .await
            .map_err(|e| ServiceError::internal(format!("set recording enabled: {e}")))?;

        let has_ingest = engine.ingests.active.read().await.contains_key(pipeline_id);
        if has_ingest && !engine.is_recording_active(pipeline_id).await {
            let recording_settings = load_recording_settings(&self.db).await;
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
        self.set_recording_switch(pipeline_id, "0")
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
            .db
            .read(crate::db::list_recordings)
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
        if restream_engine::recording::is_recording_source_filename(filename) {
            let converted_path = restream_engine::recording::build_mp4_path(canonical_path);
            if converted_path.exists() {
                delete_paths.push(converted_path);
            }
            let state_path =
                restream_engine::recording::build_conversion_state_path(canonical_path);
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
        if restream_engine::recording::is_recording_source_filename(filename) {
            let source_converted = restream_engine::recording::build_mp4_path(source_path);
            let destination_converted =
                restream_engine::recording::build_mp4_path(destination_path);
            if source_converted.exists() {
                if destination_converted.exists() {
                    return Err(MediaRenamePlanError::ConvertedExists);
                }
                rename_pairs.push((source_converted, destination_converted));
            }

            let source_state = restream_engine::recording::build_conversion_state_path(source_path);
            let destination_state =
                restream_engine::recording::build_conversion_state_path(destination_path);
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
                .rename_ingest_file(&ingest.id, filename, new_name)
                .await
            {
                rollback_ingest_updates(&self.ingest_service, updated_ingests).await;
                rollback_renames(completed).await;
                return Err(MediaRenameError::IngestUpdate(error.to_string()));
            }
            updated_ingests.push((
                ingest.id.clone(),
                filename.to_string(),
                new_name.to_string(),
            ));
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

/// Best-effort rollback for ingest renames (id, from, to) after a later
/// rename step fails: each is renamed back.
async fn rollback_ingest_updates(
    ingest_service: &IngestService,
    updated_ingests: Vec<(String, String, String)>,
) {
    for (id, from, to) in updated_ingests.into_iter().rev() {
        let _ = ingest_service.rename_ingest_file(&id, &to, &from).await;
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
    let modified_at = u64::try_from(modified_ms)
        .ok()
        .and_then(|ms| std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_millis(ms)))
        .map(restream_domain::timestamp::rfc3339)
        .unwrap_or_default();
    (modified_at, modified_ms)
}

/// The library's supported files by name.
async fn read_media_entries(media_dir: &str) -> HashMap<String, MediaDirEntry> {
    let mut entries = HashMap::new();
    let Ok(mut media_dir_entries) = tokio::fs::read_dir(media_dir).await else {
        return entries;
    };
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
    entries
}

/// Whether an MP4 has the recording `.ts` source it was converted from.
fn has_recording_source(mp4_name: &str, entries: &HashMap<String, MediaDirEntry>) -> bool {
    Path::new(mp4_name)
        .with_extension("ts")
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .is_some_and(|source| {
            restream_engine::recording::is_recording_source_filename(&source)
                && entries.contains_key(&source)
        })
}

/// A row for one file played as it is.
fn media_file(
    name: String,
    entry: MediaDirEntry,
    ingest_count: usize,
    kind: &str,
    recording: RecordingFields,
) -> MediaLibraryFile {
    MediaLibraryFile {
        name,
        size: entry.size,
        modified_at: entry.modified_at,
        ingest_count,
        kind: kind.to_string(),
        source_name: entry.name.clone(),
        source_size: entry.size,
        converted_name: None,
        converted_size: None,
        play_name: Some(entry.name),
        conversion_status: None,
        conversion_error: None,
        conversion_updated_at: None,
        recording_id: recording.recording_id,
        pipeline_id: recording.pipeline_id,
        recording_status: recording.recording_status,
        recording_started_at: recording.recording_started_at,
        recording_ended_at: recording.recording_ended_at,
        recording_codec_summary: recording.recording_codec_summary,
        recording_error: recording.recording_error,
    }
}

/// Folds a recording source's MP4 conversion into its row: the MP4 (only
/// when it exists) is what plays, its size adds to the row's, the newer
/// time wins, and the conversion state is shown. The MP4 is marked consumed.
fn apply_conversion(
    file: &mut MediaLibraryFile,
    source_modified_ms: i64,
    media_dir: &str,
    entries: &HashMap<String, MediaDirEntry>,
    consumed: &mut HashSet<String>,
) {
    let source_path = Path::new(media_dir).join(&file.name);
    let converted = restream_engine::recording::build_mp4_path(&source_path)
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .and_then(|candidate| entries.get(&candidate));
    if let Some(converted) = converted {
        consumed.insert(converted.name.clone());
    }
    let conversion_state = restream_engine::recording::load_conversion_state(&source_path);
    file.conversion_status = if converted.is_some() {
        Some("ready".to_string())
    } else {
        conversion_state.as_ref().map(|state| {
            match state.status {
                restream_engine::recording::RecordingConversionStatus::Converting => "converting",
                restream_engine::recording::RecordingConversionStatus::Ready => "ready",
                restream_engine::recording::RecordingConversionStatus::Failed => "failed",
            }
            .to_string()
        })
    };
    file.conversion_error = conversion_state
        .as_ref()
        .and_then(|state| state.error.clone());
    file.conversion_updated_at = conversion_state.map(|state| state.updated_at);
    file.converted_name = converted.map(|value| value.name.clone());
    file.converted_size = converted.map(|value| value.size);
    file.play_name = file.converted_name.clone();
    file.size = file.source_size + file.converted_size.unwrap_or(0);
    if let Some(converted) = converted.filter(|value| value.modified_ms > source_modified_ms) {
        file.modified_at = converted.modified_at.clone();
    }
}

#[cfg(test)]
mod tests;
