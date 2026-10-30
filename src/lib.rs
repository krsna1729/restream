//! Crate root for the restream server.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]
// Raw std lock access is disallowed in shipped code (clippy.toml, `restream_sync::lock`);
// tests may poison and inspect locks directly.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

// Per-entity fault domains (`panic_boundary`) contain a panic by unwinding.
// With `panic = "abort"` every contained panic would end the process, so
// that configuration must not build. Kani builds abort by design and run no
// boundary: a panic in a proof is a failed property.
#[cfg(all(panic = "abort", not(kani)))]
compile_error!(
    "restream requires panic = \"unwind\": per-connection, per-peer and per-output \
     panic boundaries (src/media/panic_boundary.rs) depend on it"
);

#[cfg(feature = "mcp-http-backend")]
pub mod agent_backends;
#[cfg(any(feature = "agent-plane", feature = "mcp-core"))]
pub mod agent_core;
#[cfg(feature = "agent-execution")]
pub mod agent_execution;
#[cfg(feature = "mcp-core")]
pub mod agent_mcp;
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
pub mod media;
pub mod runtime_info;
pub(crate) mod system_sampling;

pub use config::{AppConfig, RuntimeTuning, ServerPorts, TokioRuntimeConfig};
pub use infrastructure::bootstrap::run_app;
pub use runtime_info::emit_sbom;
