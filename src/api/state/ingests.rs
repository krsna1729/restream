//! File-ingest and recirculation operations on the API state.

use std::path::Path;

use super::AppState;
use crate::application::ingest::PipelineFileIngestState;
use crate::application::services::file_ingest_service::{
    FileIngestConfigInput, FileIngestStartError,
};
use restream_domain::models::{Ingest, Pipeline};

impl AppState {
    /// Fetches the pipeline that owns one file-ingest configuration.
    pub(crate) async fn file_ingest_pipeline(
        &self,
        pipeline_id: &str,
    ) -> crate::application::services::ServiceResult<Pipeline> {
        self.file_ingest_service.get_pipeline(pipeline_id).await
    }

    /// Loads one pipeline's file-ingest state.
    pub(crate) async fn file_ingest_state(
        &self,
        pipeline: &Pipeline,
    ) -> crate::application::services::ServiceResult<PipelineFileIngestState> {
        self.file_ingest_service
            .load_pipeline_file_ingest_state(&self.engine, pipeline)
            .await
    }

    /// Applies a file-ingest configuration change to one pipeline.
    pub(crate) async fn apply_file_ingest(
        &self,
        pipeline: &Pipeline,
        previous_stream_key: Option<&str>,
        payload: Option<Option<FileIngestConfigInput>>,
    ) -> crate::application::services::ServiceResult<PipelineFileIngestState> {
        self.file_ingest_service
            .apply_file_ingest_payload(&self.engine, pipeline, previous_stream_key, payload)
            .await
    }

    /// Deletes one ingest and clears its runtime state.
    pub(crate) async fn delete_ingest_with_cleanup(
        &self,
        id: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.file_ingest_service
            .delete_ingest_with_runtime_cleanup(&self.engine, id)
            .await
    }

    /// Starts one file ingest.
    pub(crate) async fn start_ingest(&self, id: &str) -> Result<Ingest, FileIngestStartError> {
        self.file_ingest_service
            .start_ingest(self.engine.clone(), Path::new(&self.media_dir), id)
            .await
    }

    /// Stops one file ingest and clears its runtime state.
    pub(crate) async fn stop_ingest_with_cleanup(
        &self,
        id: &str,
    ) -> crate::application::services::ServiceResult<Ingest> {
        self.file_ingest_service
            .stop_ingest_with_runtime_cleanup(&self.engine, id)
            .await
    }

    /// Lists the persisted file ingests.
    pub(crate) async fn list_ingests(
        &self,
    ) -> crate::application::services::ServiceResult<Vec<restream_domain::models::Ingest>> {
        self.ingest_service.list_ingests().await
    }

    /// Creates one persisted file ingest.
    pub(crate) async fn create_ingest(
        &self,
        id: &str,
        payload: &crate::api::NormalizedIngestPayload,
    ) -> crate::application::services::ServiceResult<restream_domain::models::Ingest> {
        self.ingest_service
            .create_ingest(
                id,
                &payload.filename,
                &payload.stream_key,
                payload.loop_flag,
                &payload.start_time,
                payload.live_optimized,
                payload.target_gop_seconds,
            )
            .await
    }

    /// Updates one persisted file ingest.
    pub(crate) async fn update_ingest(
        &self,
        id: &str,
        payload: &crate::api::NormalizedIngestPayload,
    ) -> crate::application::services::ServiceResult<restream_domain::models::Ingest> {
        self.ingest_service
            .update_ingest(
                id,
                &payload.filename,
                &payload.stream_key,
                payload.loop_flag,
                &payload.start_time,
                payload.live_optimized,
                payload.target_gop_seconds,
            )
            .await
    }

    /// Validates a recirculation target before an output is reserved.
    pub(crate) async fn validate_recirculation_candidate(
        &self,
        source_pipeline_id: &str,
        target: &restream_domain::domain::output_spec::RecirculationTarget,
    ) -> crate::application::services::ServiceResult<()> {
        self.recirculation_service
            .validate_output_candidate(source_pipeline_id, target)
            .await
    }
}
