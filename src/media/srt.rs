//! Native SRT ingest and egress via raw `libsrt` FFI bindings.

use std::os::raw::{c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use tokio::sync::Notify;
use tracing::{error, info, warn};

use crate::domain::state::EgressPhase;
use crate::media::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::media::engine::{EgressRegistration, MediaEngine};
use crate::media::ingest_auth::PipelineAccessAuthenticator;
use crate::media::ring_buffer::{MEDIA_PULL_BURST_PACKETS, RingBuffer};
use crate::media::snapshots::PublisherQuality;
use crate::media::startup_policy;
use crate::media::ts_chunk_ring::TsChunkReader;

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
#[path = "srt_egress.rs"]
mod srt_egress;
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
use shared_muxer::estimate_ts_accum_capacity;
pub(crate) use shared_muxer::start_shared_ts_muxer;
pub use socket::{DESIRED_UDP_BUF, linked_srt_version, srt_set_connect_timeout};
#[cfg(test)]
use socket::{
    SrtGroupSummary, enable_srt_group_connect, is_srt_group, streamid_from_getsockopt_buffer,
    summarize_group_members,
};
use socket::{
    add_srt_group_quality, check_srt_option_result, check_sysctl_limits, srt_group_summary,
    srt_log_effective_opts, srt_set_highbitrate_opts, to_sockaddr_in,
    try_acquire_srt_sender_permit,
};
#[cfg(test)]
use srt_crypto::apply_srt_crypto_socket;
#[cfg(test)]
use srt_crypto::srt_crypto_from_url;
pub use srt_egress::start_srt_egress;
pub(crate) use srt_egress_connect::{
    SrtEgressMuxerPortClaim, bind_srt_egress_muxer_port, claim_srt_egress_muxer_port,
    connected_srt_local_port, resolve_host as resolve_srt_egress_host, set_srt_reuseaddr,
    to_libc_sockaddr,
};
pub(crate) use srt_egress_engine::SrtEgressEngine;
pub(crate) use srt_egress_poller::{SrtEgressInterest, SrtEgressPollError, SrtReadyLeaf};
pub(crate) use srt_egress_sender::SrtMessageSender;
#[cfg(test)]
pub(crate) use srt_egress_sender::SrtSendResult;
pub(crate) use srt_egress_socket::{
    SrtEgressSendMode, SrtEgressSocketError, configure_connected_srt_egress_socket,
};
#[cfg(test)]
use srt_monitor::{audio_codec_id, monitor_listener_socket, read_udp_socket_stats, video_codec_id};
pub use srt_policy::{SrtIngestPolicyEntry, SrtIngestPolicyStore};
#[cfg(test)]
use srt_quality::{SrtCounterSnapshot, quality_from_stats as srt_quality_from_stats};
use srt_quality::{
    SrtSenderCounterSnapshot, sender_quality_from_stats as srt_sender_quality_from_stats,
};
#[cfg(test)]
use srt_stream_id::{SrtConnectionMode, parse_srt_stream_id, percent_decode};
pub(crate) use sys::SrtTraceBStats;
use sys::*;
pub use sys::{
    SRTO_ENFORCEDENCRYPTION, SRTO_LATENCY, SRTO_PASSPHRASE, SRTO_PBKEYLEN, SRTSOCKET, sockaddr_in,
    srt_accept, srt_bind, srt_cleanup, srt_close, srt_connect, srt_create_socket,
    srt_getlasterror_str, srt_getsockname, srt_listen, srt_recv, srt_send, srt_setsockopt,
    srt_startup,
};

pub(crate) struct SrtFabricPoller(srt_egress_poller::SrtEgressPoller);

impl SrtFabricPoller {
    #[allow(dead_code)]
    pub(crate) fn new(max_events: usize) -> Result<Self, SrtEgressPollError> {
        srt_egress_poller::SrtEgressPoller::new(max_events).map(Self)
    }

    pub(crate) fn register_leaf(
        &mut self,
        socket: SRTSOCKET,
        key: crate::media::egress::scheduler::LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        self.0.register_leaf(socket, key, generation, interest)
    }

    pub(crate) fn remove(&mut self, socket: SRTSOCKET) -> Result<(), SrtEgressPollError> {
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
        check_sysctl_limits();
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
