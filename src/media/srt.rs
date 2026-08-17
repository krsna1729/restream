//! Native SRT ingest and egress via raw `libsrt` FFI bindings.

use std::os::fd::RawFd;
#[cfg(test)]
use std::os::raw::{c_int, c_void};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::Ordering;
#[cfg(test)]
use std::time::{Duration, Instant};

#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use tokio::sync::Notify;
use tracing::error;
#[cfg(test)]
use tracing::{info, warn};

use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::PipelineAccessAuthenticator;
#[cfg(test)]
use crate::media::ring_buffer::{MEDIA_PULL_BURST_PACKETS, RingBuffer};
#[cfg(test)]
use crate::media::snapshots::PublisherQuality;
#[cfg(test)]
use crate::media::startup_policy;
#[cfg(test)]
use crate::media::ts_chunk_ring::TsChunkReader;

#[path = "srt/buffer_sizing.rs"]
mod buffer_sizing;
#[path = "srt/ingest.rs"]
mod ingest;
#[path = "srt/ingest_packets.rs"]
mod ingest_packets;
#[path = "srt/listener.rs"]
mod listener;
#[path = "srt/play.rs"]
mod play;
#[path = "srt/shared_muxer.rs"]
mod shared_muxer;
#[path = "srt/socket.rs"]
mod socket;
#[path = "srt/crypto.rs"]
mod srt_crypto;
#[path = "srt/egress_connect.rs"]
mod srt_egress_connect;
#[path = "srt/egress_engine.rs"]
mod srt_egress_engine;
#[cfg(test)]
#[path = "srt/egress_engine_tests.rs"]
mod srt_egress_engine_tests;
#[cfg(test)]
#[path = "srt/egress_fabric_tests.rs"]
mod srt_egress_fabric_tests;
#[path = "srt/egress_poller.rs"]
mod srt_egress_poller;
#[cfg(test)]
#[path = "srt/egress_poller_tests.rs"]
mod srt_egress_poller_tests;
#[path = "srt/egress_sender.rs"]
mod srt_egress_sender;
#[cfg(test)]
#[path = "srt/egress_sender_tests.rs"]
mod srt_egress_sender_tests;
#[path = "srt/egress_socket.rs"]
mod srt_egress_socket;
#[cfg(test)]
#[path = "srt_egress_tests.rs"]
mod srt_egress_tests;
#[path = "srt_monitor.rs"]
mod srt_monitor;
#[path = "srt_policy.rs"]
mod srt_policy;
#[path = "srt_quality.rs"]
mod srt_quality;
#[path = "srt_stream_id.rs"]
mod srt_stream_id;
#[path = "srt/url.rs"]
mod srt_url;
#[path = "srt/sys.rs"]
mod sys;

#[cfg(test)]
use buffer_sizing::srt_set_ingest_latency_opts;
#[cfg(test)]
use shared_muxer::estimate_ts_accum_capacity;
pub(crate) use shared_muxer::start_shared_ts_muxer;
pub(crate) use socket::srt_get_configured_sndbuf;
#[cfg(test)]
use socket::{
    DESIRED_FC, DESIRED_SRT_BUF, SrtGroupSummary, add_srt_group_quality, enable_srt_group_connect,
    is_srt_group, streamid_from_getsockopt_buffer, summarize_group_members,
    try_acquire_srt_sender_permit,
};
pub use socket::{DESIRED_UDP_BUF, linked_srt_version, srt_set_connect_timeout};
use socket::{check_srt_option_result, check_sysctl_limits, srt_log_effective_opts};
#[cfg(test)]
use srt_crypto::apply_srt_crypto_socket;
#[cfg(test)]
use srt_crypto::srt_crypto_from_url;
pub(crate) use srt_egress_connect::{
    SrtFabricEgressConnectConfig, SrtFabricEgressConnectSpec, claim_srt_egress_muxer_port,
    connect_fabric_srt_egress_socket,
};
#[cfg(test)]
use srt_egress_connect::{resolve_host, to_libc_sockaddr};
pub(crate) use srt_egress_engine::SrtEgressEngine;
pub(crate) use srt_egress_poller::{SrtEgressInterest, SrtEgressPollError, SrtReadyLeaf};
pub(crate) use srt_egress_sender::SrtSenderStats;
pub(crate) use srt_egress_sender::{NativeSendBacklog, SrtMessageSender};
pub(crate) use srt_egress_sender::{SrtSendFailure, SrtSendResult};
pub(crate) use srt_egress_socket::{
    SrtEgressSendMode, SrtEgressSocketError, apply_srt_egress_stream_id,
    configure_connected_srt_egress_socket,
};
#[cfg(test)]
use srt_monitor::{monitor_listener_socket, read_udp_socket_stats};
pub use srt_policy::{SrtIngestPolicyEntry, SrtIngestPolicyStore};
#[cfg(test)]
use srt_quality::{SrtCounterSnapshot, quality_from_stats as srt_quality_from_stats};
pub(crate) use srt_quality::{
    SrtSenderCounterSnapshot, sender_quality_from_stats as srt_sender_quality_from_stats,
};
#[cfg(test)]
use srt_stream_id::{SrtConnectionMode, parse_srt_stream_id, percent_decode};
pub(crate) use sys::SrtTraceBStats;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SrtLeafHandle {
    Native(SRTSOCKET),
    Rust(RawFd),
}
use sys::*;
pub use sys::{
    SRTO_ENFORCEDENCRYPTION, SRTO_LATENCY, SRTO_PASSPHRASE, SRTO_PBKEYLEN, SRTSOCKET, sockaddr_in,
    srt_accept, srt_bind, srt_cleanup, srt_close, srt_connect, srt_create_socket,
    srt_getlasterror_str, srt_getsockname, srt_listen, srt_recv, srt_send, srt_setsockopt,
    srt_startup,
};

pub(crate) struct SrtFabricPoller(srt_egress_poller::SrtEgressPoller);

impl SrtFabricPoller {
    pub(crate) fn new(max_events: usize) -> Result<Self, SrtEgressPollError> {
        srt_egress_poller::SrtEgressPoller::new(max_events).map(Self)
    }

    pub(crate) fn register_leaf(
        &mut self,
        handle: SrtLeafHandle,
        key: crate::media::egress::scheduler::LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        let SrtLeafHandle::Native(socket) = handle else {
            return Err(SrtEgressPollError {
                operation: "srt_epoll_register",
                code: -1,
                message: "native SRT poller cannot register a Rust transport handle".into(),
            });
        };
        self.0.register_leaf(socket, key, generation, interest)
    }

    pub(crate) fn remove(&mut self, handle: SrtLeafHandle) -> Result<(), SrtEgressPollError> {
        let SrtLeafHandle::Native(socket) = handle else {
            return Err(SrtEgressPollError {
                operation: "srt_epoll_remove_usock",
                code: -1,
                message: "native SRT poller cannot remove a Rust transport handle".into(),
            });
        };
        self.0.remove(socket)
    }

    pub(crate) fn poll_leaves(
        &mut self,
        timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError> {
        self.0.poll_leaves(timeout_ms, ready)
    }
}

pub(crate) fn srt_fabric_message_sender(socket: SRTSOCKET) -> Box<dyn SrtMessageSender + Send> {
    Box::new(srt_egress_sender::SrtNativeMessageSender::new(socket))
}

#[cfg(test)]
use ingest::{
    EpollWaiterSignal, SRT_INGEST_READINESS_RETRY, SrtReceiveErrorAction,
    classify_srt_receive_error, wait_for_srt_ingest_readiness,
};
#[cfg(test)]
use srt_url::parse_srt_egress_url;

pub struct SrtServer {
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    engine: Arc<MediaEngine>,
    security: Arc<crate::media::security::IngestSecurityService>,
    ingest_policy_store: Arc<SrtIngestPolicyStore>,
}

impl SrtServer {
    pub fn new(
        pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
        engine: Arc<MediaEngine>,
        security: Arc<crate::media::security::IngestSecurityService>,
        ingest_policy_store: Arc<SrtIngestPolicyStore>,
    ) -> Self {
        // SAFETY: this singleton constructor initializes libsrt before any
        // socket owner can be created; teardown remains process-ordered.
        unsafe {
            srt_startup();
            srt_setloglevel(socket::SRT_LOG_CRIT);
        }
        check_sysctl_limits(engine.config.srt_udp_buffer as i32);
        Self {
            pipeline_access,
            engine,
            security,
            ingest_policy_store,
        }
    }
}

impl Drop for SrtServer {
    fn drop(&mut self) {
        // srt_cleanup() is intentionally NOT called here.
        //
        // SrtServer is Arc-owned by a tokio task that may be dropped during
        // runtime shutdown, at which point SRT egress sender OS threads may
        // still hold open SRTSOCKET handles.  Calling srt_cleanup() while live
        // sockets exist violates the libsrt API contract and can produce SIGSEGV
        // or assertion failures inside libsrt.
        //
        // Instead, call crate::media::srt::teardown_srt() explicitly from
        // run_app() AFTER all OS threads have been joined (and therefore all
        // SRT sockets have been closed via srt_close() in their cleanup paths).
    }
}

/// Call srt_cleanup() to release libsrt global state.
///
/// Must be called AFTER all SRT sockets (server + egress) are closed and
/// their OS threads have been joined.  run_app() calls this at the very end
/// of the graceful-shutdown sequence, after drain_os_thread_handles().
// SAFETY: srt_cleanup must be called after all SRT sockets are closed
// and all OS threads using libsrt have been joined. run_app() enforces
// this by calling teardown_srt() as the final step of graceful shutdown.
pub fn teardown_srt() {
    unsafe {
        srt_cleanup();
    }
}

#[cfg(test)]
#[path = "srt_tests.rs"]
mod tests;
