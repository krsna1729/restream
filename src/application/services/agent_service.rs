use std::sync::Arc;

use crate::application::models::{Ingest, Job, Output, Pipeline};
use crate::application::ports::{
    IngestHostStore, IngestLookup, JobStore, MetaStore, OutputStore, PipelineStore,
};
use crate::application::settings::{SettingsSnapshot, load_settings_snapshot};
use crate::media::security::IngestSecurityService;
use crate::planner::backend_policy::BackendPolicy;

#[derive(Debug)]
pub struct AgentContextCatalog {
    pub pipelines: Vec<Pipeline>,
    pub outputs: Vec<Output>,
    pub jobs: Vec<Job>,
    pub ingests: Vec<Ingest>,
    pub settings: Option<SettingsSnapshot>,
    pub custom_encoding_len: usize,
}

#[derive(Debug)]
pub struct AgentPipelineOutputCatalog {
    pub pipelines: Vec<Pipeline>,
    pub outputs: Vec<Output>,
}

/// Application service for read-only agent context assembly.
///
/// The API layer still owns response shaping and redaction, but persistence and
/// cross-source settings reads live here instead of in the handler module.
#[derive(Clone)]
pub struct AgentService {
    pipeline_store: Arc<dyn PipelineStore>,
    output_store: Arc<dyn OutputStore>,
    job_store: Arc<dyn JobStore>,
    ingest_store: Arc<dyn IngestLookup>,
    meta_store: Arc<dyn MetaStore>,
    ingest_host_store: Arc<dyn IngestHostStore>,
}

impl AgentService {
    pub fn with_stores(
        pipeline_store: Arc<dyn PipelineStore>,
        output_store: Arc<dyn OutputStore>,
        job_store: Arc<dyn JobStore>,
        ingest_store: Arc<dyn IngestLookup>,
        meta_store: Arc<dyn MetaStore>,
        ingest_host_store: Arc<dyn IngestHostStore>,
    ) -> Self {
        Self {
            pipeline_store,
            output_store,
            job_store,
            ingest_store,
            meta_store,
            ingest_host_store,
        }
    }

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
            .get_meta("custom_encoding")
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

    pub async fn load_pipeline_output_catalog(&self) -> AgentPipelineOutputCatalog {
        self.try_load_pipeline_output_catalog()
            .await
            .unwrap_or_else(|_| AgentPipelineOutputCatalog {
                pipelines: Vec::new(),
                outputs: Vec::new(),
            })
    }

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::models::JobStatus;
    use crate::domain::output_spec::OutputConfig;
    use crate::domain::state::DesiredOutputState;

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
            &OutputConfig::parse("source"),
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
        let service = AgentService::new(pool);
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
            &OutputConfig::parse("source"),
        )
        .await
        .unwrap();

        let service = AgentService::new(pool);
        let catalog = service.load_pipeline_output_catalog().await;

        assert_eq!(catalog.pipelines.len(), 1);
        assert_eq!(catalog.pipelines[0].id, "pipe-1");
        assert_eq!(catalog.outputs.len(), 1);
        assert_eq!(catalog.outputs[0].id, "out-1");
    }
}
