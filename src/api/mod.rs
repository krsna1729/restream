//! Dashboard API surface modules.
//!
//! Each submodule owns a small transport boundary: request validation, auth
//! gating, and response shaping before control passes into application or
//! runtime services.

pub mod agent;
pub mod alerts;
pub mod auth;
pub mod file_ingest;
pub mod health;
pub mod hls;
pub mod ingests;
pub mod logs;
pub mod media_library;
pub mod outputs;
pub mod pipeline_inputs;
pub mod pipelines;
pub mod router;
pub mod settings;
pub mod state;
pub mod static_assets;
pub mod telemetry;

pub use auth::{initialize_auth, initialize_auth_for_test};
pub use router::create_router;
pub use state::{AppState, PortConfig};
pub use static_assets::EmbeddedAssets;
