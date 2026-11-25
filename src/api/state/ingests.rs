//! File-ingest and recirculation operations on the API state.

use super::AppState;

impl AppState {
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
