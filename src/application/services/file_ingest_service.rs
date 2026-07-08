use std::sync::Arc;

use sqlx::SqlitePool;

use crate::application::ingest::{
    FileIngestConfig, PipelineFileIngestState, clear_stream_key_file_ingests,
    load_pipeline_file_ingest_state, persist_pipeline_file_ingest, remove_pipeline_file_ingest,
};
use crate::application::ports::{IngestLookup, SqliteIngestLookup, SqlitePipelineStore};
use crate::media::engine::MediaEngine;
use crate::types::Pipeline;

use super::error::{ApiError, ApiResult};
use super::pipeline_service::PipelineService;

pub struct FileIngestConfigInput {
    pub filename: String,
    pub loop_flag: bool,
    pub start_time: String,
    pub live_optimized: bool,
    pub target_gop_seconds: u32,
}

pub struct FileIngestService {
    db: SqlitePool,
    pipeline_service: PipelineService,
}

impl FileIngestService {
    pub fn new(db: SqlitePool, pipeline_service: PipelineService) -> Self {
        Self {
            db,
            pipeline_service,
        }
    }

    pub async fn get_pipeline(&self, id: &str) -> ApiResult<Pipeline> {
        self.pipeline_service.get_by_id(id).await
    }

    pub async fn apply_file_ingest_payload(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline: &Pipeline,
        previous_stream_key: Option<&str>,
        payload: Option<Option<FileIngestConfigInput>>,
    ) -> ApiResult<PipelineFileIngestState> {
        let ingest_store = SqliteIngestLookup::new(self.db.clone());
        let pipeline_store = SqlitePipelineStore::new(self.db.clone());

        if let Some(previous_stream_key) =
            previous_stream_key.filter(|previous| *previous != pipeline.stream_key.as_str())
        {
            clear_stream_key_file_ingests(
                &pipeline_store,
                &ingest_store,
                engine,
                previous_stream_key,
            )
            .await
            .map_err(|_| ApiError::internal("clear stream key file ingests (previous)"))?;
        }

        if let Some(payload) = payload {
            clear_stream_key_file_ingests(
                &pipeline_store,
                &ingest_store,
                engine,
                &pipeline.stream_key,
            )
            .await
            .map_err(|_| ApiError::internal("clear stream key file ingests (current)"))?;

            match payload {
                Some(input) => {
                    let _ = persist_pipeline_file_ingest(
                        &ingest_store,
                        &ingest_store,
                        &pipeline_store,
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
                    .await;
                }
                None => {
                    remove_pipeline_file_ingest(
                        &ingest_store,
                        &ingest_store,
                        &pipeline_store,
                        pipeline,
                    )
                    .await
                    .map_err(|_| ApiError::internal("remove pipeline file ingest"))?;
                }
            }
        }

        load_pipeline_file_ingest_state(&ingest_store, engine, pipeline)
            .await
            .map_err(|_| ApiError::internal("load pipeline file ingest state"))
    }
}
