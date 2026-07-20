//! Application service wrapper for file-ingest persistence and runtime
//! lifecycle coordination.
//!
//! This module sits between HTTP handlers and the media engine: it resolves
//! stored ingest configuration, validates media-library paths, and keeps the
//! persistence/runtime cleanup steps aligned when file-ingest state changes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::application::ingest::{
    FileIngestConfig, PipelineFileIngestState, PipelineInputLookup, ResolveFileIngestError,
    clear_stream_key_file_ingests, load_pipeline_file_ingest_state, persist_pipeline_file_ingest,
    remove_pipeline_file_ingest, resolve_file_ingest_context,
};
use crate::application::models::{Ingest, Pipeline};
use crate::application::ports::{IngestLookup, IngestWriter, PipelineStore};
use crate::media::engine::MediaEngine;
use crate::media::external_file_ingest::{
    ExternalFileIngestRuntime, ExternalFileIngestSource, start_external_file_ingest,
};

use super::error::{ServiceError, ServiceResult};
use super::pipeline_service::PipelineService;

/// Transport-facing payload for creating or updating one persisted file ingest
/// configuration before it is translated into the domain/storage model.
pub struct FileIngestConfigInput {
    pub filename: String,
    pub loop_flag: bool,
    pub start_time: String,
    pub live_optimized: bool,
    pub target_gop_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Start-time failures that callers need to distinguish between bad inputs,
/// missing catalog state, and runtime/process startup problems.
pub enum FileIngestStartError {
    NotFound,
    MissingPipelineForStreamKey,
    IngestLookup,
    PipelineStore(String),
    AlreadyRunning,
    InvalidMediaPath,
    MediaFileNotFound,
    PipelineAlreadyActive,
    Spawn(String),
}

/// Application service that coordinates file-ingest persistence with runtime
/// media-engine state so stored config and active ingest processes stay aligned.
pub struct FileIngestService {
    ingest_lookup: Arc<dyn IngestLookup>,
    ingest_writer: Arc<dyn IngestWriter>,
    pipeline_store: Arc<dyn PipelineStore>,
    pipeline_input_lookup: Arc<dyn PipelineInputLookup>,
    pipeline_service: PipelineService,
}

impl FileIngestService {
    /// Builds the service from the lookup/write ports and pipeline catalog it
    /// needs to coordinate file-ingest state.
    pub fn with_ports(
        ingest_lookup: Arc<dyn IngestLookup>,
        ingest_writer: Arc<dyn IngestWriter>,
        pipeline_store: Arc<dyn PipelineStore>,
        pipeline_input_lookup: Arc<dyn PipelineInputLookup>,
        pipeline_service: PipelineService,
    ) -> Self {
        Self {
            ingest_lookup,
            ingest_writer,
            pipeline_store,
            pipeline_input_lookup,
            pipeline_service,
        }
    }

    /// Resolves one user-facing filename inside the configured media library.
    ///
    /// The path must stay relative to `media_dir`; canonicalization rejects
    /// parent traversal, absolute paths, and symlink escapes before the media
    /// engine attempts to read the file.
    pub fn resolve_media_file_path(
        media_dir: &Path,
        filename: &str,
    ) -> Result<PathBuf, FileIngestStartError> {
        if filename.trim().is_empty() {
            return Err(FileIngestStartError::InvalidMediaPath);
        }

        let relative = Path::new(filename);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(FileIngestStartError::InvalidMediaPath);
        }

        let media_root = media_dir
            .canonicalize()
            .map_err(|_| FileIngestStartError::MediaFileNotFound)?;
        let file_path = media_root.join(relative);
        let canonical_file = file_path
            .canonicalize()
            .map_err(|_| FileIngestStartError::MediaFileNotFound)?;

        if !canonical_file.starts_with(&media_root) || !canonical_file.is_file() {
            return Err(FileIngestStartError::InvalidMediaPath);
        }

        Ok(canonical_file)
    }

    /// Resolves one pipeline through the shared pipeline service so file-ingest
    /// handlers can validate pipeline ownership before touching ingest state.
    pub async fn get_pipeline(&self, id: &str) -> ServiceResult<Pipeline> {
        self.pipeline_service.get_by_id(id).await
    }

    /// Rebuilds the derived file-ingest view for one pipeline after a create,
    /// update, stop, or delete operation touches runtime state.
    pub async fn load_pipeline_file_ingest_state(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline: &Pipeline,
    ) -> ServiceResult<PipelineFileIngestState> {
        load_pipeline_file_ingest_state(self.ingest_lookup.as_ref(), engine, pipeline)
            .await
            .map_err(|_| ServiceError::internal("load pipeline file ingest state"))
    }

    /// Looks up one ingest record and normalizes a missing row into the API
    /// layer's stable not-found error.
    async fn get_ingest_or_not_found(&self, id: &str) -> ServiceResult<Ingest> {
        self.ingest_lookup
            .get_ingest(id)
            .await
            .map_err(|err| ServiceError::internal(format!("get ingest: {err}")))?
            .ok_or_else(|| ServiceError::not_found("Ingest not found"))
    }

    /// Clears any runtime markers and persisted runtime-derived state that are
    /// keyed by one stream key after a stop/delete transition.
    async fn clear_stream_key_runtime_state(
        &self,
        engine: &Arc<MediaEngine>,
        stream_key: &str,
        error_context: &'static str,
    ) -> ServiceResult<()> {
        clear_stream_key_file_ingests(
            self.pipeline_store.as_ref(),
            self.ingest_lookup.as_ref(),
            engine,
            stream_key,
        )
        .await
        .map_err(|err| ServiceError::internal(format!("{error_context}: {err:?}")))
    }

    /// Best-effort rollback for a start attempt that already registered runtime
    /// state before a later spawn step failed.
    async fn clear_started_ingest_on_failure(
        engine: &Arc<MediaEngine>,
        ingest_id: &str,
        pipeline_id: &str,
    ) {
        engine.clear_file_ingest_running(ingest_id).await;
        engine.unregister_ingest(pipeline_id).await;
    }

    /// Deletes one stored ingest after clearing any runtime state tied to its
    /// stream key, so the engine does not keep a stale file-ingest session.
    pub async fn delete_ingest_with_runtime_cleanup(
        &self,
        engine: &Arc<MediaEngine>,
        id: &str,
    ) -> ServiceResult<()> {
        let ingest = self.get_ingest_or_not_found(id).await?;

        self.clear_stream_key_runtime_state(engine, &ingest.stream_key, "clear file ingest state")
            .await?;

        let _ = engine.stop_file_ingest_child(id).await;
        engine.clear_file_ingest_running(id).await;
        let deleted = self
            .ingest_writer
            .delete_ingest(id)
            .await
            .map_err(|err| ServiceError::internal(format!("delete ingest: {err}")))?;
        if deleted {
            Ok(())
        } else {
            Err(ServiceError::not_found("Ingest not found"))
        }
    }

    /// Stops the runtime side of an ingest without deleting its persisted
    /// configuration, returning the stored ingest record to the caller.
    pub async fn stop_ingest_with_runtime_cleanup(
        &self,
        engine: &Arc<MediaEngine>,
        id: &str,
    ) -> ServiceResult<Ingest> {
        let ingest = self.get_ingest_or_not_found(id).await?;

        self.clear_stream_key_runtime_state(engine, &ingest.stream_key, "clear file ingest state")
            .await?;

        let _ = engine.stop_file_ingest_child(id).await;
        engine.clear_file_ingest_running(id).await;

        Ok(ingest)
    }

    /// Starts one persisted file ingest against the media engine.
    ///
    /// This function resolves the stored ingest/pipeline pair first, then
    /// registers the runtime attempt before choosing the internal ingest path
    /// or the external FFmpeg child path. Any failure after registration must
    /// unwind the runtime markers so the pipeline can be started again cleanly.
    pub async fn start_ingest(
        &self,
        engine: Arc<MediaEngine>,
        media_dir: &Path,
        id: &str,
    ) -> Result<Ingest, FileIngestStartError> {
        let resolved = match resolve_file_ingest_context(
            self.ingest_lookup.as_ref(),
            self.pipeline_store.as_ref(),
            id,
        )
        .await
        {
            Ok(Some(context)) => context,
            Ok(None) => return Err(FileIngestStartError::NotFound),
            Err(ResolveFileIngestError::MissingPipelineForStreamKey(_)) => {
                return Err(FileIngestStartError::MissingPipelineForStreamKey);
            }
            Err(ResolveFileIngestError::IngestLookup(_)) => {
                return Err(FileIngestStartError::IngestLookup);
            }
            Err(ResolveFileIngestError::PipelineStore(err)) => {
                return Err(FileIngestStartError::PipelineStore(err.to_string()));
            }
        };
        let ingest = resolved.ingest;
        let pipeline = resolved.pipeline;

        if engine.is_file_ingest_running(id).await {
            return Err(FileIngestStartError::AlreadyRunning);
        }

        let file_path = Self::resolve_media_file_path(media_dir, &ingest.filename)?;
        let input = self
            .pipeline_input_lookup
            .get_by_stream_key(&ingest.stream_key)
            .await
            .map_err(|error| FileIngestStartError::PipelineStore(error.to_string()))?
            .ok_or(FileIngestStartError::MissingPipelineForStreamKey)?;

        let ring_buffer = engine.get_or_create_pipeline(&pipeline.id).await;
        let Some(registration) = engine
            .try_register_pipeline_input_attempt(
                &pipeline.id,
                &input.id,
                &ingest.stream_key,
                "file",
                input.selected,
            )
            .await
        else {
            return Err(FileIngestStartError::PipelineAlreadyActive);
        };

        engine.mark_file_ingest_running(&ingest.id).await;

        if crate::media::file_ingest::use_internal_file_ingest(&engine.config)
            && !ingest.live_optimized
        {
            if let Err(err) = crate::media::file_ingest::spawn_internal_file_ingest(
                engine.clone(),
                tokio::runtime::Handle::current(),
                ingest.id.clone(),
                pipeline.id.clone(),
                file_path,
                ingest.start_time.clone(),
                ingest.loop_flag,
                ring_buffer,
                registration,
            ) {
                Self::clear_started_ingest_on_failure(&engine, &ingest.id, &pipeline.id).await;
                return Err(FileIngestStartError::Spawn(err));
            }
        } else {
            let runtime = ExternalFileIngestRuntime {
                engine: engine.clone(),
                ingest_id: ingest.id.clone(),
                pipeline_id: pipeline.id.clone(),
                source: ExternalFileIngestSource {
                    file_path,
                    start_time: ingest.start_time.clone(),
                    loop_enabled: ingest.loop_flag,
                    live_optimized: ingest.live_optimized,
                    target_gop_seconds: ingest.target_gop_seconds,
                },
                ring_buffer,
                registration,
            };
            if let Err(err) = start_external_file_ingest(runtime) {
                Self::clear_started_ingest_on_failure(&engine, &ingest.id, &pipeline.id).await;
                return Err(FileIngestStartError::Spawn(err));
            }
        }

        Ok(ingest)
    }

    /// Applies an optional file-ingest payload to one pipeline and then returns
    /// the rebuilt derived state used by the dashboard.
    pub async fn apply_file_ingest_payload(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline: &Pipeline,
        previous_stream_key: Option<&str>,
        payload: Option<Option<FileIngestConfigInput>>,
    ) -> ServiceResult<PipelineFileIngestState> {
        if let Some(previous_stream_key) =
            previous_stream_key.filter(|previous| *previous != pipeline.stream_key.as_str())
        {
            self.clear_stream_key_runtime_state(
                engine,
                previous_stream_key,
                "clear stream key file ingests (previous)",
            )
            .await?;
        }

        if let Some(payload) = payload {
            match payload {
                Some(input) => {
                    persist_pipeline_file_ingest(
                        self.ingest_lookup.as_ref(),
                        self.ingest_writer.as_ref(),
                        self.pipeline_store.as_ref(),
                        pipeline,
                        &FileIngestConfig {
                            filename: input.filename,
                            loop_flag: input.loop_flag,
                            start_time: input.start_time,
                            live_optimized: input.live_optimized,
                            target_gop_seconds: input.target_gop_seconds,
                        },
                        || {
                            let bytes: [u8; 8] = rand::random();
                            format!(
                                "ingest_{}",
                                bytes
                                    .iter()
                                    .map(|b| format!("{:02x}", b))
                                    .collect::<String>()
                            )
                        },
                    )
                    .await
                    .map_err(|_| ServiceError::internal("persist pipeline file ingest"))?;

                    self.clear_stream_key_runtime_state(
                        engine,
                        &pipeline.stream_key,
                        "clear stream key file ingests (current)",
                    )
                    .await?;
                }
                None => {
                    self.clear_stream_key_runtime_state(
                        engine,
                        &pipeline.stream_key,
                        "clear stream key file ingests (current)",
                    )
                    .await?;

                    remove_pipeline_file_ingest(
                        self.ingest_lookup.as_ref(),
                        self.ingest_writer.as_ref(),
                        self.pipeline_store.as_ref(),
                        pipeline,
                    )
                    .await
                    .map_err(|_| ServiceError::internal("remove pipeline file ingest"))?;
                }
            }
        }

        self.load_pipeline_file_ingest_state(engine, pipeline).await
    }
}

#[cfg(test)]
#[path = "file_ingest_service/tests.rs"]
mod tests;
