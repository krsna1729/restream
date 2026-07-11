use std::collections::HashMap;

use crate::application::models::Output;
use crate::media::engine::MediaEngine;

#[derive(Clone, Default)]
pub struct RuntimeViewService;

impl RuntimeViewService {
    pub fn new() -> Self {
        Self
    }

    pub async fn output_status(
        &self,
        engine: &MediaEngine,
        output_id: &str,
    ) -> Option<serde_json::Value> {
        crate::api_runtime_views::output_status(engine, output_id).await
    }

    pub async fn health_snapshot(
        &self,
        engine: &MediaEngine,
        pipeline_ids: &[String],
        recording_enabled: &HashMap<String, bool>,
        disconnect_grace_ms: u64,
    ) -> serde_json::Value {
        crate::api_runtime_views::health_snapshot(
            engine,
            pipeline_ids,
            recording_enabled,
            disconnect_grace_ms,
        )
        .await
    }

    pub async fn health_summary_snapshot(
        &self,
        engine: &MediaEngine,
        pipeline_ids: &[String],
        recording_enabled: &HashMap<String, bool>,
        disconnect_grace_ms: u64,
    ) -> serde_json::Value {
        crate::api_runtime_views::health_summary_snapshot(
            engine,
            pipeline_ids,
            recording_enabled,
            disconnect_grace_ms,
        )
        .await
    }

    pub async fn processing_graph(
        &self,
        engine: &MediaEngine,
        pipeline_id: &str,
        outputs: &[Output],
    ) -> serde_json::Value {
        crate::api_runtime_views::processing_graph(engine, pipeline_id, outputs).await
    }

    pub async fn engine_telemetry(&self, engine: &MediaEngine) -> serde_json::Value {
        crate::api_runtime_views::engine_telemetry(engine).await
    }

    pub async fn resource_map(
        &self,
        engine: &MediaEngine,
        process: crate::api_runtime_views::ProcessResourceSnapshot,
        pipeline_id: Option<&str>,
        options: crate::api_runtime_views::ResourceMapOptions,
    ) -> serde_json::Value {
        crate::api_runtime_views::resource_map(engine, process, pipeline_id, options).await
    }

    pub async fn pipeline_telemetry(
        &self,
        engine: &MediaEngine,
        pipeline_id: &str,
    ) -> serde_json::Value {
        crate::api_runtime_views::pipeline_telemetry(engine, pipeline_id).await
    }

    pub async fn stage_telemetry_by_display(
        &self,
        engine: &MediaEngine,
        stage_key: &str,
    ) -> Option<serde_json::Value> {
        crate::api_runtime_views::stage_telemetry_by_display(engine, stage_key).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::media::engine::MediaEngine;

    #[tokio::test]
    async fn runtime_view_service_builds_health_snapshot() {
        let service = RuntimeViewService::new();
        let engine = MediaEngine::new();
        let pipeline_ids = vec!["pipe-runtime-view".to_string()];

        let snapshot = service
            .health_snapshot(&engine, &pipeline_ids, &HashMap::new(), 0)
            .await;

        assert_eq!(snapshot["status"], "ready");
        assert!(snapshot["pipelines"]["pipe-runtime-view"].is_object());
    }

    #[tokio::test]
    async fn runtime_view_service_returns_none_for_missing_output_status() {
        let service = RuntimeViewService::new();
        let engine = MediaEngine::new();

        assert!(
            service
                .output_status(&engine, "missing-output")
                .await
                .is_none()
        );
    }
}
