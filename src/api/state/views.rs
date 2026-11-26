//! Config-derived runtime views: health, resource attribution, and capacity.

use std::collections::HashMap;

use super::AppState;
use crate::api::runtime_views::ResourceMapOptions;
use crate::application::capacity::{
    Calibration, CapacitySnapshot, NoLiveIngest, Scenario, ScenarioChange,
};
use crate::host::system_sampling::ProcessResourceSnapshot;

impl AppState {
    /// The full health tree for one pipeline set.
    pub(crate) async fn health_snapshot(
        &self,
        pipeline_ids: &[String],
        recording_enabled: &HashMap<String, bool>,
        disconnect_grace_ms: u64,
    ) -> serde_json::Value {
        crate::api::runtime_views::health_snapshot(
            &self.engine,
            &self.config,
            pipeline_ids,
            recording_enabled,
            disconnect_grace_ms,
        )
        .await
    }

    /// The full health view, serialized without an intermediate JSON tree.
    pub(crate) async fn health_snapshot_view(
        &self,
        pipeline_ids: &[String],
        recording_enabled: &HashMap<String, bool>,
        disconnect_grace_ms: u64,
    ) -> impl serde::Serialize + use<> {
        crate::api::runtime_views::health_snapshot_view(
            &self.engine,
            &self.config,
            pipeline_ids,
            recording_enabled,
            disconnect_grace_ms,
        )
        .await
    }

    /// The compact health summary for one pipeline set.
    pub(crate) async fn health_summary_snapshot(
        &self,
        pipeline_ids: &[String],
        recording_enabled: &HashMap<String, bool>,
        disconnect_grace_ms: u64,
    ) -> serde_json::Value {
        crate::api::runtime_views::health_summary_snapshot(
            &self.engine,
            &self.config,
            pipeline_ids,
            recording_enabled,
            disconnect_grace_ms,
        )
        .await
    }

    /// The resource attribution map for one process sample.
    pub(crate) async fn resource_map(
        &self,
        process: ProcessResourceSnapshot,
        pipeline_id: Option<&str>,
        options: ResourceMapOptions,
    ) -> serde_json::Value {
        crate::api::runtime_views::resource_map(
            &self.engine,
            &self.config,
            process,
            pipeline_id,
            options,
        )
        .await
    }

    /// The current capacity priors.
    #[cfg(feature = "agent-execution")]
    pub(crate) async fn capacity_priors(&self) -> CapacitySnapshot {
        crate::application::capacity::capacity_priors(&self.engine, &self.config).await
    }

    /// The current capacity snapshot.
    pub(crate) async fn capacity_snapshot(&self) -> CapacitySnapshot {
        crate::application::capacity::capacity_snapshot(&self.engine, &self.config).await
    }

    /// The capacity scenario for one proposed change.
    pub(crate) async fn capacity_scenario(
        &self,
        change: &ScenarioChange,
    ) -> Result<Scenario, NoLiveIngest> {
        crate::application::capacity::capacity_scenario(&self.engine, &self.config, change).await
    }

    /// The capacity-model calibration.
    pub(crate) fn capacity_calibration(&self) -> Calibration {
        crate::application::capacity::capacity_calibration(&self.config)
    }

    /// The configured output reconnect policy.
    pub(crate) fn output_retry_policy(&self) -> crate::application::reconcile::OutputRetryPolicy {
        self.config.tuning.output_retry_policy()
    }

    /// The configured Tokio worker count.
    pub(crate) fn tokio_worker_threads(&self) -> usize {
        self.config.tokio_runtime.worker_threads
    }
}
