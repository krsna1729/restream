//! Pipeline catalog operations exposed to API handlers.

use restream_domain::models::Pipeline;

use super::AppState;
use crate::application::services::ServiceResult;
use crate::application::services::pipeline_service::PipelineService;

impl AppState {
    /// All pipelines in the catalog.
    pub(crate) async fn list_pipelines(&self) -> ServiceResult<Vec<Pipeline>> {
        self.pipeline_service.list_pipelines().await
    }

    /// The pipeline IDs in dashboard order.
    pub(crate) async fn list_pipeline_ids(&self) -> ServiceResult<Vec<String>> {
        self.pipeline_service.list_pipeline_ids().await
    }

    /// One pipeline by ID.
    pub(crate) async fn pipeline(&self, id: &str) -> ServiceResult<Pipeline> {
        self.pipeline_service.get_by_id(id).await
    }

    /// Creates a pipeline and reloads admission before returning.
    pub(crate) async fn create_pipeline(
        &self,
        id: &str,
        name: &str,
        stream_key: &str,
        input_source: Option<&str>,
        srt_ingest_policy: Option<&str>,
    ) -> ServiceResult<Pipeline> {
        self.pipeline_service
            .create_pipeline(id, name, stream_key, input_source, srt_ingest_policy)
            .await
    }

    /// Updates a pipeline and reloads admission before returning.
    pub(crate) async fn update_pipeline(
        &self,
        id: &str,
        name: &str,
        stream_key: &str,
        input_source: Option<&str>,
        srt_ingest_policy: Option<&str>,
    ) -> ServiceResult<Pipeline> {
        self.pipeline_service
            .update_pipeline(id, name, stream_key, input_source, srt_ingest_policy)
            .await
    }

    /// Deletes a pipeline and reloads admission before returning.
    pub(crate) async fn delete_pipeline(&self, id: &str) -> ServiceResult<bool> {
        self.pipeline_service.delete_pipeline(id).await
    }

    /// The configured ingest host for publish URLs.
    pub(crate) async fn ingest_host(&self) -> String {
        self.pipeline_service.get_ingest_host().await
    }

    /// Replaces the pipeline service, used by failure-injection tests.
    pub fn replace_pipeline_service(&mut self, service: PipelineService) {
        self.pipeline_service = service;
    }
}
