//! Low-level SRT protocol configuration, caller setup, and metrics.

pub mod address_family;
pub mod calls;
pub mod egress_connect;
pub mod egress_engine;
pub mod egress_stats;
pub mod knobs;

pub use address_family::AddressFamily;
pub use calls::{SrtCallInputs, SrtCallSpec, validate_srt_call_url};
pub use egress_connect::{
    SRT_LATENCY_MS_CEILING, SRT_LATENCY_MS_FLOOR, SrtConnectKind, SrtConnectRequest,
    SrtFabricEgressConnectSpec, validate_srt_egress_url,
};
pub use egress_engine::{MAX_SRT_MESSAGE_PAYLOAD, SrtSendResult};
pub use egress_stats::{SrtSendBacklog, send_backlog};
pub use knobs::{
    DESIRED_UDP_BUF, apply_optional_udp_buf, desired_udp_buf, recv_budget, recv_budget_or,
};
