//! Dashboard API surface modules.
//!
//! Each submodule owns a small transport boundary: request validation, auth
//! gating, and response shaping before control passes into application or
//! runtime services.

use crate::application::pipeline_inputs::PipelineInputService;
use crate::application::recirculation::RecirculationService;
use crate::application::services::{
    AgentService, AuthService, FileIngestService, HealthService, IngestService, LogService,
    MediaLibraryService, OutputService, PipelineService, SettingsService,
};

pub mod agent;
pub mod alerts;
pub mod auth;
pub mod error;
pub mod file_ingest;
pub mod health;
pub mod hls;
pub mod ingests;
pub mod logs;
pub mod media_library;
pub mod outputs;
pub mod pipeline_inputs;
pub mod pipeline_observability;
pub mod pipelines;
pub mod router;
pub mod settings;
pub mod state;
pub mod static_assets;
pub mod telemetry;

/// Storage-neutral application services consumed by the HTTP boundary.
pub struct AppServices {
    pub pipeline_service: PipelineService,
    pub pipeline_input_service: PipelineInputService,
    pub recirculation_service: RecirculationService,
    pub output_service: OutputService,
    pub ingest_service: IngestService,
    pub auth_service: AuthService,
    pub settings_service: SettingsService,
    pub health_service: HealthService,
    pub file_ingest_service: FileIngestService,
    pub media_library_service: MediaLibraryService,
    pub log_service: LogService,
    pub agent_service: AgentService,
}

pub use router::create_router;
pub use state::{AppState, PortConfig};
pub use static_assets::EmbeddedAssets;
