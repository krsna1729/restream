//! Application service layer — typed use-case wrappers around persistence.
//!
//! Handlers should validate HTTP input, call these services, and serialize
//! responses. Services own orchestration logic that is not HTTP-specific.

pub mod error;
pub mod file_ingest_service;
pub mod health_service;
pub mod ingest_service;
pub mod media_library_service;
pub mod output_service;
pub mod pipeline_service;
pub mod settings_service;

pub use error::{ApiError, ApiResult};
pub use file_ingest_service::FileIngestService;
pub use health_service::HealthService;
pub use ingest_service::IngestService;
pub use media_library_service::MediaLibraryService;
pub use output_service::OutputService;
pub use pipeline_service::PipelineService;
pub use settings_service::SettingsService;
