//! SRT API surface backed by the external `srt-rs` protocol implementation.

#[path = "srt/calls.rs"]
mod calls;
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
#[path = "srt/ingress_media.rs"]
mod ingress_media;
#[path = "srt/ingress_owner.rs"]
mod ingress_owner;
#[path = "srt/ingress_quality.rs"]
mod ingress_quality;
#[cfg(test)]
#[path = "srt/ingress_test_support.rs"]
pub(crate) mod ingress_test_support;
#[path = "srt/knobs.rs"]
mod knobs;
#[path = "srt/shared_muxer.rs"]
mod shared_muxer;
#[path = "srt/egress_engine.rs"]
pub(crate) mod srt_egress_engine;
#[path = "srt_policy.rs"]
mod srt_policy;
mod tokio_ingress;

pub(crate) use calls::validate_srt_call_url;
pub use calls::{SrtCallInputs, SrtCallSpec};
pub(crate) use egress_connect::{
    AddressFamily, SrtConnectKind, SrtConnectRequest, SrtFabricEgressConnectSpec,
    validate_srt_egress_url,
};
pub(crate) use egress_stats::SrtSendBacklog;
pub(crate) use ingress_media::ingest_packets::forward_ingest_packets;
pub(crate) use knobs::desired_udp_buf;
pub(crate) use shared_muxer::start_shared_ts_muxer;
pub(crate) use srt_egress_engine::{SrtEgressEngine, SrtSendResult};
pub use srt_policy::{SrtIngestPolicyEntry, SrtIngestPolicyStore};
pub(crate) use tokio_ingress::*;

/// Public A/B knob accessors for the test harness (a separate crate).
pub mod srt_knobs {
    pub use super::knobs::{recv_budget, recv_budget_or};
}

/// The production SRT Owner runtime, datagram ceiling and listener shape, so
/// the test harness's SRT sink (a separate crate) runs the same Owner path as
/// ingress.
pub mod srt_owner_runtime {
    pub use super::ingress_owner::listener_shape;
    pub use crate::media::egress::backends::srt::owner_set::{
        SRT_OWNER_WIRE_CEILING, production_runtime,
    };
}
/// `current - previous` as a per-second rate, or `None` when the interval is
/// empty or the counter moved backwards (a reset is not a zero rate). Shared
/// by the ingress quality and egress sender statistics.
fn per_second(current: u64, previous: u64, interval: std::time::Duration) -> Option<f64> {
    let seconds = interval.as_secs_f64();
    (seconds > 0.0 && current >= previous).then(|| (current - previous) as f64 / seconds)
}

pub(crate) fn linked_srt_version() -> String {
    "srt-rs".to_string()
}
