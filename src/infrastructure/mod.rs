//! Application composition, external adapters, and process bootstrap.

pub mod bootstrap;
pub mod notify_transport;
pub mod recording_metadata;
pub(crate) mod restart_backoff;
pub mod service_wiring;
pub(crate) mod supervised_task;
pub mod web_push;
