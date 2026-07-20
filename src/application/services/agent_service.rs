//! Application service wrapper for agent catalog reads and output mutations.
//!
//! This module gathers data from several persistence ports so the API layer can
//! expose a single operator/agent context view without duplicating cross-store
//! reads or fallback policy. When agent execution is enabled, it also owns the
//! persistence/runtime coordination for already-validated output changes.

use std::sync::Arc;

use crate::application::models::{Ingest, Job, Output, Pipeline};
use crate::application::ports::{
    IngestHostStore, IngestLookup, JobStore, MetaStore, OutputStore, PipelineStore,
};
use crate::application::settings::{SettingsSnapshot, load_settings_snapshot};
use crate::media::security::IngestSecurityService;
use crate::planner::BackendPolicy;

#[cfg(feature = "agent-execution")]
use crate::domain::output_spec::OutputConfig;
#[cfg(feature = "agent-execution")]
use crate::domain::state::DesiredOutputState;
#[cfg(feature = "agent-execution")]
use crate::media::engine::MediaEngine;

#[cfg(feature = "agent-execution")]
use super::output_service::OutputService;

const CUSTOM_ENCODING_META_KEY: &str = "custom_encoding";

#[derive(Debug)]
/// Read-only catalog bundle used by agent context routes that need pipelines,
/// outputs, jobs, ingests, and settings in one payload.
pub struct AgentContextCatalog {
    pub pipelines: Vec<Pipeline>,
    pub outputs: Vec<Output>,
    pub jobs: Vec<Job>,
    pub ingests: Vec<Ingest>,
    pub settings: Option<SettingsSnapshot>,
    pub custom_encoding_len: usize,
}

#[derive(Debug, Default)]
/// Smaller read-only catalog used by agent plan/apply flows that only need the
/// pipeline and output inventory.
pub struct AgentPipelineOutputCatalog {
    pub pipelines: Vec<Pipeline>,
    pub outputs: Vec<Output>,
}

#[cfg(feature = "agent-execution")]
pub(crate) enum AgentOutputMutation {
    Create {
        output_id: String,
        name: String,
        url: String,
        monitoring_url: Option<String>,
        desired_state: DesiredOutputState,
        config: OutputConfig,
    },
    Update {
        output_id: String,
        name: String,
        url: String,
        monitoring_url: Option<String>,
        desired_state: DesiredOutputState,
        config: OutputConfig,
    },
    Remove {
        output_id: String,
    },
    SetDesiredState {
        output_id: String,
        desired_state: DesiredOutputState,
    },
}

#[cfg(feature = "agent-execution")]
pub(crate) enum AgentOutputMutationOutcome {
    Created(Output),
    Updated { previous: Output, current: Output },
    Removed(Output),
    DesiredStateUpdated { previous: Output, current: Output },
}

/// Application service for agent catalog reads and validated output mutations.
#[derive(Clone)]
pub struct AgentService {
    pipeline_store: Arc<dyn PipelineStore>,
    output_store: Arc<dyn OutputStore>,
    job_store: Arc<dyn JobStore>,
    ingest_store: Arc<dyn IngestLookup>,
    meta_store: Arc<dyn MetaStore>,
    ingest_host_store: Arc<dyn IngestHostStore>,
    #[cfg(feature = "agent-execution")]
    output_service: OutputService,
}

impl AgentService {
    /// Builds the service from the stores used by agent catalogs and mutations.
    pub fn with_stores(
        pipeline_store: Arc<dyn PipelineStore>,
        output_store: Arc<dyn OutputStore>,
        job_store: Arc<dyn JobStore>,
        ingest_store: Arc<dyn IngestLookup>,
        meta_store: Arc<dyn MetaStore>,
        ingest_host_store: Arc<dyn IngestHostStore>,
    ) -> Self {
        #[cfg(feature = "agent-execution")]
        let output_service = OutputService::with_store(output_store.clone());

        Self {
            pipeline_store,
            output_store,
            job_store,
            ingest_store,
            meta_store,
            ingest_host_store,
            #[cfg(feature = "agent-execution")]
            output_service,
        }
    }

    /// Loads the full read-only catalog used by agent context endpoints,
    /// tolerating individual store failures with empty/default fallbacks.
    pub async fn load_context_catalog(
        &self,
        security: &IngestSecurityService,
    ) -> AgentContextCatalog {
        let pipelines = self
            .pipeline_store
            .list_pipelines()
            .await
            .unwrap_or_default();
        let outputs = self.output_store.list_outputs().await.unwrap_or_default();
        let jobs = self.job_store.list_jobs().await.unwrap_or_default();
        let ingests = self.ingest_store.list_ingests().await.unwrap_or_default();

        let settings = load_settings_snapshot(
            self.meta_store.as_ref(),
            self.ingest_host_store.as_ref(),
            security,
            BackendPolicy::default(),
        )
        .await
        .ok();
        let custom_encoding_len = self
            .meta_store
            .get_meta(CUSTOM_ENCODING_META_KEY)
            .await
            .ok()
            .flatten()
            .map(|value| value.len())
            .unwrap_or(0);

        AgentContextCatalog {
            pipelines,
            outputs,
            jobs,
            ingests,
            settings,
            custom_encoding_len,
        }
    }

    /// Loads the pipeline/output catalog and falls back to an empty catalog if
    /// either store read fails.
    pub async fn load_pipeline_output_catalog(&self) -> AgentPipelineOutputCatalog {
        self.try_load_pipeline_output_catalog()
            .await
            .unwrap_or_default()
    }

    /// Loads the pipeline/output catalog and surfaces store failures as strings
    /// for agent routes that want explicit error handling.
    pub async fn try_load_pipeline_output_catalog(
        &self,
    ) -> Result<AgentPipelineOutputCatalog, String> {
        let pipelines = self
            .pipeline_store
            .list_pipelines()
            .await
            .map_err(|error| format!("failed to list pipelines: {error}"))?;
        let outputs = self
            .output_store
            .list_outputs()
            .await
            .map_err(|error| format!("failed to list outputs: {error}"))?;

        Ok(AgentPipelineOutputCatalog { pipelines, outputs })
    }

    #[cfg(feature = "agent-execution")]
    pub(crate) async fn load_output_for_mutation(
        &self,
        pipeline_id: &str,
        output_id: &str,
    ) -> Result<Output, String> {
        self.output_service
            .get_by_id(pipeline_id, output_id)
            .await
            .map_err(|err| format!("failed to read output: {err}"))
    }

    #[cfg(feature = "agent-execution")]
    pub(crate) async fn apply_output_mutation(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline_id: &str,
        mutation: AgentOutputMutation,
    ) -> Result<AgentOutputMutationOutcome, String> {
        match mutation {
            AgentOutputMutation::Create {
                output_id,
                name,
                url,
                monitoring_url,
                desired_state,
                config,
            } => {
                let output = self
                    .output_service
                    .create_output(
                        &output_id,
                        pipeline_id,
                        &name,
                        &url,
                        monitoring_url.as_deref(),
                        desired_state.as_str(),
                        &config,
                    )
                    .await
                    .map_err(|err| format!("failed to create output: {err}"))?;
                Ok(AgentOutputMutationOutcome::Created(output))
            }
            AgentOutputMutation::Update {
                output_id,
                name,
                url,
                monitoring_url,
                desired_state,
                config,
            } => {
                let previous = self
                    .load_output_for_mutation(pipeline_id, &output_id)
                    .await?;
                let mut current = self
                    .output_service
                    .update_output(
                        pipeline_id,
                        &output_id,
                        &name,
                        &url,
                        monitoring_url.as_deref(),
                        &config,
                    )
                    .await
                    .map_err(|err| format!("failed to update output: {err}"))?;
                if desired_state != previous.desired_state {
                    current = match desired_state {
                        DesiredOutputState::Running => self
                            .output_service
                            .request_start(pipeline_id, &output_id)
                            .await
                            .map_err(|err| format!("failed to update desired state: {err}"))?,
                        DesiredOutputState::Stopped => self
                            .output_service
                            .request_stop(pipeline_id, &output_id)
                            .await
                            .map_err(|err| format!("failed to update desired state: {err}"))?,
                        DesiredOutputState::Failed => {
                            return Err(
                                "agent output updates cannot request failed state".to_string()
                            );
                        }
                    };
                }
                Ok(AgentOutputMutationOutcome::Updated { previous, current })
            }
            AgentOutputMutation::Remove { output_id } => {
                let previous = self
                    .load_output_for_mutation(pipeline_id, &output_id)
                    .await?;
                engine.unregister_egress(&output_id).await;
                let deleted = self
                    .output_service
                    .delete_output(pipeline_id, &output_id)
                    .await
                    .map_err(|err| format!("failed to delete output: {err}"))?;
                if !deleted {
                    return Err(format!(
                        "output '{output_id}' not found on pipeline '{pipeline_id}'"
                    ));
                }
                Ok(AgentOutputMutationOutcome::Removed(previous))
            }
            AgentOutputMutation::SetDesiredState {
                output_id,
                desired_state,
            } => {
                let previous = self
                    .load_output_for_mutation(pipeline_id, &output_id)
                    .await?;
                let current = match desired_state {
                    DesiredOutputState::Running => self
                        .output_service
                        .request_start(pipeline_id, &output_id)
                        .await
                        .map_err(|err| format!("failed to set desired state: {err}"))?,
                    DesiredOutputState::Stopped => self
                        .output_service
                        .request_stop(pipeline_id, &output_id)
                        .await
                        .map_err(|err| format!("failed to set desired state: {err}"))?,
                    DesiredOutputState::Failed => {
                        return Err("agent output actions cannot request failed state".to_string());
                    }
                };
                Ok(AgentOutputMutationOutcome::DesiredStateUpdated { previous, current })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::models::JobStatus;
    use crate::domain::output_spec::OutputConfig;
    use crate::domain::state::DesiredOutputState;
    use crate::infrastructure::service_wiring::SqliteServiceFactory;

    #[tokio::test]
    async fn agent_service_loads_context_catalog_from_storage() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        crate::db::create_pipeline(&pool, "pipe-1", "Pipeline", "key-1", None, None)
            .await
            .unwrap();
        crate::db::create_output(
            &pool,
            "out-1",
            "pipe-1",
            "Output",
            "rtmp://example/live",
            None,
            DesiredOutputState::Running,
            &OutputConfig::source(),
        )
        .await
        .unwrap();
        crate::db::create_job(
            &pool,
            "job-1",
            "pipe-1",
            "out-1",
            Some(123),
            JobStatus::Running,
            "2026-07-09T00:00:00Z",
        )
        .await
        .unwrap();
        crate::db::create_ingest(
            &pool,
            "ing-1",
            "fixture.mp4",
            "key-1",
            false,
            "",
            false,
            crate::application::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
        )
        .await
        .unwrap();
        crate::db::set_meta(&pool, "server_name", "Agent Test")
            .await
            .unwrap();
        crate::db::set_meta(&pool, "custom_encoding", "scale=1280:720")
            .await
            .unwrap();

        let security = IngestSecurityService::new(
            crate::domain::ingest_security::IngestSecurityConfig::default(),
        );
        let service = SqliteServiceFactory::new(&pool).agent_service();
        let catalog = service.load_context_catalog(&security).await;

        assert_eq!(catalog.pipelines.len(), 1);
        assert_eq!(catalog.outputs.len(), 1);
        assert_eq!(catalog.jobs.len(), 1);
        assert_eq!(catalog.ingests.len(), 1);
        assert_eq!(catalog.settings.unwrap().server_name, "Agent Test");
        assert_eq!(catalog.custom_encoding_len, "scale=1280:720".len());
    }

    #[tokio::test]
    async fn agent_service_loads_pipeline_output_catalog() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        crate::db::create_pipeline(&pool, "pipe-1", "Pipeline", "key-1", None, None)
            .await
            .unwrap();
        crate::db::create_output(
            &pool,
            "out-1",
            "pipe-1",
            "Output",
            "rtmp://example/live",
            None,
            DesiredOutputState::Running,
            &OutputConfig::source(),
        )
        .await
        .unwrap();

        let service = SqliteServiceFactory::new(&pool).agent_service();
        let catalog = service.load_pipeline_output_catalog().await;

        assert_eq!(catalog.pipelines.len(), 1);
        assert_eq!(catalog.pipelines[0].id, "pipe-1");
        assert_eq!(catalog.outputs.len(), 1);
        assert_eq!(catalog.outputs[0].id, "out-1");
    }
}
