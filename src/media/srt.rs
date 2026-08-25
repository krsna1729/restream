//! SRT API surface backed by the external `srt-rs` protocol implementation.

#[path = "rust_srt.rs"]
mod rust_srt;
#[path = "srt/shared_muxer.rs"]
mod shared_muxer;
#[path = "srt_policy.rs"]
mod srt_policy;

pub(crate) use rust_srt::SrtFabricPoller;
pub(crate) use rust_srt::*;
pub(crate) use shared_muxer::start_shared_ts_muxer;
pub use srt_policy::{SrtIngestPolicyEntry, SrtIngestPolicyStore};
pub(crate) fn teardown_srt() {}
pub(crate) fn linked_srt_version() -> String {
    "srt-rs".to_string()
}
pub(crate) fn srt_get_configured_sndbuf(_socket: SRTSOCKET) -> i32 {
    0
}
pub(crate) use rust_srt::configure_connected_srt_egress_socket;
