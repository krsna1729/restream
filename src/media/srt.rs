//! SRT API surface backed by the external `srt-rs` protocol implementation.

#[path = "srt/egress_connect.rs"]
pub(crate) mod egress_connect;
#[path = "srt/egress_stats.rs"]
pub(crate) mod egress_stats;
#[path = "srt/ingress_admission.rs"]
mod ingress_admission;
#[path = "srt/ingress_bridge.rs"]
mod ingress_bridge;
#[cfg(test)]
#[path = "srt/ingress_bridge_tests.rs"]
mod ingress_bridge_tests;
#[cfg(test)]
#[path = "srt/ingress_live_tests.rs"]
mod ingress_live_tests;
#[path = "srt/ingress_owner.rs"]
mod ingress_owner;
#[path = "srt/ingress_quality.rs"]
mod ingress_quality;
#[cfg(test)]
#[path = "srt/ingress_test_support.rs"]
mod ingress_test_support;
#[path = "srt/knobs.rs"]
mod knobs;
#[path = "srt/shared_muxer.rs"]
mod shared_muxer;
#[path = "srt/egress_engine.rs"]
pub(crate) mod srt_egress_engine;
#[path = "srt_policy.rs"]
mod srt_policy;
mod tokio_ingress;

pub(crate) use egress_connect::{
    AddressFamily, SrtConnectKind, SrtConnectRequest, SrtFabricEgressConnectSpec,
};
pub(crate) use egress_stats::SrtSendBacklog;
pub(crate) use knobs::{apply_optional_udp_buf, desired_udp_buf};
pub(crate) use shared_muxer::start_shared_ts_muxer;
pub(crate) use srt_egress_engine::{SrtEgressEngine, SrtSendResult};
pub use srt_policy::{SrtIngestPolicyEntry, SrtIngestPolicyStore};
pub(crate) use tokio_ingress::*;

/// Public A/B knob accessors for the test harness (a separate crate).
pub mod srt_knobs {
    pub use super::knobs::{recv_budget, recv_budget_or};
}
pub(crate) fn linked_srt_version() -> String {
    "srt-rs".to_string()
}
