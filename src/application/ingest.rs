//! Application-layer ingest coordination that resolves pipelines, loads
//! file-ingest context, and validates stream access before media processing begins.

use crate::application::models::{Ingest, Pipeline};
use crate::application::ports::{
    IngestLookup, IngestLookupError, IngestWriteError, IngestWriter, PipelineStore,
    PipelineStoreError,
};
use crate::domain::pipeline_input::PipelineInput;
use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessFuture,
    PipelineAccessMode,
};
use crate::media::security::{IngestSecurityService, RateLimitScope};
use std::sync::Arc;
use std::{future::Future, pin::Pin};

pub type PipelineInputLookupFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<PipelineInput>, PipelineStoreError>> + Send + 'a>>;

pub trait PipelineInputLookup: Send + Sync {
    fn get_by_stream_key<'a>(&'a self, stream_key: &'a str) -> PipelineInputLookupFuture<'a>;
}

#[derive(Debug)]
pub enum IngestAuthError {
    InvalidStreamKey,
    LookupFailed(PipelineStoreError),
}

pub struct PipelineStoreIngestAuthenticator {
    pipeline_lookup: Arc<dyn PipelineStore>,
    input_lookup: Arc<dyn PipelineInputLookup>,
    security: Arc<IngestSecurityService>,
}

impl PipelineStoreIngestAuthenticator {
    pub fn new(
        pipeline_lookup: Arc<dyn PipelineStore>,
        input_lookup: Arc<dyn PipelineInputLookup>,
        security: Arc<IngestSecurityService>,
    ) -> Self {
        Self {
            pipeline_lookup,
            input_lookup,
            security,
        }
    }
}

impl PipelineAccessAuthenticator for PipelineStoreIngestAuthenticator {
    fn authenticate<'a>(
        &'a self,
        mode: PipelineAccessMode,
        stream_key: &'a str,
        client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            let pipeline = match mode {
                PipelineAccessMode::RtmpPublish => authenticate_publish_stream_key(
                    self.pipeline_lookup.as_ref(),
                    &self.security,
                    stream_key,
                    client_ip,
                )
                .await
                .map_err(pipeline_access_error)?,
                PipelineAccessMode::RtmpPlay => self
                    .pipeline_lookup
                    .get_pipeline_by_stream_key(stream_key)
                    .await
                    .map_err(|err| PipelineAccessError::LookupFailed(err.to_string()))?
                    .ok_or(PipelineAccessError::InvalidStreamKey)?,
                PipelineAccessMode::SrtPublish => authenticate_srt_stream_key(
                    self.pipeline_lookup.as_ref(),
                    &self.security,
                    stream_key,
                    client_ip,
                    RateLimitScope::SrtPublish,
                )
                .await
                .map_err(pipeline_access_error)?,
                PipelineAccessMode::SrtRead => authenticate_srt_stream_key(
                    self.pipeline_lookup.as_ref(),
                    &self.security,
                    stream_key,
                    client_ip,
                    RateLimitScope::SrtRead,
                )
                .await
                .map_err(pipeline_access_error)?,
            };

            let input = self
                .input_lookup
                .get_by_stream_key(stream_key)
                .await
                .map_err(|error| PipelineAccessError::LookupFailed(error.to_string()))?
                .filter(|input| input.pipeline_id == pipeline.id && input.enabled)
                .ok_or(PipelineAccessError::InvalidStreamKey)?;

            Ok(AuthenticatedPipeline {
                id: pipeline.id,
                input_id: input.id,
                selected: input.selected,
            })
        })
    }
}

fn pipeline_access_error(error: IngestAuthError) -> PipelineAccessError {
    match error {
        IngestAuthError::InvalidStreamKey => PipelineAccessError::InvalidStreamKey,
        IngestAuthError::LookupFailed(error) => {
            PipelineAccessError::LookupFailed(error.to_string())
        }
    }
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

    Ok(PipelineFileIngestState { ingest, running })
}

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
        engine.unregister_ingest(&pipeline.id).await;
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

pub async fn authenticate_publish_stream_key(
    pipeline_lookup: &dyn PipelineStore,
    security: &IngestSecurityService,
    stream_key: &str,
    client_ip: &str,
) -> Result<Pipeline, IngestAuthError> {
    authenticate_stream_key_for_scope(
        pipeline_lookup,
        security,
        stream_key,
        client_ip,
        RateLimitScope::RtmpPublish,
        false,
    )
    .await
}

pub async fn authenticate_srt_stream_key(
    pipeline_lookup: &dyn PipelineStore,
    security: &IngestSecurityService,
    stream_key: &str,
    client_ip: &str,
    scope: RateLimitScope,
) -> Result<Pipeline, IngestAuthError> {
    authenticate_stream_key_for_scope(
        pipeline_lookup,
        security,
        stream_key,
        client_ip,
        scope,
        true,
    )
    .await
}

async fn authenticate_stream_key_for_scope(
    pipeline_lookup: &dyn PipelineStore,
    security: &IngestSecurityService,
    stream_key: &str,
    client_ip: &str,
    scope: RateLimitScope,
    clear_on_success: bool,
) -> Result<Pipeline, IngestAuthError> {
    match pipeline_lookup.get_pipeline_by_stream_key(stream_key).await {
        Ok(Some(pipeline)) => {
            if clear_on_success {
                security.record_success_for(scope, client_ip);
            }
            Ok(pipeline)
        }
        Ok(None) => {
            security.record_failure_for(scope, client_ip);
            Err(IngestAuthError::InvalidStreamKey)
        }
        Err(err) => {
            security.record_failure_for(scope, client_ip);
            Err(IngestAuthError::LookupFailed(err))
        }
    }
}

#[cfg(test)]
mod tests;
