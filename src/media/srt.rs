//! SRT API surface backed by the external `srt-rs` protocol implementation.

#[path = "srt/shared_muxer.rs"]
mod shared_muxer;
#[path = "srt/egress_engine.rs"]
pub(crate) mod srt_egress_engine;
#[path = "srt_policy.rs"]
mod srt_policy;
mod tokio_egress;
mod tokio_ingress;

pub(crate) use shared_muxer::start_shared_ts_muxer;
pub(crate) use srt_egress_engine::SrtEgressEngine;
pub use srt_policy::{SrtIngestPolicyEntry, SrtIngestPolicyStore};
pub(crate) use tokio_egress::*;
pub(crate) use tokio_ingress::*;

/// Public A/B knob accessors for the test harness (a separate crate).
pub mod srt_knobs {
    pub use super::tokio_egress::{recv_budget, recv_budget_or};
}
pub(crate) fn linked_srt_version() -> String {
    "srt-rs".to_string()
}
