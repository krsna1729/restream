//! Crate root for the restream server.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]
// Raw std lock access is disallowed in shipped code (clippy.toml, `restream_sync::lock`);
// tests may poison and inspect locks directly.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

#[cfg(feature = "agent-execution")]
pub mod agent_execution;
#[cfg(feature = "agent-plane")]
pub mod agent_plane;
pub mod alerts;
pub mod api;
pub(crate) mod api_runtime_views;
pub mod api_view_models;
pub mod application;
pub mod capacity;
pub mod config;
pub mod db;
pub mod diag;
pub mod host_tuning;
pub mod infrastructure;
pub mod logging;
pub mod malloc_tuning;
pub mod runtime_info;
pub(crate) mod system_sampling;

pub use config::{AppConfig, RuntimeTuning, ServerPorts, TokioRuntimeConfig};
pub use infrastructure::bootstrap::run_app;
pub use runtime_info::emit_sbom;
