//! Application service layer — typed use-case wrappers around persistence.
//!
//! Handlers should validate HTTP input, call these services, and serialize
//! responses. Services own orchestration logic that is not HTTP-specific.

pub mod agent_service;
pub mod auth_service;
pub mod error;
pub mod file_ingest_service;
pub mod ingest_service;
pub mod media_library_service;
pub mod notify_service;
pub mod pipeline_service;
pub mod settings_service;
pub mod team_service;

pub use agent_service::AgentService;
pub use auth_service::AuthService;
pub use error::{ServiceError, ServiceResult};
pub use file_ingest_service::FileIngestService;
pub use ingest_service::IngestService;
pub use media_library_service::MediaLibraryService;
pub use notify_service::NotifyService;
pub use pipeline_service::{PipelineService, ingest_host_or_default};
pub use settings_service::SettingsService;
pub use team_service::TeamService;
