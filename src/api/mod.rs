//! Dashboard API surface modules.
//!
//! Each submodule owns a small transport boundary: request validation, auth
//! gating, and response shaping before control passes into application or
//! runtime services.

use crate::application::pipeline_inputs::PipelineInputService;
use crate::application::recirculation::RecirculationService;
use crate::application::services::{
    AgentService, AuthService, FileIngestService, IngestService, MediaLibraryService,
    NotifyService, PipelineService, SettingsService, TeamService,
};

pub mod agent;
pub mod alerts;
pub mod auth;
pub mod error;
pub mod file_ingest;
pub mod fleet_history;
pub mod health;
pub mod hls;
pub mod hls_ingest;
pub mod http_server;
pub mod ingests;
pub mod logs;
pub mod media_library;
pub mod notifications;
pub mod outputs;
pub mod pipeline_inputs;
pub mod pipeline_observability;
pub mod pipelines;
pub mod router;
pub mod runtime_state;
pub mod schedules;
pub mod settings;
pub mod state;
pub mod static_assets;
pub mod team;
pub mod telemetry;

/// Storage-neutral application services consumed by the HTTP boundary.
pub struct AppServices {
    pub pipeline_service: PipelineService,
    pub pipeline_input_service: PipelineInputService,
    pub recirculation_service: RecirculationService,
    pub ingest_service: IngestService,
    pub auth_service: AuthService,
    pub team_service: TeamService,
    pub notify_service: NotifyService,
    pub settings_service: SettingsService,
    pub file_ingest_service: FileIngestService,
    pub media_library_service: MediaLibraryService,
    pub agent_service: AgentService,
}

pub use router::{create_hls_ingest_router, create_router};
pub use state::{AppState, PortConfig};
pub use static_assets::EmbeddedAssets;
