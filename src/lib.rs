//! Crate root for the restream server.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]
// Raw std lock access is disallowed in shipped code (clippy.toml, `restream_sync::lock`);
// tests may poison and inspect locks directly.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

pub mod api;
pub mod application;
pub mod config;
pub mod db;
pub mod diag;
pub mod host;
pub mod infrastructure;
pub mod logging;

pub use config::{AppConfig, RuntimeTuning, ServerPorts, TokioRuntimeConfig};
pub use host::runtime_info::emit_sbom;
pub use infrastructure::bootstrap::run_app;
