use std::net::SocketAddr;
use std::os::raw::{c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{error, info, warn};

use super::buffer_sizing::EgressBufferOpts;
use super::sys::*;

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

pub(super) fn last_srt_error() -> (c_int, String) {
    let mut location = 0;
    // SAFETY: srt_getlasterror writes the optional source-location code to
    // `location`; srt_getlasterror_str returns a thread-local static string.
    let code = unsafe { srt_getlasterror(&mut location) };
    let message = unsafe { std::ffi::CStr::from_ptr(srt_getlasterror_str()) }
        .to_string_lossy()
        .into_owned();
    (code, message)
}

pub(super) fn check_srt_option_result(option: &str, result: c_int) -> Result<(), String> {
    if result >= 0 {
        return Ok(());
    }
    let (code, message) = last_srt_error();
    Err(format!("failed to set {option}: {message} ({code})"))
}

pub const DESIRED_UDP_BUF: i32 = 8 * 1024 * 1024;
pub(super) const DESIRED_SRT_BUF: i32 = 12 * 1024 * 1024;
pub(super) const DESIRED_FC: i32 = 32768;
pub(super) const SRT_LOG_CRIT: c_int = 2;
pub(super) const DESIRED_LATENCY_MS: i32 = 250;
const DESIRED_LOSSMAXTTL: i32 = 256;

pub(super) fn enable_srt_group_connect(listener: SRTSOCKET) -> Result<(), String> {
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

pub(super) fn check_sysctl_limits(udp_buf: i32) {
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
        udp_buf as usize,
        "net.core.rmem_max",
    );
    check(
        "/proc/sys/net/core/wmem_max",
        udp_buf as usize,
        "net.core.wmem_max",
    );
}

pub(super) fn srt_set_highbitrate_opts(sock: SRTSOCKET, udp_buf: i32) {
    // SAFETY: All srt_setsockopt calls use correctly-sized stack-allocated
    // option values with valid SRT socket handles. The UDP/SRT buffer sizes,
    // flow control window, and latency values are within platform limits.
    unsafe {
        let latency: c_int = DESIRED_LATENCY_MS;
        srt_setsockopt(
            sock,
            0,
            SRTO_LATENCY,
            &latency as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );

        let lossmaxttl: c_int = DESIRED_LOSSMAXTTL;
        srt_setsockopt(
            sock,
            0,
            SRTO_LOSSMAXTTL,
            &lossmaxttl as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );

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

        // FC must be set before SNDBUF/RCVBUF: libsrt documents that the
        // receiver (and, per SRTO_SNDBUF's own "see RCVBUF" cross-reference,
        // sender) buffer size must not exceed FC in packet-count terms, and
        // recommends setting FC first (see the shared latency/FC doc block
        // above `SRT_LATENCY_MS_FLOOR`). DESIRED_SRT_BUF/SRT_PAYLOAD_SIZE_BYTES
        // already fits comfortably under DESIRED_FC (~8645 buffers vs 32768
        // packets), so this reorder changes no behavior here — it just stops
        // this function from being an example of the wrong order to copy.
        let fc: c_int = DESIRED_FC;
        srt_setsockopt(
            sock,
            0,
            SRTO_FC,
            &fc as *const _ as *const c_void,
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
pub(crate) const EGRESS_UDP_RCVBUF: i32 = 1024 * 1024; // control-only traffic on a send-dominant socket

/// Egress-only counterpart to `srt_set_highbitrate_opts` (used for the one
/// ingest listener socket, which stays on the flat high-bitrate preset — see
/// `buffer_sizing.rs`'s "Egress-specific buffer sizing" block for why the two
/// need to diverge). `sndbuf_bytes` is normally `srt_egress_sndbuf_bytes(bitrate)`,
/// or an explicit `sndbuf=` URL override; RCVBUF is fixed small on both the
/// UDP and SRT layers because an egress socket only ever receives small
/// ACK/NAK/ACKACK control packets, never media.
pub(super) fn srt_set_egress_opts(sock: SRTSOCKET, opts: &EgressBufferOpts) {
    // SAFETY: All srt_setsockopt calls use correctly-sized stack-allocated
    // option values with valid SRT socket handles. The buffer sizes, flow
    // control window, and latency values are within platform limits.
    unsafe {
        // Set LIVE transmission type so the egress socket can connect to
        // sink-mode peers that require LIVE mode (SRTO_TRANSTYPE=SRTT_LIVE).
        // Without this, clients default to a non-LIVE mode and the SRT
        // handshake fails with MESSAGEAPI reject on the sink side.
        let live_mode: c_int = SRTT_LIVE;
        srt_setsockopt(
            sock,
            0,
            SRTO_TRANSTYPE,
            &live_mode as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );
        let latency: c_int = opts.latency_ms;
        srt_setsockopt(
            sock,
            0,
            SRTO_LATENCY,
            &latency as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );

        let lossmaxttl: c_int = DESIRED_LOSSMAXTTL;
        srt_setsockopt(
            sock,
            0,
            SRTO_LOSSMAXTTL,
            &lossmaxttl as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );

        // Send side keeps the same generous kernel UDP buffer as ingest —
        // this is genuinely on the transmit hot path and untested as a
        // reduction target in this pass. Receive side does not need it.
        let udp_sndbuf: c_int = DESIRED_UDP_BUF;
        srt_setsockopt(
            sock,
            0,
            SRTO_UDP_SNDBUF,
            &udp_sndbuf as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );
        let udp_rcvbuf: c_int = EGRESS_UDP_RCVBUF;
        srt_setsockopt(
            sock,
            0,
            SRTO_UDP_RCVBUF,
            &udp_rcvbuf as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );

        // FC must be set before SNDBUF/RCVBUF: libsrt documents that the
        // sender/receiver buffer size must not exceed FC in packet-count
        // terms, and recommends setting FC first (see the shared
        // latency/FC doc block above `SRT_LATENCY_MS_FLOOR`).
        // `EgressBufferOpts::with_overrides` already derives `fc_pkts` so
        // it is always >= `sndbuf_bytes`/`rcvbuf_bytes` in packet-count
        // terms for whatever `latency_ms` ended up in force, so this default
        // (`DESIRED_FC` when neither `latency` nor `fc` were overridden)
        // costs nothing.
        let fc: c_int = opts.fc_pkts;
        srt_setsockopt(
            sock,
            0,
            SRTO_FC,
            &fc as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );

        let srt_sndbuf: c_int = opts.sndbuf_bytes;
        srt_setsockopt(
            sock,
            0,
            SRTO_SNDBUF,
            &srt_sndbuf as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );
        let srt_rcvbuf: c_int = opts.rcvbuf_bytes;
        srt_setsockopt(
            sock,
            0,
            SRTO_RCVBUF,
            &srt_rcvbuf as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );

        let maxbw: i64 = opts.maxbw_bps;
        srt_setsockopt(
            sock,
            0,
            SRTO_MAXBW,
            &maxbw as *const _ as *const c_void,
            std::mem::size_of::<i64>() as c_int,
        );
    }
}

pub fn srt_set_connect_timeout(sock: SRTSOCKET, timeout_ms: u64) {
    let timeout_ms = timeout_ms.min(c_int::MAX as u64) as c_int;
    // SAFETY: srt_setsockopt writes a bounded integer timeout to a valid SRT
    // socket. The timeout value comes from validated process config.
    unsafe {
        srt_setsockopt(
            sock,
            0,
            SRTO_CONNTIMEO,
            &timeout_ms as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );
    }
}

/// Reads back the effective `SRTO_SNDBUF` libsrt actually holds for this
/// socket. `SRTO_SNDBUF` is `SRTO_R_PREBIND` in libsrt (confirmed against
/// the vendored source, `srtcore/core.cpp`'s `s_perm[]` option table) — it
/// cannot change after connect, so a single read at leaf-creation time is
/// authoritative for the socket's whole lifetime; no need to re-read it on
/// the hot per-second quality-sampling path.
pub(crate) fn srt_get_configured_sndbuf(sock: SRTSOCKET) -> i32 {
    let mut value = 0i32;
    let mut len = std::mem::size_of::<c_int>() as c_int;
    // SAFETY: srt_getsockopt reads an integer option value from a valid SRT
    // socket into a correctly-sized stack variable. Benign diagnostic read.
    unsafe {
        srt_getsockopt(
            sock,
            0,
            SRTO_SNDBUF,
            &mut value as *mut _ as *mut c_void,
            &mut len,
        );
    }
    value
}

pub(super) fn srt_log_effective_opts(sock: SRTSOCKET, label: &str, expected_udp_rcv: i32) -> u64 {
    // SAFETY: srt_getsockopt reads integer option values from a valid SRT
    // socket into correctly-sized stack variables. All options are benign
    // diagnostic reads with no side effects on the socket.
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
        if udp_rcv < expected_udp_rcv {
            error!(
                "[srt] WARNING: {} UDP recv buffer clamped to {}KB (wanted {}KB). \
                 Raise net.core.rmem_max",
                label,
                udp_rcv / 1024,
                expected_udp_rcv / 1024,
            );
        }
        udp_rcv.max(0) as u64
    }
}

pub(super) fn to_sockaddr_in(addr: SocketAddr) -> sockaddr_in {
    let ip = match addr.ip() {
        std::net::IpAddr::V4(ipv4) => u32::from_ne_bytes(ipv4.octets()),
        _ => 0,
    };
    sockaddr_in {
        sin_family: 2,
        sin_port: addr.port().to_be(),
        sin_addr: ip,
        sin_zero: [0; 8],
    }
}

pub(super) fn from_sockaddr_in(addr: sockaddr_in) -> SocketAddr {
    SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::from(addr.sin_addr.to_ne_bytes())),
        u16::from_be(addr.sin_port),
    )
}

pub(super) fn is_srt_group(socket: SRTSOCKET) -> bool {
    socket & SRTGROUP_MASK != 0
}

pub(super) fn streamid_from_getsockopt_buffer(buf: &[u8], optlen: c_int) -> Option<String> {
    if optlen < 0 {
        return None;
    }
    let len = usize::try_from(optlen).ok()?;
    if len > buf.len() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&buf[..len])
            .trim_matches('\0')
            .to_string(),
    )
}

pub(super) struct SrtListenerCloser {
    socket: SRTSOCKET,
    closed: AtomicBool,
}

impl SrtListenerCloser {
    pub(super) fn new(socket: SRTSOCKET) -> Self {
        Self {
            socket,
            closed: AtomicBool::new(false),
        }
    }

    pub(super) fn close_once(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            // SAFETY: close_once owns the listener-close responsibility and
            // prevents double-close races between explicit shutdown and Drop.
            unsafe {
                srt_close(self.socket);
            }
        }
    }
}

impl Drop for SrtListenerCloser {
    fn drop(&mut self) {
        self.close_once();
    }
}

pub(super) struct SrtSocketGuard(SRTSOCKET);

impl SrtSocketGuard {
    pub(super) fn new(socket: SRTSOCKET) -> Self {
        Self(socket)
    }
}

impl Drop for SrtSocketGuard {
    fn drop(&mut self) {
        // SAFETY: construction transfers one live socket into this guard,
        // which never exposes or duplicates close ownership.
        unsafe {
            srt_close(self.0);
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SrtGroupSummary {
    pub(super) member_count: u32,
    pub(super) connected_members: u32,
    pub(super) active_members: u32,
    pub(super) broken_members: u32,
}

pub(super) fn summarize_group_members(members: &[SrtSocketGroupData]) -> SrtGroupSummary {
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

pub(super) fn srt_group_summary(group: SRTSOCKET) -> Option<SrtGroupSummary> {
    const MAX_GROUP_MEMBERS: usize = 64;
    // SAFETY: std::mem::zeroed() for C structs is valid when the struct
    // has no invalid bit patterns (all-zero is a valid SrtSocketGroupData).
    // srt_group_data fills the array through a raw pointer; members is
    // correctly sized and aligned.
    let mut members: Vec<SrtSocketGroupData> = (0..MAX_GROUP_MEMBERS)
        .map(|_| unsafe { std::mem::zeroed() })
        .collect();
    let mut member_count = members.len();
    // SAFETY: members is initialized, aligned storage for member_count
    // entries, and libsrt writes no more than the supplied capacity.
    let result = unsafe { srt_group_data(group, members.as_mut_ptr(), &mut member_count) };
    if result < 0 {
        return None;
    }
    members.truncate(member_count.min(members.len()));
    Some(summarize_group_members(&members))
}

pub(super) fn add_srt_group_quality(
    quality: &mut crate::media::snapshots::PublisherQuality,
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

pub(super) fn try_acquire_srt_sender_permit(
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
) -> Result<tokio::sync::OwnedSemaphorePermit, tokio::sync::TryAcquireError> {
    semaphore.try_acquire_owned()
}
