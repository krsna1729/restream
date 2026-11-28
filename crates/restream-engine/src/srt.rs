//! SRT API surface backed by the external `srt-rs` protocol implementation.

#[doc(hidden)]
pub mod bench_entry;
mod calls;
#[path = "srt/ingress_admission.rs"]
mod ingress_admission;
#[path = "srt/ingress_bridge.rs"]
mod ingress_bridge;
mod ingress_media;
mod ingress_owner;
#[path = "srt/ingress_quality.rs"]
mod ingress_quality;
#[cfg(test)]
#[path = "srt/ingress_test_support.rs"]
pub(crate) mod ingress_test_support;
mod ingress_types;
#[path = "srt_policy.rs"]
mod srt_policy;
#[cfg(test)]
mod tests;
mod tokio_ingress;

pub(crate) use ingress_media::ingest_packets::forward_ingest_packets;
pub use srt_policy::{SrtIngestPolicyEntry, SrtIngestPolicyStore};
pub use tokio_ingress::*;

/// Public A/B knob accessors for the test harness (a separate crate).
pub mod srt_knobs {
    pub use crate::egress::backends::srt::knobs::{recv_budget, recv_budget_or};
}

/// The production SRT Owner runtime, datagram ceiling and listener shape, so
/// the test harness's SRT sink (a separate crate) runs the same Owner path as
/// ingress.
pub mod srt_owner_runtime {
    pub use super::ingress_owner::listener_shape;
    pub use crate::egress::backends::srt::owner_set::{SRT_OWNER_WIRE_CEILING, production_runtime};
}

pub fn linked_srt_version() -> String {
    "srt-rs".to_string()
}
