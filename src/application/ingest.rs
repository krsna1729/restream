//! Application-layer ingest coordination that resolves pipelines and loads
//! file-ingest context. Connect-time admission lives in
//! [`crate::application::catalog_auth`].

use crate::db::{self, DeadlineDb};
use restream_domain::domain::pipeline_input::PipelineInput;
use restream_domain::models::{Ingest, Pipeline};
use restream_engine::engine::MediaEngine;
use std::{future::Future, pin::Pin};

pub type PipelineInputLookupFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<PipelineInput>, sqlx::Error>> + Send + 'a>>;

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
    PipelineStore(sqlx::Error),
    IngestLookup(sqlx::Error),
}

#[derive(Debug)]
pub enum ResolveFileIngestError {
    IngestLookup(sqlx::Error),
    PipelineStore(sqlx::Error),
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
    IngestLookup(sqlx::Error),
    IngestWrite(sqlx::Error),
    PipelineStore(sqlx::Error),
}

pub async fn resolve_file_ingest_context(
    db: &DeadlineDb,
    ingest_id: &str,
) -> Result<Option<FileIngestContext>, ResolveFileIngestError> {
    let Some(ingest) = db
        .read(|pool| db::get_ingest(pool, ingest_id))
        .await
        .map_err(ResolveFileIngestError::IngestLookup)?
    else {
        return Ok(None);
    };

    let pipeline = db
        .read(|pool| db::get_pipeline_by_stream_key(pool, &ingest.stream_key))
        .await
        .map_err(ResolveFileIngestError::PipelineStore)?
        .ok_or_else(|| {
            ResolveFileIngestError::MissingPipelineForStreamKey(ingest.stream_key.clone())
        })?;

    Ok(Some(FileIngestContext {
        ingest: ingest.into(),
        pipeline: pipeline.into(),
    }))
}

pub async fn load_pipeline_file_ingest_state(
    db: &DeadlineDb,
    engine: &MediaEngine,
    pipeline: &Pipeline,
) -> Result<PipelineFileIngestState, sqlx::Error> {
    let ingest = db
        .read(|pool| db::get_ingest_by_stream_key(pool, &pipeline.stream_key))
        .await?
        .map(Ingest::from);
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
    db: &DeadlineDb,
    engine: &MediaEngine,
    stream_key: &str,
) -> Result<(), ClearFileIngestsError> {
    if let Some(pipeline) = db
        .read(|pool| db::get_pipeline_by_stream_key(pool, stream_key))
        .await
        .map_err(ClearFileIngestsError::PipelineStore)?
    {
        engine.unregister_file_ingest(&pipeline.id).await;
    }

    let ingests = db
        .read(|pool| db::list_ingests_for_stream_key(pool, stream_key))
        .await
        .map_err(ClearFileIngestsError::IngestLookup)?;
    for ingest in ingests {
        let _ = engine.stop_file_ingest_child(&ingest.id).await;
        engine.clear_file_ingest_running(&ingest.id).await;
    }

    Ok(())
}

pub async fn persist_pipeline_file_ingest(
    db: &DeadlineDb,
    pipeline: &Pipeline,
    config: FileIngestConfig,
    id_factory: impl FnOnce() -> String,
) -> Result<Ingest, PersistFileIngestError> {
    let existing = db
        .read(|pool| db::get_ingest_by_stream_key(pool, &pipeline.stream_key))
        .await
        .map_err(PersistFileIngestError::IngestLookup)?;

    let FileIngestConfig {
        filename,
        playlist,
        loop_flag,
        start_time,
        live_optimized,
        target_gop_seconds,
    } = config;
    let input_source = format!("file:{filename}");
    let stream_key = pipeline.stream_key.clone();
    let update = existing.is_some();
    let id = existing.map_or_else(id_factory, |ingest| ingest.id);
    let saved = db
        .write(move |pool| async move {
            if update {
                db::update_ingest(
                    &pool,
                    &id,
                    &filename,
                    &stream_key,
                    loop_flag,
                    &start_time,
                    live_optimized,
                    target_gop_seconds,
                )
                .await?
                .ok_or(sqlx::Error::RowNotFound)
            } else {
                db::create_ingest(
                    &pool,
                    &id,
                    &filename,
                    &stream_key,
                    loop_flag,
                    &start_time,
                    live_optimized,
                    target_gop_seconds,
                )
                .await
            }
        })
        .await
        .map_err(PersistFileIngestError::IngestWrite)?;

    // A playlist of one file is that file.
    let playlist = (playlist.len() > 1)
        .then(|| serde_json::to_string(&playlist))
        .transpose()
        .map_err(|error| PersistFileIngestError::IngestWrite(sqlx::Error::Encode(error.into())))?;
    let id = saved.id.clone();
    let saved =
        db.write(move |pool| async move {
            db::set_ingest_playlist(&pool, &id, playlist.as_deref()).await
        })
        .await
        .map_err(PersistFileIngestError::IngestWrite)?
        .unwrap_or(saved);

    let ingests = db
        .read(|pool| db::list_ingests_for_stream_key(pool, &pipeline.stream_key))
        .await
        .map_err(PersistFileIngestError::IngestLookup)?;
    for ingest in ingests.into_iter().filter(|ingest| ingest.id != saved.id) {
        let _ = db
            .write(move |pool| async move { db::delete_ingest(&pool, &ingest.id).await })
            .await;
    }

    let id = pipeline.id.clone();
    db.write(move |pool| async move {
        db::update_pipeline_input_source(&pool, &id, Some(&input_source)).await
    })
    .await
    .map_err(PersistFileIngestError::PipelineStore)?;

    Ok(saved.into())
}

pub async fn remove_pipeline_file_ingest(
    db: &DeadlineDb,
    pipeline: &Pipeline,
) -> Result<(), PersistFileIngestError> {
    let ingests = db
        .read(|pool| db::list_ingests_for_stream_key(pool, &pipeline.stream_key))
        .await
        .map_err(PersistFileIngestError::IngestLookup)?;
    for ingest in ingests {
        let _ = db
            .write(move |pool| async move { db::delete_ingest(&pool, &ingest.id).await })
            .await;
    }

    let id = pipeline.id.clone();
    db.write(move |pool| async move { db::update_pipeline_input_source(&pool, &id, None).await })
        .await
        .map_err(PersistFileIngestError::PipelineStore)?;

    Ok(())
}

#[cfg(test)]
mod tests;
