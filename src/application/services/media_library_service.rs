use std::sync::Arc;

use sqlx::SqlitePool;

use crate::application::ports::SqliteMetaStore;
use crate::application::recording::{
    load_recording_settings, recording_enabled_meta_key, spawn_recording_task,
};
use crate::db;
use crate::media::engine::MediaEngine;
use crate::types::Pipeline;

use super::error::{ApiError, ApiResult};
use super::pipeline_service::PipelineService;

pub struct MediaLibraryService {
    db: SqlitePool,
    pipeline_service: PipelineService,
}

impl MediaLibraryService {
    pub fn new(db: SqlitePool, pipeline_service: PipelineService) -> Self {
        Self {
            db,
            pipeline_service,
        }
    }

    pub async fn get_pipeline(&self, id: &str) -> ApiResult<Pipeline> {
        self.pipeline_service.get_by_id(id).await
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
        let _ = db::set_meta(&self.db, &meta_key, "1").await;

        let has_ingest = engine.ingests.active.read().await.contains_key(pipeline_id);
        if has_ingest && !engine.is_recording_active(pipeline_id).await {
            let recording_settings =
                load_recording_settings(&SqliteMetaStore::new(self.db.clone())).await;
            spawn_recording_task(
                engine.clone(),
                pipeline_name,
                pipeline_id.to_string(),
                input_source,
                media_dir.to_string(),
                recording_settings,
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
        let _ = db::set_meta(&self.db, &meta_key, "0").await;
        engine.unregister_recording(pipeline_id).await;
        Ok(())
    }
}
