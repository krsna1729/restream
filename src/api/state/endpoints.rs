//! Listener ports and pipeline-input operations on the API state.

use super::AppState;
use restream_domain::domain::pipeline_input::{PipelineInput, PipelineInputSource};

impl AppState {
    /// The publish endpoints for `host` on this server's RTMP and SRT
    /// listeners.
    pub(crate) fn publish_endpoints<'a>(
        &self,
        host: &'a str,
    ) -> crate::api::view_models::PublishEndpoints<'a> {
        crate::api::view_models::PublishEndpoints::new(host, self.ports.rtmp, self.ports.srt)
    }

    /// Every listener port in the dashboard's wire shape.
    pub(crate) fn server_ports(&self) -> crate::config::ServerPorts {
        self.ports.server_ports()
    }

    /// Lists one pipeline's inputs.
    pub(crate) async fn pipeline_inputs(
        &self,
        pipeline_id: &str,
    ) -> crate::application::services::ServiceResult<Vec<PipelineInput>> {
        self.pipeline_input_service.list(pipeline_id).await
    }

    /// Creates one pipeline input.
    pub(crate) async fn create_pipeline_input(
        &self,
        pipeline_id: &str,
        label: &str,
        source: PipelineInputSource,
    ) -> crate::application::services::ServiceResult<PipelineInput> {
        self.pipeline_input_service
            .create(pipeline_id, label, source)
            .await
    }

    /// Fetches one pipeline input.
    pub(crate) async fn pipeline_input(
        &self,
        pipeline_id: &str,
        input_id: &str,
    ) -> crate::application::services::ServiceResult<PipelineInput> {
        self.pipeline_input_service.get(pipeline_id, input_id).await
    }

    /// Updates one pipeline input.
    pub(crate) async fn update_pipeline_input(
        &self,
        pipeline_id: &str,
        input_id: &str,
        label: &str,
        enabled: bool,
        source: PipelineInputSource,
    ) -> crate::application::services::ServiceResult<PipelineInput> {
        self.pipeline_input_service
            .update(pipeline_id, input_id, label, enabled, source)
            .await
    }

    /// Deletes one pipeline input.
    pub(crate) async fn delete_pipeline_input(
        &self,
        pipeline_id: &str,
        input_id: &str,
    ) -> crate::application::services::ServiceResult<bool> {
        self.pipeline_input_service
            .delete(pipeline_id, input_id)
            .await
    }

    /// Promotes one pipeline input to the selected input.
    pub(crate) async fn promote_pipeline_input(
        &self,
        pipeline_id: &str,
        input_id: &str,
    ) -> crate::application::services::ServiceResult<PipelineInput> {
        self.pipeline_input_service
            .promote(pipeline_id, input_id)
            .await
    }
}
