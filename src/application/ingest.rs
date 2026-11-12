//! Application-layer ingest coordination that resolves pipelines and loads
//! file-ingest context. Connect-time admission lives in
//! [`crate::application::catalog_auth`].

use crate::application::ports::{
    IngestLookup, IngestLookupError, IngestWriteError, IngestWriter, PipelineStore,
    PipelineStoreError,
};
use restream_domain::domain::pipeline_input::PipelineInput;
use restream_domain::models::{Ingest, Pipeline};
use restream_engine::engine::MediaEngine;
use std::{future::Future, pin::Pin};

pub type PipelineInputLookupFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<PipelineInput>, PipelineStoreError>> + Send + 'a>>;

pub trait PipelineInputLookup: Send + Sync {
    fn get_by_stream_key<'a>(&'a self, stream_key: &'a str) -> PipelineInputLookupFuture<'a>;
}

#[derive(Debug)]
pub struct FileIngestContext {
    pub ingest: Ingest,
    pub pipeline: Pipeline,
}

#[derive(Debug)]
pub struct PipelineFileIngestState {
    pub ingest: Option<Ingest>,
    pub running: bool,
    /// While running: the playlist item playing (0 for a single file).
    pub now_playing: Option<usize>,
}

#[derive(Debug)]
pub enum ClearFileIngestsError {
    PipelineStore(PipelineStoreError),
    IngestLookup(IngestLookupError),
}

#[derive(Debug)]
pub enum ResolveFileIngestError {
    IngestLookup(IngestLookupError),
    PipelineStore(PipelineStoreError),
    MissingPipelineForStreamKey(String),
}

#[derive(Debug, Clone)]
pub struct FileIngestConfig {
    pub filename: String,
    /// Play order when more than one file; the first is `filename`.
    pub playlist: Vec<String>,
    pub loop_flag: bool,
    pub start_time: String,
    pub live_optimized: bool,
    pub target_gop_seconds: u32,
}

#[derive(Debug)]
pub enum PersistFileIngestError {
    IngestLookup(IngestLookupError),
    IngestWrite(IngestWriteError),
    PipelineStore(PipelineStoreError),
}

pub async fn resolve_file_ingest_context(
    ingest_lookup: &dyn IngestLookup,
    pipeline_lookup: &dyn PipelineStore,
    ingest_id: &str,
) -> Result<Option<FileIngestContext>, ResolveFileIngestError> {
    let Some(ingest) = ingest_lookup
        .get_ingest(ingest_id)
        .await
        .map_err(ResolveFileIngestError::IngestLookup)?
    else {
        return Ok(None);
    };

    let pipeline = pipeline_lookup
        .get_pipeline_by_stream_key(&ingest.stream_key)
        .await
        .map_err(ResolveFileIngestError::PipelineStore)?
        .ok_or_else(|| {
            ResolveFileIngestError::MissingPipelineForStreamKey(ingest.stream_key.clone())
        })?;

    Ok(Some(FileIngestContext { ingest, pipeline }))
}

pub async fn load_pipeline_file_ingest_state(
    ingest_lookup: &dyn IngestLookup,
    engine: &MediaEngine,
    pipeline: &Pipeline,
) -> Result<PipelineFileIngestState, IngestLookupError> {
    let ingest = ingest_lookup
        .get_ingest_by_stream_key(&pipeline.stream_key)
        .await?;
    let running = match ingest.as_ref() {
        Some(ingest) => engine.is_file_ingest_running(&ingest.id).await,
        None => false,
    };
    let now_playing = match ingest.as_ref() {
        Some(ingest) if running => engine.file_ingest_position(&ingest.id).await,
        _ => None,
    };

    Ok(PipelineFileIngestState {
        ingest,
        running,
        now_playing,
    })
}

/// Stops the file sources publishing with `stream_key` and releases their
/// sessions now (a restart right after must not find them held); live
/// publishers of the same pipeline keep their sessions.
pub async fn clear_stream_key_file_ingests(
    pipeline_lookup: &dyn PipelineStore,
    ingest_lookup: &dyn IngestLookup,
    engine: &MediaEngine,
    stream_key: &str,
) -> Result<(), ClearFileIngestsError> {
    if let Some(pipeline) = pipeline_lookup
        .get_pipeline_by_stream_key(stream_key)
        .await
        .map_err(ClearFileIngestsError::PipelineStore)?
    {
        engine.unregister_file_ingest(&pipeline.id).await;
    }

    let ingests = ingest_lookup
        .list_ingests_for_stream_key(stream_key)
        .await
        .map_err(ClearFileIngestsError::IngestLookup)?;
    for ingest in ingests {
        let _ = engine.stop_file_ingest_child(&ingest.id).await;
        engine.clear_file_ingest_running(&ingest.id).await;
    }

    Ok(())
}

pub async fn persist_pipeline_file_ingest(
    ingest_lookup: &dyn IngestLookup,
    ingest_writer: &dyn IngestWriter,
    pipeline_store: &dyn PipelineStore,
    pipeline: &Pipeline,
    config: &FileIngestConfig,
    id_factory: impl FnOnce() -> String,
) -> Result<Ingest, PersistFileIngestError> {
    let existing = ingest_lookup
        .get_ingest_by_stream_key(&pipeline.stream_key)
        .await
        .map_err(PersistFileIngestError::IngestLookup)?;

    let saved = match existing {
        Some(ingest) => ingest_writer
            .update_ingest(
                &ingest.id,
                &config.filename,
                &pipeline.stream_key,
                config.loop_flag,
                &config.start_time,
                config.live_optimized,
                config.target_gop_seconds,
            )
            .await
            .map_err(PersistFileIngestError::IngestWrite)?
            .ok_or_else(|| {
                PersistFileIngestError::IngestWrite(IngestWriteError::new("ingest not found"))
            })?,
        None => ingest_writer
            .create_ingest(
                &id_factory(),
                &config.filename,
                &pipeline.stream_key,
                config.loop_flag,
                &config.start_time,
                config.live_optimized,
                config.target_gop_seconds,
            )
            .await
            .map_err(PersistFileIngestError::IngestWrite)?,
    };

    // A playlist of one file is that file.
    let playlist: &[String] = if config.playlist.len() > 1 {
        &config.playlist
    } else {
        &[]
    };
    let saved = ingest_writer
        .set_ingest_playlist(&saved.id, playlist)
        .await
        .map_err(PersistFileIngestError::IngestWrite)?
        .unwrap_or(saved);

    let ingests = ingest_lookup
        .list_ingests_for_stream_key(&pipeline.stream_key)
        .await
        .map_err(PersistFileIngestError::IngestLookup)?;
    for ingest in ingests.into_iter().filter(|ingest| ingest.id != saved.id) {
        let _ = ingest_writer.delete_ingest(&ingest.id).await;
    }

    let input_source = format!("file:{}", config.filename);
    pipeline_store
        .update_pipeline_input_source(pipeline, Some(&input_source))
        .await
        .map_err(PersistFileIngestError::PipelineStore)?;

    Ok(saved)
}

pub async fn remove_pipeline_file_ingest(
    ingest_lookup: &dyn IngestLookup,
    ingest_writer: &dyn IngestWriter,
    pipeline_store: &dyn PipelineStore,
    pipeline: &Pipeline,
) -> Result<(), PersistFileIngestError> {
    let ingests = ingest_lookup
        .list_ingests_for_stream_key(&pipeline.stream_key)
        .await
        .map_err(PersistFileIngestError::IngestLookup)?;
    for ingest in ingests {
        let _ = ingest_writer.delete_ingest(&ingest.id).await;
    }

    pipeline_store
        .update_pipeline_input_source(pipeline, None)
        .await
        .map_err(PersistFileIngestError::PipelineStore)?;

    Ok(())
}

#[cfg(test)]
mod tests;
