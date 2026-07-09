//! Native SRT ingest and egress via raw `libsrt` FFI bindings.
//!
//! Ingest: SRT listener accepts connections, reads `streamid` for authentication,
//! pipes MPEG-TS data into a `MemoryQueue`, and runs an FFmpeg demuxer on a
//! dedicated OS thread (wrapped in `catch_unwind`). The demuxer publishes ALL
//! video and audio streams (not just "best") into the `RingBuffer` with per-track
//! indices for multi-track audio support. The listener has `SRTO_GROUPCONNECT=1`
//! enabled, so bonded ingest connections from encoders that support SRT bonding
//! (e.g., Haivision, srt-live-transmit) are accepted transparently.
//!
//! Egress: connects to an SRT target via `srt_connect` (single link) or
//! `srt_connect_group` (bonded backup, when `bond=` URL parameter is present).
//! MPEG-TS muxing is deferred until ingest metadata is available to avoid
//! "no streams to mux" errors when the egress starts before ingest.
//!
//! # Socket Sizing
//!
//! All sockets (listener, accepted, egress) get high-bitrate tuning via
//! `srt_set_highbitrate_opts`: 12 MB send/recv buffers (vs. default ~1.5 MB),
//! 32768-packet flow control window (vs. default 8192), unlimited max bandwidth.
//! These values accommodate 4K 60fps H.264 streams at 50 Mbps peak with
//! headroom for retransmission bursts on lossy links.
//!
//! # libsrt FFI safety contract
//!
//! All unsafe blocks in this file call into libsrt's C API. Every call site
//! upholds these invariants:
//!
//! 1. `srt_startup()` is called once before any other SRT function.
//! 2. `srt_cleanup()` is called once after all sockets are closed.
//! 3. Every `srt_create_socket()` is balanced by exactly one `srt_close()`.
//!    `SrtSockGuard` provides RAII cleanup for the listener; ingest/egress
//!    sockets are closed on all error and success paths.
//! 4. `srt_setsockopt`/`srt_getsockopt` receive correctly-sized option values
//!    with valid pointers to live stack variables.
//! 5. `srt_send`/`srt_recv` buffers are valid, sized `Vec<u8>` with matching
//!    capacity arguments.
//! 6. `srt_epoll_*` functions are used in matched create/add/remove/release
//!    pairs; the epoll instance outlives all registered sockets.
//! 7. `CStr::from_ptr(srt_getlasterror_str())` returns a thread-local static
//!    string valid until the next SRT call on the same thread.
//! 8. `std::mem::zeroed()` initializes FFI structs (`SrtSocketGroupData`,
//!    `SrtTraceBStats`, `sockaddr_storage`) before the kernel/lib fills them.
//! 9. `srt_bistats` receives a pointer to a correctly-sized `SrtTraceBStats`.
//! 10. Raw pointer writes to `sockaddr` fields target correctly-typed pointers
//!     obtained from a `sockaddr_storage` cast, with the family field set first.

use std::net::SocketAddr;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::application::ingest::authenticate_srt_stream_key;
use crate::application::ports::PipelineStore;
use crate::domain::srt_ingest::{ResolvedSrtIngestConfig, SrtPipelineIngestConfig};
use crate::domain::state::EgressPhase;
use crate::media::engine::{EgressRegistration, MediaEngine, PublisherQuality};
use crate::media::ring_buffer::{MediaPacket, MediaType, Reader, RingBuffer};
use crate::media::startup_policy;
use crate::media::ts_chunk_ring::{TsChunkReader, TsChunkRing};
use crate::media::{MEDIA_PULL_BURST_PACKETS, MEDIA_TS_BATCH_TARGET_BYTES};

// 256 slots covers the mux wakeup → SRT socket-write latency (sub-millisecond
// to single-digit milliseconds in practice). The SRT protocol's own send buffer
// (~12 MB at 250 ms latency × 8 Mb/s) is the actual jitter absorber; this ring
// only bridges the gap between the muxer thread and the SRT socket write.
// At ~400 chunks/s for an 8 Mb/s stream, 256 slots ≈ 640 ms of absorption.
#[path = "srt_policy.rs"]
mod srt_policy;
pub use srt_policy::SrtIngestPolicyStore;

// Raw SRT Types & FFI Bindings
pub type SRTSOCKET = c_int;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct sockaddr_in {
    pub sin_family: u16,
    pub sin_port: u16,
    pub sin_addr: u32,
    pub sin_zero: [u8; 8],
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct SrtTraceBStats {
    pub ms_time_stamp: i64,
    pub pkt_sent_total: i64,
    pub pkt_recv_total: i64,
    pub pkt_snd_loss_total: c_int,
    pub pkt_rcv_loss_total: c_int,
    pub pkt_retrans_total: c_int,
    pub pkt_sent_ack_total: c_int,
    pub pkt_recv_ack_total: c_int,
    pub pkt_sent_nak_total: c_int,
    pub pkt_recv_nak_total: c_int,
    pub us_snd_duration_total: i64,
    pub pkt_snd_drop_total: c_int,
    pub pkt_rcv_drop_total: c_int,
    pub pkt_rcv_undecrypt_total: c_int,
    pub byte_sent_total: u64,
    pub byte_recv_total: u64,
    pub byte_rcv_loss_total: u64,
    pub byte_retrans_total: u64,
    pub byte_snd_drop_total: u64,
    pub byte_rcv_drop_total: u64,
    pub byte_rcv_undecrypt_total: u64,
    pub pkt_sent: i64,
    pub pkt_recv: i64,
    pub pkt_snd_loss: c_int,
    pub pkt_rcv_loss: c_int,
    pub pkt_retrans: c_int,
    pub pkt_rcv_retrans: c_int,
    pub pkt_sent_ack: c_int,
    pub pkt_recv_ack: c_int,
    pub pkt_sent_nak: c_int,
    pub pkt_recv_nak: c_int,
    pub mbps_send_rate: f64,
    pub mbps_recv_rate: f64,
    pub us_snd_duration: i64,
    pub pkt_reorder_distance: c_int,
    pub pkt_rcv_avg_belated_time: f64,
    pub pkt_rcv_belated: i64,
    pub pkt_snd_drop: c_int,
    pub pkt_rcv_drop: c_int,
    pub pkt_rcv_undecrypt: c_int,
    pub byte_sent: u64,
    pub byte_recv: u64,
    pub byte_rcv_loss: u64,
    pub byte_retrans: u64,
    pub byte_snd_drop: u64,
    pub byte_rcv_drop: u64,
    pub byte_rcv_undecrypt: u64,
    pub us_pkt_snd_period: f64,
    pub pkt_flow_window: c_int,
    pub pkt_congestion_window: c_int,
    pub pkt_flight_size: c_int,
    pub ms_rtt: f64,
    pub mbps_bandwidth: f64,
    pub byte_avail_snd_buf: c_int,
    pub byte_avail_rcv_buf: c_int,
    pub mbps_max_bw: f64,
    pub byte_mss: c_int,
    pub pkt_snd_buf: c_int,
    pub byte_snd_buf: c_int,
    pub ms_snd_buf: c_int,
    pub ms_snd_tsb_pd_delay: c_int,
    pub pkt_rcv_buf: c_int,
    pub byte_rcv_buf: c_int,
    pub ms_rcv_buf: c_int,
    pub ms_rcv_tsb_pd_delay: c_int,
    pub pkt_snd_filter_extra_total: c_int,
    pub pkt_rcv_filter_extra_total: c_int,
    pub pkt_rcv_filter_supply_total: c_int,
    pub pkt_rcv_filter_loss_total: c_int,
    pub pkt_snd_filter_extra: c_int,
    pub pkt_rcv_filter_extra: c_int,
    pub pkt_rcv_filter_supply: c_int,
    pub pkt_rcv_filter_loss: c_int,
    pub pkt_reorder_tolerance: c_int,
    pub pkt_sent_unique_total: i64,
    pub pkt_recv_unique_total: i64,
    pub byte_sent_unique_total: u64,
    pub byte_recv_unique_total: u64,
    pub pkt_sent_unique: i64,
    pub pkt_recv_unique: i64,
    pub byte_sent_unique: u64,
    pub byte_recv_unique: u64,
}

// SRT bonding group types
pub const SRTGROUP_MASK: c_int = 1 << 30;
pub const SRT_GTYPE_BROADCAST: c_int = 1;
pub const SRT_GTYPE_BACKUP: c_int = 2;
const SRTS_CONNECTED: c_int = 5;
const SRTS_BROKEN: c_int = 6;
const SRT_GST_RUNNING: c_int = 2;
const SRT_GST_BROKEN: c_int = 3;

// SRT epoll event flags
const SRT_EPOLL_IN: c_int = 0x1;
const SRT_EPOLL_ERR: c_int = 0x8;

const SRT_ESCLOSED: c_int = 1005;
const SRT_ECONNLOST: c_int = 2001;
const SRT_ENOCONN: c_int = 2002;
const SRT_EASYNCRCV: c_int = 6002;
const SRT_ETIMEOUT: c_int = 6003;

#[repr(C)]
pub struct SrtSockOptConfig {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct SrtGroupMemberConfig {
    pub id: SRTSOCKET,
    pub srcaddr: libc::sockaddr_storage,
    pub peeraddr: libc::sockaddr_storage,
    pub weight: u16,
    pub config: *mut SrtSockOptConfig,
    pub errorcode: c_int,
    pub token: c_int,
}

#[repr(C)]
pub struct SrtSocketGroupData {
    pub id: SRTSOCKET,
    pub peeraddr: libc::sockaddr_storage,
    pub sockstate: c_int,
    pub weight: u16,
    pub memberstate: c_int,
    pub result: c_int,
    pub token: c_int,
}

// SAFETY: FFI declarations for the libsrt C library. All function signatures
// are verified against the libsrt public API (srt.h). The library is loaded
// at link time (dynamic or static) and is guaranteed to be present when
// srt_startup() succeeds during SrtServer::new(). None of these functions
// have Rust-side invariants beyond correct argument types, which are
// enforced by the Rust type system at each call site.
unsafe extern "C" {
    pub fn srt_getversion() -> u32;
    pub fn srt_startup() -> c_int;
    pub fn srt_cleanup() -> c_int;
    pub fn srt_create_socket() -> SRTSOCKET;
    pub fn srt_create_group(gtype: c_int) -> SRTSOCKET;
    pub fn srt_close(u: SRTSOCKET) -> c_int;
    pub fn srt_bind(u: SRTSOCKET, name: *const sockaddr_in, namelen: c_int) -> c_int;
    pub fn srt_listen(u: SRTSOCKET, backlog: c_int) -> c_int;
    pub fn srt_listen_callback(
        lsn: SRTSOCKET,
        hook_fn: Option<
            unsafe extern "C" fn(
                opaq: *mut c_void,
                ns: SRTSOCKET,
                hsversion: c_int,
                peeraddr: *const libc::sockaddr,
                streamid: *const c_char,
            ) -> c_int,
        >,
        hook_opaque: *mut c_void,
    ) -> c_int;
    pub fn srt_accept(u: SRTSOCKET, addr: *mut sockaddr_in, addrlen: *mut c_int) -> SRTSOCKET;
    pub fn srt_getsockname(u: SRTSOCKET, name: *mut sockaddr_in, namelen: *mut c_int) -> c_int;
    pub fn srt_connect(u: SRTSOCKET, name: *const sockaddr_in, namelen: c_int) -> c_int;
    pub fn srt_connect_group(
        group: SRTSOCKET,
        name: *mut SrtGroupMemberConfig,
        arraysize: c_int,
    ) -> c_int;
    pub fn srt_group_data(
        group: SRTSOCKET,
        output: *mut SrtSocketGroupData,
        inoutlen: *mut usize,
    ) -> c_int;
    pub fn srt_prepare_endpoint(
        src: *const libc::sockaddr,
        adr: *const libc::sockaddr,
        namelen: c_int,
    ) -> SrtGroupMemberConfig;
    pub fn srt_create_config() -> *mut SrtSockOptConfig;
    pub fn srt_delete_config(config: *mut SrtSockOptConfig);
    pub fn srt_config_add(
        config: *mut SrtSockOptConfig,
        option: c_int,
        contents: *const c_void,
        len: c_int,
    ) -> c_int;
    pub fn srt_recv(u: SRTSOCKET, buf: *mut u8, len: c_int) -> c_int;
    pub fn srt_recvmsg2(
        u: SRTSOCKET,
        buf: *mut u8,
        len: c_int,
        message_control: *mut c_void,
    ) -> c_int;
    pub fn srt_send(u: SRTSOCKET, buf: *const u8, len: c_int) -> c_int;
    pub fn srt_setsockopt(
        u: SRTSOCKET,
        level: c_int,
        optname: c_int,
        optval: *const c_void,
        optlen: c_int,
    ) -> c_int;
    pub fn srt_setsockflag(
        u: SRTSOCKET,
        optname: c_int,
        optval: *const c_void,
        optlen: c_int,
    ) -> c_int;
    pub fn srt_getsockopt(
        u: SRTSOCKET,
        level: c_int,
        optname: c_int,
        optval: *mut c_void,
        optlen: *mut c_int,
    ) -> c_int;
    pub fn srt_getlasterror(locp: *mut c_int) -> c_int;
    pub fn srt_getlasterror_str() -> *const c_char;
    pub fn srt_setrejectreason(sock: SRTSOCKET, value: c_int) -> c_int;
    pub fn srt_bistats(
        u: SRTSOCKET,
        perf: *mut SrtTraceBStats,
        clear: c_int,
        instantaneous: c_int,
    ) -> c_int;
    pub fn srt_epoll_create() -> c_int;
    pub fn srt_epoll_add_usock(eid: c_int, u: SRTSOCKET, events: *const c_int) -> c_int;
    pub fn srt_epoll_remove_usock(eid: c_int, u: SRTSOCKET) -> c_int;
    pub fn srt_epoll_release(eid: c_int) -> c_int;
    pub fn srt_epoll_wait(
        eid: c_int,
        readfds: *mut SRTSOCKET,
        rnum: *mut c_int,
        writefds: *mut SRTSOCKET,
        wnum: *mut c_int,
        ms_timeout: i64,
        lrfds: *mut c_int,
        lrnum: *mut c_int,
        lwfds: *mut c_int,
        lwnum: *mut c_int,
    ) -> c_int;
}

pub fn linked_srt_version() -> String {
    // SAFETY: srt_getversion returns a u32 with no side effects. Safe to
    // call at any time after srt_startup() (called during server init).
    let version = unsafe { srt_getversion() };
    format!(
        "{}.{}.{}",
        (version >> 16) & 0xff,
        (version >> 8) & 0xff,
        version & 0xff
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SrtReceiveErrorAction {
    WaitForReadiness,
    Disconnect,
}

fn classify_srt_receive_error(error_code: c_int) -> SrtReceiveErrorAction {
    match error_code {
        SRT_EASYNCRCV | SRT_ETIMEOUT => SrtReceiveErrorAction::WaitForReadiness,
        SRT_ESCLOSED | SRT_ECONNLOST | SRT_ENOCONN => SrtReceiveErrorAction::Disconnect,
        _ => SrtReceiveErrorAction::Disconnect,
    }
}

fn last_srt_error() -> (c_int, String) {
    let mut location = 0;
    // SAFETY: srt_getlasterror writes the optional source-location code to
    // `location`; srt_getlasterror_str returns a thread-local static string.
    let code = unsafe { srt_getlasterror(&mut location) };
    let message = unsafe { std::ffi::CStr::from_ptr(srt_getlasterror_str()) }
        .to_string_lossy()
        .into_owned();
    (code, message)
}

// SRT socket options — values from srt.h SRT_SOCKOPT enum
pub const SRTO_SNDSYN: c_int = 1;
pub const SRTO_RCVSYN: c_int = 2;
pub const SRTO_FC: c_int = 4;
pub const SRTO_SNDBUF: c_int = 5;
pub const SRTO_RCVBUF: c_int = 6;
pub const SRTO_UDP_SNDBUF: c_int = 8;
pub const SRTO_UDP_RCVBUF: c_int = 9;
pub const SRTO_MAXBW: c_int = 16;
pub const SRTO_LATENCY: c_int = 23;
pub const SRTO_INPUTBW: c_int = 24;
pub const SRTO_OHEADBW: c_int = 25;
pub const SRTO_PASSPHRASE: c_int = 26;
pub const SRTO_PBKEYLEN: c_int = 27;
pub const SRTO_LOSSMAXTTL: c_int = 42;
pub const SRTO_RCVLATENCY: c_int = 43;
pub const SRTO_PEERLATENCY: c_int = 44;
pub const SRTO_STREAMID: c_int = 46;
pub const SRTO_TRANSTYPE: c_int = 50;
pub const SRTO_ENFORCEDENCRYPTION: c_int = 53;
pub const SRTO_GROUPCONNECT: c_int = 57;
pub const SRTO_GROUPTYPE: c_int = 59;

pub const SRTT_LIVE: c_int = 0;

pub const DESIRED_UDP_BUF: i32 = 8 * 1024 * 1024;
const DESIRED_SRT_BUF: i32 = 12 * 1024 * 1024;
const DESIRED_FC: i32 = 32768;
// 4×RTT + 2×jitter for 50ms RTT, ~10ms jitter = 220ms. Round to 250ms for margin.
const DESIRED_LATENCY_MS: i32 = 250;
// Max reorder tolerance: at 50 Mbps / 1316 B per packet ≈ 4750 pkt/s.
// 50ms of reordering ≈ 238 packets. Default (0) lets SRT auto-detect, but
// setting a floor prevents premature loss declarations on jittery links.
const DESIRED_LOSSMAXTTL: i32 = 256;

fn enable_srt_group_connect(listener: SRTSOCKET) -> Result<(), String> {
    let group_connect: c_int = 1;
    // SAFETY: srt_setsockflag sets an option on a valid SRT socket. The
    // `group_connect` pointer and size are correctly typed.
    let result = unsafe {
        srt_setsockflag(
            listener,
            SRTO_GROUPCONNECT,
            &group_connect as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        )
    };
    if result >= 0 {
        Ok(())
    } else {
        // SAFETY: srt_getlasterror_str returns a NUL-terminated thread-local
        // static string valid until the next SRT call on this thread.
        let error = unsafe { std::ffi::CStr::from_ptr(srt_getlasterror_str()) };
        Err(error.to_string_lossy().into_owned())
    }
}

fn check_sysctl_limits() {
    let check = |path: &str, need: usize, label: &str| {
        if let Ok(s) = std::fs::read_to_string(path)
            && let Ok(val) = s.trim().parse::<usize>()
            && val < need
        {
            warn!(
                "{} = {} but we need {}. \
                         Run: sudo sysctl -w {}={}",
                path, val, need, label, need,
            );
        }
    };
    check(
        "/proc/sys/net/core/rmem_max",
        DESIRED_UDP_BUF as usize,
        "net.core.rmem_max",
    );
    check(
        "/proc/sys/net/core/wmem_max",
        DESIRED_UDP_BUF as usize,
        "net.core.wmem_max",
    );
}

/// Tune SRT socket for streams up to 4K 60fps (~50 Mbps H.264 peak).
///
/// Sizing rationale (designed for ≤50ms RTT, ~10ms jitter, ≤5% loss):
///
/// 1. **Latency** (`SRTO_LATENCY`): governs the receiver's dejitter/retransmit
///    window. Formula: `4×RTT + 2×jitter` = 4×50 + 2×10 = 220ms. Set 250ms
///    for margin. Sender and receiver negotiate the max of both sides. At
///    50 Mbps, 250ms = 1.56 MB in flight — well within our buffer sizes.
///
/// 2. **Kernel UDP socket** (`SRTO_UDP_SNDBUF`/`RCVBUF`): default ~208 KB
///    fills in ~33ms at 50 Mbps. Set to 8 MB (~1.3s at peak rate).
///
/// 3. **SRT internal buffers** (`SRTO_SNDBUF`/`RCVBUF`): hold packets for
///    retransmission. Must be ≥ latency × bitrate × (1 + loss_overhead).
///    At 250ms, 50 Mbps, 5% loss: 1.56 MB × 1.15 ≈ 1.8 MB minimum.
///    Set to 12 MB for headroom on burst retransmissions.
///
/// 4. **Flow control window** (`SRTO_FC`): max packets in flight. At 50 Mbps
///    / 1316 B = ~4750 pkt/s; 250ms latency = ~1188 in-flight packets.
///    Default 8192 is OK but set 32768 for high-latency links.
///
/// 5. **Loss max TTL** (`SRTO_LOSSMAXTTL`): reorder tolerance before
///    declaring loss. Default 0 = auto. Set 256 packets (~54ms at 50 Mbps)
///    to handle jitter without premature NACK storms.
// SAFETY: All srt_setsockopt calls use correctly-sized stack-allocated
// option values with valid SRT socket handles. The UDP/SRT buffer sizes,
// flow control window, and latency values are within platform limits.
fn srt_set_highbitrate_opts(sock: SRTSOCKET) {
    unsafe {
        // Latency: dejitter + retransmit window (4×RTT + 2×jitter)
        let latency: c_int = DESIRED_LATENCY_MS;
        srt_setsockopt(
            sock,
            0,
            SRTO_LATENCY,
            &latency as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );

        // Reorder tolerance before declaring loss
        let lossmaxttl: c_int = DESIRED_LOSSMAXTTL;
        srt_setsockopt(
            sock,
            0,
            SRTO_LOSSMAXTTL,
            &lossmaxttl as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );

        let udp_buf: c_int = DESIRED_UDP_BUF;
        srt_setsockopt(
            sock,
            0,
            SRTO_UDP_SNDBUF,
            &udp_buf as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );
        srt_setsockopt(
            sock,
            0,
            SRTO_UDP_RCVBUF,
            &udp_buf as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );

        let srt_buf: c_int = DESIRED_SRT_BUF;
        srt_setsockopt(
            sock,
            0,
            SRTO_SNDBUF,
            &srt_buf as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );
        srt_setsockopt(
            sock,
            0,
            SRTO_RCVBUF,
            &srt_buf as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );

        let fc: c_int = DESIRED_FC;
        srt_setsockopt(
            sock,
            0,
            SRTO_FC,
            &fc as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );

        let maxbw: i64 = -1;
        srt_setsockopt(
            sock,
            0,
            SRTO_MAXBW,
            &maxbw as *const _ as *const c_void,
            std::mem::size_of::<i64>() as c_int,
        );
    }
}

// SAFETY: srt_getsockopt reads integer option values from a valid SRT
// socket into correctly-sized stack variables. All options are benign
// diagnostic reads with no side effects on the socket.
fn srt_log_effective_opts(sock: SRTSOCKET, label: &str) {
    unsafe {
        let mut udp_snd = 0i32;
        let mut udp_rcv = 0i32;
        let mut srt_snd = 0i32;
        let mut srt_rcv = 0i32;
        let mut fc = 0i32;
        let mut latency = 0i32;
        let mut lossmaxttl = 0i32;
        let sz = std::mem::size_of::<c_int>() as c_int;
        let mut len = sz;
        srt_getsockopt(
            sock,
            0,
            SRTO_UDP_SNDBUF,
            &mut udp_snd as *mut _ as *mut c_void,
            &mut len,
        );
        len = sz;
        srt_getsockopt(
            sock,
            0,
            SRTO_UDP_RCVBUF,
            &mut udp_rcv as *mut _ as *mut c_void,
            &mut len,
        );
        len = sz;
        srt_getsockopt(
            sock,
            0,
            SRTO_SNDBUF,
            &mut srt_snd as *mut _ as *mut c_void,
            &mut len,
        );
        len = sz;
        srt_getsockopt(
            sock,
            0,
            SRTO_RCVBUF,
            &mut srt_rcv as *mut _ as *mut c_void,
            &mut len,
        );
        len = sz;
        srt_getsockopt(sock, 0, SRTO_FC, &mut fc as *mut _ as *mut c_void, &mut len);
        len = sz;
        srt_getsockopt(
            sock,
            0,
            SRTO_LATENCY,
            &mut latency as *mut _ as *mut c_void,
            &mut len,
        );
        len = sz;
        srt_getsockopt(
            sock,
            0,
            SRTO_LOSSMAXTTL,
            &mut lossmaxttl as *mut _ as *mut c_void,
            &mut len,
        );
        info!(
            "[srt] {} config: latency={}ms lossmaxttl={} UDP snd={}KB rcv={}KB, SRT snd={}KB rcv={}KB, FC={}",
            label,
            latency,
            lossmaxttl,
            udp_snd / 1024,
            udp_rcv / 1024,
            srt_snd / 1024,
            srt_rcv / 1024,
            fc,
        );
        if udp_snd < DESIRED_UDP_BUF {
            error!(
                "[srt] WARNING: {} UDP send buffer clamped to {}KB (wanted {}KB). \
                 Raise net.core.wmem_max",
                label,
                udp_snd / 1024,
                DESIRED_UDP_BUF / 1024,
            );
        }
        if udp_rcv < DESIRED_UDP_BUF {
            error!(
                "[srt] WARNING: {} UDP recv buffer clamped to {}KB (wanted {}KB). \
                 Raise net.core.rmem_max",
                label,
                udp_rcv / 1024,
                DESIRED_UDP_BUF / 1024,
            );
        }
    }
}

fn to_sockaddr_in(addr: SocketAddr) -> sockaddr_in {
    let ip = match addr.ip() {
        std::net::IpAddr::V4(ipv4) => u32::from_ne_bytes(ipv4.octets()),
        _ => 0,
    };
    sockaddr_in {
        sin_family: 2, // AF_INET
        sin_port: addr.port().to_be(),
        sin_addr: ip,
        sin_zero: [0; 8],
    }
}

fn from_sockaddr_in(addr: sockaddr_in) -> SocketAddr {
    SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::from(addr.sin_addr.to_ne_bytes())),
        u16::from_be(addr.sin_port),
    )
}

fn is_srt_group(socket: SRTSOCKET) -> bool {
    socket & SRTGROUP_MASK != 0
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SrtGroupSummary {
    member_count: u32,
    connected_members: u32,
    active_members: u32,
    broken_members: u32,
}

fn summarize_group_members(members: &[SrtSocketGroupData]) -> SrtGroupSummary {
    let mut summary = SrtGroupSummary {
        member_count: members.len() as u32,
        ..SrtGroupSummary::default()
    };
    for member in members {
        if member.sockstate == SRTS_CONNECTED {
            summary.connected_members += 1;
        }
        if member.memberstate == SRT_GST_RUNNING {
            summary.active_members += 1;
        }
        if member.sockstate == SRTS_BROKEN || member.memberstate == SRT_GST_BROKEN {
            summary.broken_members += 1;
        }
    }
    summary
}

fn srt_group_summary(group: SRTSOCKET) -> Option<SrtGroupSummary> {
    // Ingest bonds are normally two links. Keep ample room so this call stays
    // allocation-only and does not need to guess at libsrt's resize semantics.
    const MAX_GROUP_MEMBERS: usize = 64;
    // SAFETY: std::mem::zeroed() for C structs is valid when the struct
    // has no invalid bit patterns (all-zero is a valid SrtSocketGroupData).
    // srt_group_data fills the array through a raw pointer; members is
    // correctly sized and aligned.
    let mut members: Vec<SrtSocketGroupData> = (0..MAX_GROUP_MEMBERS)
        .map(|_| unsafe { std::mem::zeroed() })
        .collect();
    let mut member_count = members.len();
    let result = unsafe { srt_group_data(group, members.as_mut_ptr(), &mut member_count) };
    if result < 0 {
        return None;
    }
    members.truncate(member_count.min(members.len()));
    Some(summarize_group_members(&members))
}

fn add_srt_group_quality(
    quality: &mut PublisherQuality,
    is_group: bool,
    summary: Option<SrtGroupSummary>,
) {
    quality.srt_bonded = Some(is_group);
    if let Some(summary) = summary {
        quality.srt_group_member_count = Some(summary.member_count);
        quality.srt_group_connected_members = Some(summary.connected_members);
        quality.srt_group_active_members = Some(summary.active_members);
        quality.srt_group_broken_members = Some(summary.broken_members);
    }
}

#[derive(Debug, Clone, Copy)]
struct SrtCounterSnapshot {
    packets_received_loss: u64,
    packets_received_drop: u64,
    packets_received_retrans: u64,
    packets_received_undecrypt: u64,
    sampled_at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct SrtSenderCounterSnapshot {
    packets_sent_loss: u64,
    packets_sent_drop: u64,
    packets_sent_retrans: u64,
    sampled_at: Instant,
}

fn counter_rate(current: u64, previous: u64, elapsed_seconds: f64) -> Option<f64> {
    if elapsed_seconds <= 0.0 {
        return None;
    }
    current
        .checked_sub(previous)
        .map(|delta| (delta as f64 / elapsed_seconds * 10.0).round() / 10.0)
}

fn srt_quality_from_stats(
    stats: &SrtTraceBStats,
    previous: Option<SrtCounterSnapshot>,
    sampled_at: Instant,
) -> (PublisherQuality, SrtCounterSnapshot) {
    let current = SrtCounterSnapshot {
        packets_received_loss: stats.pkt_rcv_loss_total.max(0) as u64,
        packets_received_drop: stats.pkt_rcv_drop_total.max(0) as u64,
        packets_received_retrans: stats.pkt_rcv_retrans.max(0) as u64,
        packets_received_undecrypt: stats.pkt_rcv_undecrypt_total.max(0) as u64,
        sampled_at,
    };
    let elapsed =
        previous.map(|snapshot| sampled_at.duration_since(snapshot.sampled_at).as_secs_f64());

    (
        PublisherQuality {
            ms_rtt: Some(stats.ms_rtt),
            mbps_receive_rate: Some(stats.mbps_recv_rate),
            mbps_link_capacity: Some(stats.mbps_bandwidth),
            ms_receive_tsb_pd_delay: Some(stats.ms_rcv_tsb_pd_delay.max(0) as f64),
            ms_receive_buf: Some(stats.ms_rcv_buf.max(0) as f64),
            packets_sent_nak: Some(stats.pkt_sent_nak_total.max(0) as u64),
            packets_received_loss: Some(current.packets_received_loss),
            packets_received_drop: Some(current.packets_received_drop),
            packets_received_retrans: Some(current.packets_received_retrans),
            packets_received_undecrypt: Some(current.packets_received_undecrypt),
            packets_received_loss_per_sec: previous.zip(elapsed).and_then(|(snapshot, seconds)| {
                counter_rate(
                    current.packets_received_loss,
                    snapshot.packets_received_loss,
                    seconds,
                )
            }),
            packets_received_drop_per_sec: previous.zip(elapsed).and_then(|(snapshot, seconds)| {
                counter_rate(
                    current.packets_received_drop,
                    snapshot.packets_received_drop,
                    seconds,
                )
            }),
            packets_received_retrans_per_sec: previous.zip(elapsed).and_then(
                |(snapshot, seconds)| {
                    counter_rate(
                        current.packets_received_retrans,
                        snapshot.packets_received_retrans,
                        seconds,
                    )
                },
            ),
            packets_received_undecrypt_per_sec: previous.zip(elapsed).and_then(
                |(snapshot, seconds)| {
                    counter_rate(
                        current.packets_received_undecrypt,
                        snapshot.packets_received_undecrypt,
                        seconds,
                    )
                },
            ),
            srt_send_buf_bytes: Some(stats.byte_snd_buf),
            srt_recv_buf_bytes: Some(stats.byte_rcv_buf),
            srt_send_buf_avail_bytes: Some(stats.byte_avail_snd_buf),
            srt_recv_buf_avail_bytes: Some(stats.byte_avail_rcv_buf),
            srt_flight_size_pkts: Some(stats.pkt_flight_size),
            ..PublisherQuality::default()
        },
        current,
    )
}

fn srt_sender_quality_from_stats(
    stats: &SrtTraceBStats,
    previous: Option<SrtSenderCounterSnapshot>,
    sampled_at: Instant,
) -> (PublisherQuality, SrtSenderCounterSnapshot) {
    let current = SrtSenderCounterSnapshot {
        packets_sent_loss: stats.pkt_snd_loss_total.max(0) as u64,
        packets_sent_drop: stats.pkt_snd_drop_total.max(0) as u64,
        packets_sent_retrans: stats.pkt_retrans_total.max(0) as u64,
        sampled_at,
    };
    let elapsed =
        previous.map(|snapshot| sampled_at.duration_since(snapshot.sampled_at).as_secs_f64());

    (
        PublisherQuality {
            ms_rtt: Some(stats.ms_rtt),
            mbps_send_rate: Some(stats.mbps_send_rate),
            mbps_link_capacity: Some(stats.mbps_bandwidth),
            ms_send_tsb_pd_delay: Some(stats.ms_snd_tsb_pd_delay.max(0) as f64),
            ms_send_buf: Some(stats.ms_snd_buf.max(0) as f64),
            packets_sent_loss: Some(current.packets_sent_loss),
            packets_sent_drop: Some(current.packets_sent_drop),
            packets_sent_retrans: Some(current.packets_sent_retrans),
            packets_received_nak: Some(stats.pkt_recv_nak_total.max(0) as u64),
            packets_sent_loss_per_sec: previous.zip(elapsed).and_then(|(snapshot, seconds)| {
                counter_rate(
                    current.packets_sent_loss,
                    snapshot.packets_sent_loss,
                    seconds,
                )
            }),
            packets_sent_drop_per_sec: previous.zip(elapsed).and_then(|(snapshot, seconds)| {
                counter_rate(
                    current.packets_sent_drop,
                    snapshot.packets_sent_drop,
                    seconds,
                )
            }),
            packets_sent_retrans_per_sec: previous.zip(elapsed).and_then(|(snapshot, seconds)| {
                counter_rate(
                    current.packets_sent_retrans,
                    snapshot.packets_sent_retrans,
                    seconds,
                )
            }),
            srt_send_buf_bytes: Some(stats.byte_snd_buf),
            srt_send_buf_avail_bytes: Some(stats.byte_avail_snd_buf),
            srt_flight_size_pkts: Some(stats.pkt_flight_size),
            srt_flow_window_pkts: Some(stats.pkt_flow_window),
            srt_congestion_window_pkts: Some(stats.pkt_congestion_window),
            ..PublisherQuality::default()
        },
        current,
    )
}

#[path = "srt_stream_id.rs"]
mod srt_stream_id;
use srt_stream_id::{SrtConnectionMode, parse_srt_stream_id, percent_decode};
fn try_acquire_srt_sender_permit(
    semaphore: Arc<tokio::sync::Semaphore>,
) -> Result<tokio::sync::OwnedSemaphorePermit, tokio::sync::TryAcquireError> {
    semaphore.try_acquire_owned()
}

const SRT_REJX_UNAUTHORIZED: c_int = 1401;
const SRT_REJX_BAD_MODE: c_int = 1405;
const SRT_REJX_ISE: c_int = 1500;

unsafe extern "C" fn srt_listener_policy_callback(
    opaq: *mut c_void,
    ns: SRTSOCKET,
    hsversion: c_int,
    peeraddr: *const libc::sockaddr,
    streamid: *const c_char,
) -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        srt_listener_policy_callback_inner(opaq, ns, hsversion, peeraddr, streamid)
    }));

    match result {
        Ok(code) => code,
        Err(_) => {
            error!("[srt] listener policy callback panicked; rejecting connection");
            unsafe {
                srt_setrejectreason(ns, SRT_REJX_ISE);
            }
            -1
        }
    }
}

unsafe fn srt_listener_policy_callback_inner(
    opaq: *mut c_void,
    ns: SRTSOCKET,
    _hsversion: c_int,
    _peeraddr: *const libc::sockaddr,
    streamid: *const c_char,
) -> c_int {
    if opaq.is_null() {
        unsafe {
            srt_setrejectreason(ns, SRT_REJX_ISE);
        }
        return -1;
    }

    let store = unsafe { &*(opaq as *const SrtIngestPolicyStore) };
    let streamid = if streamid.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(streamid) }
            .to_string_lossy()
            .to_string()
    };
    let parsed = parse_srt_stream_id(&streamid);
    if !matches!(
        parsed.mode,
        SrtConnectionMode::Publish | SrtConnectionMode::Read
    ) || parsed.stream_key.is_empty()
    {
        unsafe {
            srt_setrejectreason(ns, SRT_REJX_BAD_MODE);
        }
        return -1;
    }

    let Some(policy) = store.resolved_policy(&parsed.stream_key) else {
        unsafe {
            srt_setrejectreason(ns, SRT_REJX_UNAUTHORIZED);
        }
        return -1;
    };

    if let Some(crypto) = srt_crypto_from_resolved(policy)
        && apply_srt_crypto_socket(ns, &crypto).is_err()
    {
        unsafe {
            srt_setrejectreason(ns, SRT_REJX_ISE);
        }
        return -1;
    }

    0
}

#[path = "srt_monitor.rs"]
mod srt_monitor;
use srt_monitor::monitor_listener_socket;
#[cfg(test)]
use srt_monitor::{audio_codec_id, read_udp_socket_stats, video_codec_id};
pub struct SrtServer {
    pipeline_lookup: Arc<dyn PipelineStore>,
    engine: Arc<MediaEngine>,
    security: Arc<crate::media::security::IngestSecurityService>,
    ingest_policy_store: Arc<SrtIngestPolicyStore>,
}

impl SrtServer {
    pub fn new(
        pipeline_lookup: Arc<dyn PipelineStore>,
        engine: Arc<MediaEngine>,
        security: Arc<crate::media::security::IngestSecurityService>,
        ingest_policy_store: Arc<SrtIngestPolicyStore>,
    ) -> Self {
        // SAFETY: srt_startup must be called once before any other SRT
        // function. This is the only call site, at server construction time,
        // enforced by the singleton SrtServer pattern.
        unsafe {
            srt_startup();
        }
        check_sysctl_limits();
        Self {
            pipeline_lookup,
            engine,
            security,
            ingest_policy_store,
        }
    }

    pub async fn run(self: Arc<Self>, port: u16) {
        // SAFETY: srt_create_socket returns a valid SRT socket handle or -1
        // on error. The socket is closed via SrtSockGuard on drop or
        // explicitly on bind/listen failure below. Balanced by srt_close.
        let server_sock = unsafe { srt_create_socket() };
        if server_sock < 0 {
            error!("Failed to create socket");
            return;
        }

        // SAFETY: Sets SRTT_LIVE transmission type on a valid listener
        // socket. The option value is a stack-allocated c_int.
        unsafe {
            let live_mode: c_int = SRTT_LIVE;
            srt_setsockopt(
                server_sock,
                0,
                SRTO_TRANSTYPE,
                &live_mode as *const _ as *const c_void,
                std::mem::size_of::<c_int>() as c_int,
            );
        }
        let listener_store_ptr = Arc::as_ptr(&self.ingest_policy_store) as *mut c_void;
        let callback_res = unsafe {
            srt_listen_callback(
                server_sock,
                Some(srt_listener_policy_callback),
                listener_store_ptr,
            )
        };
        if callback_res < 0 {
            error!("[srt] failed to install listener policy callback");
            unsafe {
                srt_close(server_sock);
            }
            return;
        }
        if let Some(crypto) = srt_crypto_from_resolved(
            self.ingest_policy_store
                .global_config()
                .resolve()
                .unwrap_or(ResolvedSrtIngestConfig::Plaintext),
        ) {
            info!(
                "[srt] default listener ingest encryption enabled (pbkeylen={})",
                crypto.pbkeylen
            );
        }
        match enable_srt_group_connect(server_sock) {
            Ok(()) => {
                self.engine
                    .runtime
                    .listener_stats
                    .bonding_available
                    .store(true, Ordering::Relaxed);
                info!("Bonded ingest enabled on the shared listener (SRTO_GROUPCONNECT)",)
            }
            Err(error) => {
                self.engine
                    .runtime
                    .listener_stats
                    .bonding_available
                    .store(false, Ordering::Relaxed);
                error!(
                    "[srt] WARNING: bonded ingest is unavailable: linked libsrt rejected \
                 SRTO_GROUPCONNECT ({error}). Install/build libsrt with ENABLE_BONDING=ON. \
                 Single-link SRT ingest remains available."
                )
            }
        }
        srt_set_highbitrate_opts(server_sock);
        srt_log_effective_opts(server_sock, "listener");

        let addr_str = format!("0.0.0.0:{}", port);
        let addr = match addr_str.parse::<SocketAddr>() {
            Ok(a) => a,
            Err(e) => {
                error!("Invalid address: {:?}", e);
                return;
            }
        };

        let sin = to_sockaddr_in(addr);
        // SAFETY: srt_bind binds a valid server socket to the given
        // sockaddr_in. The sockaddr struct is stack-allocated and correctly
        // sized. On failure the socket is closed explicitly.
        let bind_res = unsafe {
            srt_bind(
                server_sock,
                &sin,
                std::mem::size_of::<sockaddr_in>() as c_int,
            )
        };
        if bind_res < 0 {
            error!("Bind failed");
            // SAFETY: server_sock is a valid socket not yet closed.
            unsafe {
                srt_close(server_sock);
            }
            return;
        }

        // SAFETY: srt_listen starts listening on a bound socket. Backlog 1024
        // is a common value for high-throughput servers. On failure the socket
        // is closed explicitly.
        let listen_res = unsafe { srt_listen(server_sock, 1024) };
        if listen_res < 0 {
            error!("Listen failed");
            // SAFETY: Valid socket, not yet closed.
            unsafe {
                srt_close(server_sock);
            }
            return;
        }

        info!("Server listening on srt://{}", addr_str);

        // Monitor the shared listener socket's kernel UDP buffer occupancy
        let listener_stats = self.engine.listener_stats_handle();
        tokio::spawn(async move {
            monitor_listener_socket(port, listener_stats).await;
        });

        // Bounded channel between the blocking accept thread and the tokio task.
        // Capacity of 1024 means at most 1024 accepted-but-unprocessed sockets
        // queue up before the accept thread blocks. This limits memory growth
        // under a connection-flood attack without rejecting valid clients under
        // normal load (tokio processes items as fast as it can).
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(SRTSOCKET, sockaddr_in)>(1024);

        // RAII guard: close server_sock when run() returns (normal exit, task
        // cancellation, or panic).  Closing the socket interrupts srt_accept()
        // in the accept thread, which then exits via the tx.send() failure path.
        // SAFETY: SrtSockGuard is an RAII guard that closes the server
        // socket on drop. The socket was created by srt_create_socket()
        // above and has not been closed elsewhere. srt_close is idempotent
        // for invalid handles but the guard is only constructed for valid
        // sockets.
        struct SrtSockGuard(SRTSOCKET);
        impl Drop for SrtSockGuard {
            fn drop(&mut self) {
                // SAFETY: The guard owns a socket created by
                // srt_create_socket(). srt_close is called exactly once
                // per socket via this RAII drop.
                unsafe {
                    srt_close(self.0);
                }
            }
        }
        let _server_sock_guard = SrtSockGuard(server_sock);

        // Blocking accept thread — srt_accept in sync mode blocks until a connection arrives.
        // Wrapped in catch_unwind so a panic cannot crash the process (AGENTS.md).
        let accept_handle = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                loop {
                    let mut client_sin = sockaddr_in {
                        sin_family: 0,
                        sin_port: 0,
                        sin_addr: 0,
                        sin_zero: [0; 8],
                    };
                    let mut len = std::mem::size_of::<sockaddr_in>() as c_int;
                    // SAFETY: srt_accept blocks until a connection arrives.
                    // Called from a dedicated std::thread (not tokio), so
                    // blocking is acceptable. server_sock is valid; client_sin
                    // and len are correctly sized.
                    let client_sock = unsafe { srt_accept(server_sock, &mut client_sin, &mut len) };
                    if client_sock < 0 {
                        // SAFETY: srt_getlasterror_str returns a thread-local
                        // static string valid until the next SRT call.
                        let err = unsafe { std::ffi::CStr::from_ptr(srt_getlasterror_str()) };
                        warn!("accept error: {}", err.to_string_lossy());
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    }
                    // blocking_send: the accept thread is a std::thread so it
                    // can block here when the channel is full. This creates
                    // natural backpressure — the accept thread pauses while
                    // tokio drains the queue, preventing unbounded growth.
                    if tx.blocking_send((client_sock, client_sin)).is_err() {
                        // SAFETY: client_sock was just accepted and has not
                        // been closed. Channel closure means the server is
                        // shutting down — clean up the accepted socket.
                        unsafe {
                            srt_close(client_sock);
                        }
                        break;
                    }
                }
            }));
            if result.is_err() {
                error!("Accept thread panicked — ingest listener is down");
            }
        });
        self.engine.register_os_thread(accept_handle);

        while let Some((client_sock, client_addr)) = rx.recv().await {
            let self_clone = self.clone();
            tokio::spawn(async move {
                self_clone
                    .handle_client(client_sock, from_sockaddr_in(client_addr))
                    .await;
            });
        }
    }

    async fn handle_client(&self, client_sock: SRTSOCKET, client_addr: SocketAddr) {
        let is_group = is_srt_group(client_sock);
        let client_ip = client_addr.ip().to_string();

        // Rate-limit check — same gate as RTMP (H1 fix)
        if let Some(remaining) = self.security.is_ip_banned(&client_ip) {
            error!(
                "[srt] Rejecting banned IP {} (ban expires in {:.1}s)",
                client_ip,
                remaining.as_secs_f64()
            );
            // SAFETY: client_sock is a valid accepted socket not yet closed.
            unsafe { srt_close(client_sock) };
            return;
        }

        // Read streamid
        let mut streamid_buf = [0u8; 512];
        let mut optlen = streamid_buf.len() as c_int;
        // SAFETY: srt_getsockopt reads the STREAMID from a valid client
        // socket. streamid_buf is a 512-byte stack buffer; optlen is
        // initialized to the buffer size and updated with the actual length.
        let res = unsafe {
            srt_getsockopt(
                client_sock,
                0,
                SRTO_STREAMID,
                streamid_buf.as_mut_ptr() as *mut c_void,
                &mut optlen,
            )
        };

        let streamid = if res >= 0 {
            String::from_utf8_lossy(&streamid_buf[..optlen as usize])
                .trim_matches('\0')
                .to_string()
        } else {
            "".to_string()
        };

        info!(
            "[srt] {} accepted (id={}). StreamID: {}",
            if is_group {
                "Bonded group"
            } else {
                "Connection"
            },
            client_sock,
            streamid
        );

        let parsed = parse_srt_stream_id(&streamid);
        let is_reader = parsed.mode == SrtConnectionMode::Read;
        let stream_key = parsed.stream_key.as_str();

        // Query pipeline for stream key validation
        let pipeline = match authenticate_srt_stream_key(
            self.pipeline_lookup.as_ref(),
            &self.security,
            stream_key,
            &client_ip,
        )
        .await
        {
            Ok(pipeline) => pipeline,
            Err(_) => {
                warn!("unauthorized connection for stream key: {}", stream_key);
                // SAFETY: client_sock is a valid accepted socket not yet closed.
                unsafe {
                    srt_close(client_sock);
                }
                return;
            }
        };

        info!(
            "[srt] Authenticated stream key: {} for pipeline: {} (mode={})",
            stream_key,
            pipeline.id,
            if is_reader { "read" } else { "publish" }
        );

        if is_reader {
            self.handle_play(client_sock, &pipeline.id).await;
            return;
        }

        let mut ring_buffer = self.engine.get_or_create_pipeline(&pipeline.id).await;
        let Some(registration) = self
            .engine
            .try_register_ingest_attempt(&pipeline.id, stream_key, "srt")
            .await
        else {
            error!(
                "[srt] Rejecting duplicate publisher for pipeline {}",
                pipeline.id
            );
            // SAFETY: Valid socket, not yet closed elsewhere.
            unsafe { srt_close(client_sock) };
            return;
        };
        self.engine
            .update_ingest_meta(&pipeline.id, None, None, Some(client_addr.to_string()))
            .await;
        if is_group {
            match srt_group_summary(client_sock) {
                Some(summary) => info!(
                    sock = client_sock,
                    members = summary.member_count,
                    connected = summary.connected_members,
                    active = summary.active_members,
                    broken = summary.broken_members,
                    "bonded ingest group accepted",
                ),
                None => warn!(
                    sock = client_sock,
                    "bonded ingest group accepted but member state not available"
                ),
            }
        }

        let Some((bytes_received, ingest_metrics)) = self
            .engine
            .with_active_ingest(&pipeline.id, |ingest| {
                (ingest.bytes_received.clone(), ingest.metrics.clone())
            })
            .await
        else {
            error!(
                "[srt] Ingest vanished before receive loop for pipeline {}",
                pipeline.id
            );
            self.engine
                .unregister_ingest_if_current(&pipeline.id, &registration)
                .await;
            // SAFETY: Valid socket, clean up on early return.
            unsafe { srt_close(client_sock) };
            return;
        };

        // Cache a clone of the keyframe_times Arc so we can lock it directly
        // without an async registry lookup (active_ingests.read().await +
        // HashMap::get()) on every IDR frame in the ingest hot loop.
        let cached_keyframe_times = self
            .engine
            .with_active_ingest(&pipeline.id, |ingest| ingest.keyframe_times.clone())
            .await;

        // Pure-Rust MPEG-TS demuxer — no FFmpeg thread or MemoryQueue needed
        let mut demuxer = crate::media::mpegts::TsDemuxer::new();
        let mut packets = Vec::with_capacity(16);
        let mut probe_sent = false;
        let mut disconnect_phase: Option<String> = None;
        let mut disconnect_reason: Option<String> = None;
        let mut disconnect_had_error = false;

        // Set non-blocking mode so srt_recv returns immediately with EAGAIN
        // instead of blocking the tokio runtime thread
        // SAFETY: Sets non-blocking mode on a valid client socket. The zero
        // value and sizeof(c_int) are correct for SRTO_RCVSYN.
        let zero: c_int = 0;
        unsafe {
            srt_setsockopt(
                client_sock,
                0,
                SRTO_RCVSYN,
                &zero as *const _ as *const c_void,
                std::mem::size_of::<c_int>() as c_int,
            );
        }

        // SAFETY: srt_epoll_create creates a new epoll instance. The handle
        // is valid or negative on error. Released by the epoll_waiter task
        // (see below) so it is always freed even if this async future is
        // dropped at an await point before reaching the cleanup block.
        let eid = unsafe { srt_epoll_create() };
        if eid < 0 {
            error!("Failed to create epoll instance");
            self.engine
                .unregister_ingest_if_current(&pipeline.id, &registration)
                .await;
            // SAFETY: Valid socket, clean up on epoll failure.
            unsafe { srt_close(client_sock) };
            return;
        }
        let epoll_events = (SRT_EPOLL_IN | SRT_EPOLL_ERR) as c_int;
        // SAFETY: srt_epoll_add_usock registers client_sock with the epoll
        // instance. eid and client_sock are valid handles. epoll_events
        // pointer references a live stack variable.
        if unsafe { srt_epoll_add_usock(eid, client_sock, &epoll_events) } < 0 {
            error!("Failed to add socket to epoll");
            self.engine
                .unregister_ingest_if_current(&pipeline.id, &registration)
                .await;
            // SAFETY: eid and client_sock are valid handles. Clean up in
            // reverse creation order: release epoll, then close socket.
            unsafe {
                srt_epoll_release(eid);
                srt_close(client_sock)
            };
            return;
        }

        // RAII guard: closes client_sock when this scope exits (normal exit,
        // panic, or future drop at an await point).  Created after all early-
        // return paths that would double-close the socket.
        // SAFETY: client_sock is a valid socket not closed elsewhere after
        // this point; srt_close is called exactly once via this guard.
        struct SrtClientGuard(SRTSOCKET);
        impl Drop for SrtClientGuard {
            fn drop(&mut self) {
                unsafe {
                    srt_close(self.0);
                }
            }
        }
        let _client_sock_guard = SrtClientGuard(client_sock);

        // Socket groups use the message API and may deliver up to the live
        // payload limit. Single sockets retain the lean plain-recv path.
        let mut buf = vec![0u8; if is_group { 2048 } else { 1316 }];
        let mut previous_stats: Option<SrtCounterSnapshot> = None;
        let mut last_stats_sample = Instant::now() - Duration::from_secs(1);

        // Long-lived epoll waiter: one spawn_blocking task for the entire
        // connection lifetime replaces per-EAGAIN spawn_blocking. Solves:
        //   1. Task allocation per idle cycle
        //   2. No cancellation propagation (infinite epoll_wait timeout)
        //   3. Silently discarded errors on EAGAIN path
        let data_ready = Arc::new(AtomicBool::new(false));
        let epoll_stop = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Notify::new());

        let w_data_ready = data_ready.clone();
        let w_epoll_stop = epoll_stop.clone();
        let w_notify = notify.clone();
        // The task owns eid and releases it before signaling completion.
        // This ensures srt_epoll_release runs even if the outer async future
        // is dropped at an await point (the JoinHandle detaches but the
        // blocking task continues to completion).
        let mut epoll_waiter = Some(tokio::task::spawn_blocking(move || {
            loop {
                if w_epoll_stop.load(Ordering::Acquire) {
                    // Release the epoll handle before waking the outer task.
                    // SAFETY: eid is valid; we are the only caller of
                    // srt_epoll_release for this handle. The outer code no
                    // longer calls srt_epoll_release after this task exits.
                    unsafe {
                        srt_epoll_release(eid);
                    }
                    // Wake the main task so it can observe we're done.
                    w_data_ready.store(true, Ordering::Release);
                    w_notify.notify_one();
                    return;
                }

                let mut read_ready = [SRTSOCKET::default(); 1];
                let mut rnum = 1i32;
                // SAFETY: srt_epoll_wait blocks the OS thread until data
                // arrives or timeout. NULL write/lwfd/wfds sets are valid
                // (we only wait for read-ready). Called from spawn_blocking
                // so the tokio runtime is not blocked.
                //
                // 200ms timeout balances:
                //   - Cancellation responsiveness: ≤200ms from cancel to exit
                //   - CPU: no busy-loop (vs polling with a microsleep)
                //   - Perceptibility: 200ms is imperceptible on stream stop
                //   - Cleanup: ≤200ms delay before epoll handle is freed
                let ret = unsafe {
                    srt_epoll_wait(
                        eid,
                        read_ready.as_mut_ptr(),
                        &mut rnum,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        200,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
                if ret > 0 {
                    // Data available — wake the consumer.
                    w_data_ready.store(true, Ordering::Release);
                    w_notify.notify_one();
                }
                // ret == 0 (timeout) or < 0 (error): loop back and check stop.
            }
        }));

        // RAII guard: signals the epoll_waiter task to exit when this scope
        // ends (normal return, panic, or future dropped at an await point).
        // The task then calls srt_epoll_release(eid) before exiting.
        struct EpollStopGuard {
            stop: Arc<AtomicBool>,
            notify: Arc<Notify>,
        }
        impl Drop for EpollStopGuard {
            fn drop(&mut self) {
                self.stop.store(true, Ordering::Release);
                self.notify.notify_one();
            }
        }
        let _epoll_stop_guard = EpollStopGuard {
            stop: epoll_stop.clone(),
            notify: notify.clone(),
        };

        loop {
            if registration.cancel_token.is_cancelled() {
                break;
            }

            // SAFETY: srt_recv/srt_recvmsg2 reads from a valid
            // non-blocking SRT socket into `buf`, which is a correctly
            // sized Vec<u8>. The msghdr argument for srt_recvmsg2 is NULL
            // (we don't need per-message metadata). Returns bytes read or
            // -1 on error (EAGAIN in non-blocking mode).
            let n = unsafe {
                if is_group {
                    srt_recvmsg2(
                        client_sock,
                        buf.as_mut_ptr(),
                        buf.len() as c_int,
                        std::ptr::null_mut(),
                    )
                } else {
                    srt_recv(client_sock, buf.as_mut_ptr(), buf.len() as c_int)
                }
            };
            if n > 0 {
                // Data received — process below
            } else if n == 0 {
                disconnect_phase = Some("disconnect".to_string());
                disconnect_reason = Some("publisher disconnected".to_string());
                break; // connection closed
            } else {
                let (error_code, error_message) = last_srt_error();
                match classify_srt_receive_error(error_code) {
                    SrtReceiveErrorAction::WaitForReadiness => {
                        if !data_ready.swap(false, Ordering::Acquire) {
                            tokio::select! {
                                _ = notify.notified() => {}
                                _ = registration.cancel_token.cancelled() => break,
                            }
                        }
                    }
                    SrtReceiveErrorAction::Disconnect => {
                        error!(
                            "[srt] Receive ended for pipeline {}: code={} {}",
                            pipeline.id, error_code, error_message
                        );
                        disconnect_phase = Some("receive".to_string());
                        disconnect_reason = Some(format!("code={error_code} {error_message}"));
                        disconnect_had_error = true;
                        break;
                    }
                }
                continue;
            }

            // Feed into demuxer and push completed packets to ring buffer
            demuxer.feed(&buf[..n as usize]);
            if demuxer.drain_into(&mut packets) > 0 {
                for pkt in &packets {
                    if pkt.media_type == crate::media::ring_buffer::MediaType::Video
                        && let Some(parameter_sets) =
                            crate::media::codec::annexb_parameter_sets(&pkt.payload)
                    {
                        ring_buffer.set_video_parameter_sets(parameter_sets);
                    }
                    if pkt.media_type == crate::media::ring_buffer::MediaType::Video
                        && pkt.is_keyframe
                        && let Some(ref kf_times) = cached_keyframe_times
                    {
                        let mut times = kf_times.lock().unwrap_or_else(|e| e.into_inner());
                        times.push(pkt.pts);
                        if times.len() > 30 {
                            times.remove(0);
                        }
                    }
                }
                ring_buffer.push_drained_batch_capped(&mut packets);
            }

            // Send probe metadata once ready
            if !probe_sent && let Some(probe) = demuxer.take_probe() {
                probe_sent = true;
                let video_fps = probe.video.as_ref().map(|v| v.fps).unwrap_or(30.0);
                let audio_track_count = probe.audio_tracks.len();
                if let Some(ref v) = probe.video {
                    info!(
                        "[srt] Probed video: {} {}x{} {:.1}fps profile={:?}",
                        v.codec, v.width, v.height, v.fps, v.profile
                    );
                }
                for a in &probe.audio_tracks {
                    info!(
                        "[srt] Probed audio track {}: {} {}Hz {}ch",
                        a.track_index, a.codec, a.sample_rate, a.channels
                    );
                }
                let first_audio = probe.audio_tracks.first().cloned();
                let selected_video_track_index = probe.video.as_ref().map(|_| 0);
                self.engine
                    .update_ingest_meta(&pipeline.id, probe.video, first_audio, None)
                    .await;
                self.engine
                    .update_ingest_video_track_selection(
                        &pipeline.id,
                        probe.video_track_count,
                        selected_video_track_index,
                    )
                    .await;
                if !probe.audio_tracks.is_empty() {
                    self.engine
                        .update_ingest_audio_tracks(&pipeline.id, probe.audio_tracks)
                        .await;
                }
                // Adapt ring capacity for the detected packet rate.
                // If the ring was resized, update the local reference so
                // subsequent push_batch() calls write to the new ring.
                if let Some(new_ring) = self
                    .engine
                    .adapt_pipeline_ring(&pipeline.id, video_fps, audio_track_count)
                    .await
                {
                    ring_buffer = new_ring;
                }
            }

            bytes_received.fetch_add(n as u64, Ordering::Relaxed);
            ingest_metrics.record_in(n as u64);

            if last_stats_sample.elapsed() >= std::time::Duration::from_secs(1) {
                let mut stats: SrtTraceBStats = unsafe { std::mem::zeroed() };
                let sampled_at = Instant::now();
                let group_summary = is_group.then(|| srt_group_summary(client_sock)).flatten();
                if unsafe { srt_bistats(client_sock, &mut stats, 0, 1) } >= 0 {
                    let (mut quality, snapshot) =
                        srt_quality_from_stats(&stats, previous_stats, sampled_at);
                    add_srt_group_quality(&mut quality, is_group, group_summary);
                    previous_stats = Some(snapshot);
                    self.engine
                        .update_publisher_quality(&pipeline.id, quality)
                        .await;
                } else {
                    let mut quality = PublisherQuality::default();
                    add_srt_group_quality(&mut quality, is_group, group_summary);
                    self.engine
                        .update_publisher_quality(&pipeline.id, quality)
                        .await;
                }
                last_stats_sample = sampled_at;
            }
        }

        // Flush any remaining PES data
        demuxer.flush();
        if demuxer.drain_into(&mut packets) > 0 {
            for pkt in &packets {
                if pkt.media_type == crate::media::ring_buffer::MediaType::Video
                    && let Some(parameter_sets) =
                        crate::media::codec::annexb_parameter_sets(&pkt.payload)
                {
                    ring_buffer.set_video_parameter_sets(parameter_sets);
                }
            }
            ring_buffer.push_drained_batch_capped(&mut packets);
        }

        info!("Ingest stream finished for pipeline: {}", pipeline.id);
        self.engine
            .record_ingest_disconnect_if_current(
                &pipeline.id,
                &registration,
                disconnect_phase.as_deref(),
                disconnect_reason,
                disconnect_had_error,
            )
            .await;
        self.engine
            .unregister_ingest_if_current(&pipeline.id, &registration)
            .await;

        // Signal the epoll_waiter task to stop and wait for it to release eid.
        // The _epoll_stop_guard would do this on drop, but signaling explicitly
        // here lets us await the task handle — ensuring eid is released before
        // the _client_sock_guard drops and closes the socket.
        epoll_stop.store(true, Ordering::Release);
        notify.notify_one();
        if let Some(handle) = epoll_waiter.take() {
            let _ = handle.await;
        }
        // _epoll_stop_guard and _client_sock_guard drop here in LIFO order:
        //   1. _epoll_stop_guard: no-op (stop already set above)
        //   2. _client_sock_guard: srt_close(client_sock)
    }

    async fn handle_play(&self, client_sock: SRTSOCKET, pipeline_id: &str) {
        // Verify active ingest exists
        if !self
            .engine
            .ingests
            .active
            .read()
            .await
            .contains_key(pipeline_id)
        {
            warn!("no active ingest for play: {}", pipeline_id);
            // SAFETY: client_sock is a valid accepted socket not yet closed.
            unsafe {
                srt_close(client_sock);
            }
            return;
        }

        let ring_buf = self.engine.get_or_create_pipeline(pipeline_id).await;
        let shared_muxer = self
            .engine
            .get_or_create_ts_muxer_stage(pipeline_id, "play", ring_buf.clone())
            .await;

        let out_queue = Arc::new(crate::media::avio::MemoryQueue::new_with_capacity(
            self.engine.config.avio_capacity,
        ));

        // Sender thread: reads MPEG-TS from out_queue, sends via SRT.
        // Wrapped in catch_unwind so a panic cannot crash the process (AGENTS.md).
        // Acquire a semaphore permit to cap concurrent SRT sender threads at 512.
        // try_acquire_owned returns Err if the semaphore is exhausted; in that
        // case we reject the play connection gracefully rather than spawning a
        // thread that would push memory/VAS over the limit.
        let permit =
            match try_acquire_srt_sender_permit(self.engine.runtime.sender_semaphore.clone()) {
                Ok(p) => p,
                Err(_) => {
                    warn!(
                        "sender thread limit reached — rejecting play for {}",
                        pipeline_id
                    );
                    // SAFETY: Valid socket, clean up on capacity rejection.
                    unsafe {
                        srt_close(client_sock);
                    }
                    return;
                }
            };
        let out_queue_send = out_queue.clone();
        let pid_log = pipeline_id.to_string();
        let out_queue_c = out_queue.clone();
        let play_sender_handle = std::thread::spawn(move || {
            let _permit = permit; // dropped when thread exits → releases semaphore slot
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut buf = vec![0u8; 1316];
                loop {
                    let n = out_queue_send.read(&mut buf);
                    if n == 0 {
                        break;
                    }
                    // SAFETY: srt_send transmits data over a valid SRT
                    // socket. buf is a correctly sized Vec<u8>; n is the
                    // number of bytes read from MemoryQueue (≤ buf.len()).
                    let sent = unsafe { srt_send(client_sock, buf.as_ptr(), n as c_int) };
                    if sent < 0 {
                        break;
                    }
                }
            }));
            if result.is_err() {
                error!(
                    "[srt] Play sender thread panicked for pipeline: {}",
                    pid_log
                );
            } else {
                info!(
                    "[srt] Play subscriber disconnected for pipeline: {}",
                    pid_log
                );
            }
            out_queue_c.close();
            // SAFETY: client_sock was created during handle_client and
            // passed to this thread. It is closed exactly once here after
            // the sender loop exits (either normal disconnect or error).
            unsafe {
                srt_close(client_sock);
            }
        });
        self.engine.register_os_thread(play_sender_handle);

        let mut reader = TsChunkReader::new(format!("srt_play:{}", pipeline_id), &shared_muxer);
        let mut pull_packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
        let mut ts_batch: Vec<u8> = Vec::with_capacity(MEDIA_TS_BATCH_TARGET_BYTES);

        loop {
            let wake = reader.wait_for_data_or_cancelled().await;
            if out_queue.is_closed() {
                break;
            }
            loop {
                pull_packets.clear();
                match reader.pull_burst(&mut pull_packets, MEDIA_PULL_BURST_PACKETS) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                for pkt in &pull_packets {
                    if !pkt.payload.is_empty() {
                        ts_batch.extend_from_slice(&pkt.payload);
                    }
                }
                // One lock acquisition for the whole burst.
                if !ts_batch.is_empty() {
                    out_queue.write(&ts_batch).await;
                    ts_batch.clear();
                }
            }
            // Check if ingest is still alive before waiting again
            if out_queue.is_closed()
                || !self
                    .engine
                    .ingests
                    .active
                    .read()
                    .await
                    .contains_key(pipeline_id)
                || matches!(
                    wake,
                    crate::media::ts_chunk_ring::TsChunkWaitResult::Cancelled
                )
            {
                break;
            }
        }

        info!("Feed loop exited for pipeline={}", pipeline_id);
        out_queue.close();
    }
}

#[path = "srt_egress.rs"]
mod srt_egress;
#[cfg(test)]
#[path = "srt_tests.rs"]
mod tests;
use srt_egress::{apply_srt_crypto_socket, srt_crypto_from_resolved};
#[cfg(test)]
use srt_egress::{estimate_ts_accum_capacity, parse_srt_egress_url};
pub use srt_egress::{
    parse_pipeline_srt_ingest_policy, serialize_pipeline_srt_ingest_policy, start_shared_ts_muxer,
    start_srt_egress, teardown_srt,
};
