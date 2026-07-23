//! SQLite-backed application-service composition.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::api::AppServices;
use crate::application::pipeline_inputs::PipelineInputService;
use crate::application::recirculation::RecirculationService;
use crate::application::services::{
    AgentService, AuthService, FileIngestService, HealthService, IngestService, LogService,
    MediaLibraryService, OutputService, PipelineService, SettingsService,
};
use crate::infrastructure::pipeline_input_store::SqlitePipelineInputStore;
use crate::infrastructure::recording_metadata::spawn_recording_metadata_reporter;
use crate::infrastructure::sqlite_ports::{
    SqliteIngestLookup, SqliteJobStore, SqliteLogStore, SqliteMetaStore, SqliteOutputStore,
    SqlitePipelineStore, SqliteRecordingStore, SqliteSessionStore,
};

/// Infrastructure-owned factory for SQLite application-port adapters.
pub struct SqliteServiceFactory<'pool> {
    db: &'pool SqlitePool,
}

impl<'pool> SqliteServiceFactory<'pool> {
    pub const fn new(db: &'pool SqlitePool) -> Self {
        Self { db }
    }

    /// Builds the complete service graph used by the API state.
    pub fn compose(&self) -> AppServices {
        let pipeline_service = self.pipeline_service();
        let output_service = self.output_service();
        let pipeline_input_service = self.pipeline_input_service(pipeline_service.clone());
        let recirculation_service = RecirculationService::with_services(
            output_service.clone(),
            pipeline_input_service.clone(),
        );
        let ingest_service = self.ingest_service();
        let settings_service =
            self.settings_service_with(pipeline_service.clone(), output_service.clone());

        AppServices {
            pipeline_input_service,
            recirculation_service,
            auth_service: self.auth_service(),
            settings_service,
            health_service: self.health_service(),
            file_ingest_service: self.file_ingest_service(pipeline_service.clone()),
            media_library_service: self
                .media_library_service(pipeline_service.clone(), ingest_service.clone()),
            log_service: self.log_service(),
            agent_service: self.agent_service(),
            pipeline_service,
            output_service,
            ingest_service,
        }
    }

    pub fn pipeline_service(&self) -> PipelineService {
        PipelineService::with_store(Arc::new(SqlitePipelineStore::new(self.db.clone())))
    }

    pub fn pipeline_input_service(&self, pipelines: PipelineService) -> PipelineInputService {
        PipelineInputService::with_store(
            Arc::new(SqlitePipelineInputStore::new(self.db.clone())),
            pipelines,
        )
    }

    pub fn output_service(&self) -> OutputService {
        OutputService::with_store(Arc::new(SqliteOutputStore::new(self.db.clone())))
    }

    pub fn ingest_service(&self) -> IngestService {
        let store = Arc::new(SqliteIngestLookup::new(self.db.clone()));
        IngestService::with_ports(store.clone(), store)
    }

    pub fn auth_service(&self) -> AuthService {
        let meta_store = Arc::new(SqliteMetaStore::new(self.db.clone()));
        AuthService::with_stores(
            meta_store.clone(),
            meta_store,
            Arc::new(SqliteSessionStore::new(self.db.clone())),
        )
    }

    pub fn settings_service(&self) -> SettingsService {
        self.settings_service_with(self.pipeline_service(), self.output_service())
    }

    fn settings_service_with(
        &self,
        pipeline_service: PipelineService,
        output_service: OutputService,
    ) -> SettingsService {
        let meta_store = Arc::new(SqliteMetaStore::new(self.db.clone()));
        SettingsService::with_stores(
            meta_store.clone(),
            meta_store.clone(),
            meta_store,
            Arc::new(SqliteJobStore::new(self.db.clone())),
            Arc::new(SqlitePipelineInputStore::new(self.db.clone())),
            pipeline_service,
            output_service,
        )
    }

    pub fn health_service(&self) -> HealthService {
        HealthService::with_store(Arc::new(SqlitePipelineStore::new(self.db.clone())))
    }

    pub fn file_ingest_service(&self, pipeline_service: PipelineService) -> FileIngestService {
        let ingest_store = Arc::new(SqliteIngestLookup::new(self.db.clone()));
        FileIngestService::with_ports(
            ingest_store.clone(),
            ingest_store,
            Arc::new(SqlitePipelineStore::new(self.db.clone())),
            Arc::new(SqlitePipelineInputStore::new(self.db.clone())),
            pipeline_service,
        )
    }

    pub fn media_library_service(
        &self,
        pipeline_service: PipelineService,
        ingest_service: IngestService,
    ) -> MediaLibraryService {
        let meta_store = Arc::new(SqliteMetaStore::new(self.db.clone()));
        MediaLibraryService::with_stores(
            meta_store.clone(),
            meta_store,
            Arc::new(SqliteRecordingStore::new(self.db.clone())),
            pipeline_service,
            ingest_service,
        )
        .with_recording_metadata(spawn_recording_metadata_reporter(self.db.clone()))
    }

    pub fn log_service(&self) -> LogService {
        LogService::with_store(Arc::new(SqliteLogStore::new(self.db.clone())))
    }

    pub fn agent_service(&self) -> AgentService {
        let meta_store = Arc::new(SqliteMetaStore::new(self.db.clone()));
        AgentService::with_stores(
            Arc::new(SqlitePipelineStore::new(self.db.clone())),
            Arc::new(SqliteOutputStore::new(self.db.clone())),
            Arc::new(SqliteJobStore::new(self.db.clone())),
            Arc::new(SqliteIngestLookup::new(self.db.clone())),
            meta_store.clone(),
            meta_store,
        )
    }
}
