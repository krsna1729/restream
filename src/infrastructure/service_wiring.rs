//! The control plane's composition root.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::api::AppServices;
use crate::application::catalog_auth::CatalogAuth;
use crate::application::pipeline_inputs::PipelineInputService;
use crate::application::recirculation::RecirculationService;
use crate::application::services::{
    AgentService, AuthService, FileIngestService, IngestService, MediaLibraryService,
    NotifyService, PipelineService, SettingsService, TeamService,
};
use crate::db::DeadlineDb;
use crate::infrastructure::notify_transport::HttpNotifyTransport;
use crate::infrastructure::recording_metadata::spawn_recording_metadata_reporter;
use restream_engine::recording::RecordingMetadataReporter;

/// The control plane's one service graph, built once from concrete parts.
///
/// Every service runs on the API executor and shares one ingest admission
/// index, so a catalog write through any of them refreshes the index the
/// ingest listeners read. Admission reloads and the one recording catalog
/// writer run on the lifecycle executor (production: the lifecycle pool,
/// which API load cannot exhaust).
pub struct ControlPlane {
    pub services: AppServices,
    /// Starts empty; startup fills it with [`CatalogAuth::load`] before any
    /// listener starts.
    pub catalog_auth: Arc<CatalogAuth>,
    /// The recording catalog writer the media library and housekeeping
    /// report to.
    pub recording_metadata: RecordingMetadataReporter,
}

impl ControlPlane {
    pub fn new(db: DeadlineDb, lifecycle: DeadlineDb) -> Self {
        let catalog_auth = CatalogAuth::new(lifecycle.clone());
        let recording_metadata = spawn_recording_metadata_reporter(lifecycle);
        let pipeline_service = PipelineService::new(db.clone(), catalog_auth.clone());
        let pipeline_input_service =
            PipelineInputService::new(db.clone(), pipeline_service.clone(), catalog_auth.clone());
        let ingest_service = IngestService::new(db.clone());
        let services = AppServices {
            recirculation_service: RecirculationService::with_services(
                db.clone(),
                pipeline_input_service.clone(),
            ),
            pipeline_input_service,
            auth_service: AuthService::new(db.clone()),
            team_service: TeamService::new(db.clone()),
            notify_service: NotifyService::new(
                db.clone(),
                Arc::new(HttpNotifyTransport::new(db.clone())),
            ),
            settings_service: SettingsService::new(db.clone(), pipeline_service.clone()),
            file_ingest_service: FileIngestService::new(db.clone(), pipeline_service.clone()),
            media_library_service: MediaLibraryService::new(
                db.clone(),
                pipeline_service.clone(),
                ingest_service.clone(),
            )
            .with_recording_metadata(recording_metadata.clone()),
            agent_service: AgentService::new(db.clone()),
            pipeline_service,
            ingest_service,
            db,
        };
        Self {
            services,
            catalog_auth,
            recording_metadata,
        }
    }

    /// Both executors on one pool, the lifecycle side under the shared-loop
    /// deadline: the single-database graph tests and benches run.
    pub fn on_pool(pool: &SqlitePool) -> Self {
        Self::new(
            DeadlineDb::api(pool.clone()),
            DeadlineDb::new(pool.clone(), crate::db::SHARED_LOOP_DEADLINE),
        )
    }
}
