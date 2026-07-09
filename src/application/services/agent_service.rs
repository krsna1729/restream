use crate::application::ports::SqliteMetaStore;
use crate::application::settings::{SettingsSnapshot, load_settings_snapshot};
use crate::media::security::IngestSecurityService;
use crate::types::{Ingest, Job, Output, Pipeline};
use sqlx::SqlitePool;

#[derive(Debug)]
pub struct AgentContextCatalog {
    pub pipelines: Vec<Pipeline>,
    pub outputs: Vec<Output>,
    pub jobs: Vec<Job>,
    pub ingests: Vec<Ingest>,
    pub settings: Option<SettingsSnapshot>,
    pub custom_encoding_len: usize,
}

/// Application service for read-only agent context assembly.
///
/// The API layer still owns response shaping and redaction, but persistence and
/// cross-source settings reads live here instead of in the handler module.
#[derive(Clone)]
pub struct AgentService {
    db: SqlitePool,
}

impl AgentService {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    pub async fn load_context_catalog(
        &self,
        security: &IngestSecurityService,
    ) -> AgentContextCatalog {
        let pipelines = crate::db::list_pipelines(&self.db)
            .await
            .unwrap_or_default();
        let outputs = crate::db::list_outputs(&self.db).await.unwrap_or_default();
        let jobs = crate::db::list_jobs(&self.db).await.unwrap_or_default();
        let ingests = crate::db::list_ingests(&self.db).await.unwrap_or_default();

        let settings_store = SqliteMetaStore::new(self.db.clone());
        let settings = load_settings_snapshot(&settings_store, &settings_store, security)
            .await
            .ok();
        let custom_encoding_len = crate::db::get_meta(&self.db, "custom_encoding")
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::output_spec::OutputConfig;
    use crate::domain::state::DesiredOutputState;
    use crate::types::JobStatus;

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
            crate::types::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
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
}
