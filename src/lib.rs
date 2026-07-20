//! Crate root for the restream server.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

#[cfg(any(feature = "mcp-http-backend", feature = "mcp-embedded"))]
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
pub mod config;
pub mod db;
pub mod diag;
pub mod domain;
pub mod events;
pub mod ffmpeg_extract;
pub mod infrastructure;
pub mod logging;
pub mod media;
pub mod planner;
pub mod runtime;
pub mod runtime_info;
pub mod secret_display;
pub(crate) mod system_sampling;
pub mod test_fixtures;

pub use config::{AppConfig, RuntimeTuning, ServerPorts, TokioRuntimeConfig};
pub use infrastructure::bootstrap::run_app;
pub use runtime_info::emit_sbom;

/// # Safety
///
/// Exported with the C ABI as a link-time shim for the removed libavcodec
/// symbol. Callers must pass a codec context pointer; the pointer is never
/// dereferenced here, so any value is accepted.
#[cfg(restream_ffmpeg_needs_avcodec_close_shim)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn avcodec_close(
    _ctx: *mut ffmpeg_next::ffi::AVCodecContext,
) -> std::ffi::c_int {
    0
}
