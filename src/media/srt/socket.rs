use std::net::SocketAddr;
use std::os::raw::{c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{error, info, warn};

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
const DESIRED_SRT_BUF: i32 = 12 * 1024 * 1024;
const DESIRED_FC: i32 = 32768;
pub(super) const SRT_LOG_CRIT: c_int = 2;
const DESIRED_LATENCY_MS: i32 = 250;
const DESIRED_LOSSMAXTTL: i32 = 256;

// --- Egress-specific buffer sizing -----------------------------------------
//
// `srt_set_highbitrate_opts` below (DESIRED_SRT_BUF/DESIRED_UDP_BUF/DESIRED_FC)
// is applied identically to the one ingest listener socket *and* to every
// egress destination socket. That's appropriate for ingest — there is
// exactly one such socket, and it must absorb the single highest-value,
// worst-case-bitrate contribution feed. It is not appropriate for egress,
// which is multiplied by output count and is structurally send-dominant
// (received traffic on an egress socket is ACK/NAK/ACKACK control chatter,
// not media).
//
// Evidence (2026-08-07 investigation, same VPS class as
// docs/agent-guidance/quality/baselines.md's MSR runs):
//   - Telemetry+smaps_rollup at 320 concurrent pure-SRT egress destinations
//     showed restream's own tracked ring buffers (retainedPayloadBytes) at
//     ~2.2 MB total, vs. 493 MB of "unattributed" anonymous/private-dirty
//     RSS (92% of process RSS) — i.e. almost all of it is native libsrt
//     buffer memory invisible to our own accounting, not application state.
//   - That matches an independent, earlier measurement already in
//     baselines.md: "vps-6cpu-12gb, N=100 healthy SRT outputs... per-output
//     RSS 1,500KB" (fabric proof, 2026-07-xx) — this isn't a new regression,
//     it's the same known cost, now root-caused to this constant.
//   - Loss/jitter fault injection (isolated netns + tc netem, see
//     .local/artifacts/mediamtx-forward-bench/unified/loss_jitter_test.py in
//     that investigation) at up to 20% loss / 150ms±40ms jitter / 300
//     concurrent egress destinations produced zero permanent failures with
//     the *unmodified* 12MB/8MB buffers — RSS grew by only ~12-28 KB per
//     connection under load, nowhere near the configured ceiling. That is
//     the evidence base the sizing below is calibrated against: cut the
//     ceiling, but leave several times more headroom than anything observed
//     even under deliberately severe network conditions.
//
// The native SRT send-buffer ceiling set here is not just a memory
// reservation: it is the same value the egress fabric's stall/backpressure
// classification reads (`docs/egress-implementation.md` "Native buffer
// accounting" — `SrtFabricLeaf::pressure` combines application-pending bytes
// with native sender-buffer occupancy from `srt_bistats`). Shrinking it
// too far would make the fabric classify a leaf as backpressured/stalled
// sooner under a real burst, not just save memory — that's why this is
// sized from the same bitrate*latency*margin model the neighboring
// DESIRED_LATENCY_MS/DESIRED_LOSSMAXTTL constants already use (see their
// comments), rather than picked as an arbitrary smaller round number.
//
// Formula (Haivision/SRT-Alliance guidance: buffer >= bitrate * latency,
// with headroom for ARQ retransmission overhead and encoder/network burst):
//   bytes = bitrate_bps * (DESIRED_LATENCY_MS / 1000) * safety_margin / 8
// At the same 50 Mbps worst-case bitrate DESIRED_LOSSMAXTTL's comment already
// assumes, with a 4x safety margin (the conventional top end of published
// SRT sizing guidance): 50_000_000 * 0.25 * 4 / 8 = 6.25 MB — roughly half
// of DESIRED_SRT_BUF, while still comfortably covering the highest bitrate
// this repo documents testing (docs/mahashivratri-hero-scenario.md's 40 Mbps
// bitrate envelope).
const EGRESS_SAFETY_MARGIN: i64 = 4;
const EGRESS_SNDBUF_FLOOR: i32 = 2 * 1024 * 1024; // covers low-bitrate/audio-only egress
const EGRESS_SNDBUF_CEILING: i32 = DESIRED_SRT_BUF; // never exceed the ingest-derived ceiling
const EGRESS_DEFAULT_ASSUMED_BITRATE_BPS: i64 = 50_000_000; // matches DESIRED_LOSSMAXTTL's assumption
const EGRESS_UDP_RCVBUF: i32 = 1024 * 1024; // control-only traffic on a send-dominant socket
const EGRESS_SRT_RCVBUF: i32 = 1024 * 1024; // ditto, at the SRT application-buffer layer

/// Right-sized SRT send-buffer ceiling for one egress destination. Pass the
/// output's known/configured bitrate when available; `None` falls back to
/// the same worst-case bitrate assumption DESIRED_LOSSMAXTTL already bakes
/// in, so an unknown-bitrate output is no worse off than today's flat
/// preset — it's still bounded, just no longer multiplied by 12MB per
/// output regardless of what that output actually carries.
///
/// Not currently wired to a live per-output bitrate anywhere. Two things
/// were checked before deciding to stop at the static default + URL
/// override instead of building that plumbing (2026-08-07 investigation):
///
/// 1. **Can this be resized after connect, adapting to observed live
///    throughput, instead of guessed pre-connect?** No — checked directly
///    against the vendored libsrt source
///    (`.local/build/static/src/srt/srtcore/core.cpp`'s option-restriction
///    table): `SRTO_SNDBUF`/`SRTO_RCVBUF`/`SRTO_UDP_SNDBUF`/
///    `SRTO_UDP_RCVBUF` are all flagged `SRTO_R_PREBIND` — libsrt rejects
///    `srt_setsockopt` for these once the socket is bound/connected. Any
///    "dynamic" derivation is necessarily a pre-connect estimate, not a
///    live-adapting one.
/// 2. **Is there a usable bitrate estimate available pre-connect today?**
///    Not by default. Built-in transcode presets (`media::profiles`,
///    720p/1080p/h264) all ship `bitrate: 0, max_bitrate: 0` — CRF mode,
///    genuinely variable, no fixed target. Passthrough (`source`) outputs
///    have no encoder step at all. Only an operator-authored custom
///    profile with an explicit nonzero `bitrate`/`max_bitrate` gives a
///    reliable static number, and threading that from the profile
///    registry through `OutputSpec` down to this pre-connect socket setup
///    call is real cross-module plumbing for a minority case.
///
/// Given both, the lower-risk/higher-value lever is the explicit URL
/// override in `srt_url.rs` (`sndbuf=<bytes>` query parameter): an
/// operator who *knows* a specific destination needs more (or less)
/// headroom can ask for it on that one output instead of every caller
/// paying the worst-case default. `bitrate_bps` stays `Some(...)`-capable
/// on this function so a future custom-profile-bitrate plumbing pass has
/// somewhere to call into without changing this signature again.
pub(super) fn srt_egress_sndbuf_bytes(bitrate_bps: Option<i64>) -> i32 {
    let bitrate = bitrate_bps
        .filter(|b| *b > 0)
        .unwrap_or(EGRESS_DEFAULT_ASSUMED_BITRATE_BPS);
    let bytes = bitrate
        .saturating_mul(DESIRED_LATENCY_MS as i64)
        .saturating_mul(EGRESS_SAFETY_MARGIN)
        / (1000 * 8);
    bytes
        .min(EGRESS_SNDBUF_CEILING as i64)
        .max(EGRESS_SNDBUF_FLOOR as i64) as i32
}

/// Every per-destination SRT socket option an egress connection can carry,
/// resolved once (formula defaults, then any explicit URL overrides) before
/// connect — all of `SRTO_SNDBUF`/`RCVBUF`/`LATENCY`/`MAXBW`/`FC` are `PRE`
/// or `PREBIND` in libsrt (see the "Egress-specific buffer sizing" block
/// above), so there is nothing to resolve *after* connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EgressBufferOpts {
    pub(super) sndbuf_bytes: i32,
    pub(super) rcvbuf_bytes: i32,
    pub(super) latency_ms: i32,
    pub(super) maxbw_bps: i64,
    pub(super) fc_pkts: i32,
}

impl EgressBufferOpts {
    /// Formula/constant defaults for an output whose destination didn't ask
    /// for anything different. `bitrate_bps` feeds only the SNDBUF formula
    /// (see `srt_egress_sndbuf_bytes`); the rest are the same constants
    /// every egress socket used before per-destination overrides existed.
    pub(super) fn defaults(bitrate_bps: Option<i64>) -> Self {
        Self {
            sndbuf_bytes: srt_egress_sndbuf_bytes(bitrate_bps),
            rcvbuf_bytes: EGRESS_SRT_RCVBUF,
            latency_ms: DESIRED_LATENCY_MS,
            maxbw_bps: -1, // unlimited/relative — libsrt paces to the receiver's ACKed rate
            fc_pkts: DESIRED_FC,
        }
    }

    /// Applies explicit `sndbuf=`/`rcvbuf=`/`latency=`/`maxbw=`/`fc=` URL
    /// overrides (each `None` when the query param was absent or
    /// unparseable) on top of the resolved defaults.
    pub(super) fn with_overrides(
        mut self,
        sndbuf_bytes: Option<i32>,
        rcvbuf_bytes: Option<i32>,
        latency_ms: Option<i32>,
        maxbw_bps: Option<i64>,
        fc_pkts: Option<i32>,
    ) -> Self {
        if let Some(v) = sndbuf_bytes {
            self.sndbuf_bytes = v;
        }
        if let Some(v) = rcvbuf_bytes {
            self.rcvbuf_bytes = v;
        }
        if let Some(v) = latency_ms {
            self.latency_ms = v;
        }
        if let Some(v) = maxbw_bps {
            self.maxbw_bps = v;
        }
        if let Some(v) = fc_pkts {
            self.fc_pkts = v;
        }
        self
    }
}

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

pub(super) fn check_sysctl_limits() {
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

pub(super) fn srt_set_highbitrate_opts(sock: SRTSOCKET) {
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

/// Egress-only counterpart to `srt_set_highbitrate_opts` (used for the one
/// ingest listener socket, which stays on the flat high-bitrate preset — see
/// the "Egress-specific buffer sizing" block above for why the two need to
/// diverge). `sndbuf_bytes` is normally `srt_egress_sndbuf_bytes(bitrate)`,
/// or an explicit `sndbuf=` URL override; RCVBUF is fixed small on both the
/// UDP and SRT layers because an egress socket only ever receives small
/// ACK/NAK/ACKACK control packets, never media.
pub(super) fn srt_set_egress_opts(sock: SRTSOCKET, opts: &EgressBufferOpts) {
    // SAFETY: All srt_setsockopt calls use correctly-sized stack-allocated
    // option values with valid SRT socket handles. The buffer sizes, flow
    // control window, and latency values are within platform limits.
    unsafe {
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

        // FC defaults to the same ceiling as ingest: it's normally
        // non-binding (at 1316B/packet, 32768 packets is ~43MB, well above
        // even the default 12MB byte-capped ceiling), so the default costs
        // nothing. Overridable because a caller who also lowers `sndbuf`
        // far enough, or raises `maxbw`, can make FC the real limit instead.
        let fc: c_int = opts.fc_pkts;
        srt_setsockopt(
            sock,
            0,
            SRTO_FC,
            &fc as *const _ as *const c_void,
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

pub(super) fn srt_log_effective_opts(sock: SRTSOCKET, label: &str) -> u64 {
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
        if udp_rcv < DESIRED_UDP_BUF {
            error!(
                "[srt] WARNING: {} UDP recv buffer clamped to {}KB (wanted {}KB). \
                 Raise net.core.rmem_max",
                label,
                udp_rcv / 1024,
                DESIRED_UDP_BUF / 1024,
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

#[cfg(test)]
mod egress_buffer_sizing_tests {
    use super::*;

    #[test]
    fn unknown_bitrate_falls_back_to_worst_case_default() {
        // 50 Mbps * 250ms * 4x margin / 8 = 6.25 MB — matches
        // DESIRED_LOSSMAXTTL's documented 50 Mbps worst-case assumption.
        assert_eq!(srt_egress_sndbuf_bytes(None), 6_250_000);
    }

    #[test]
    fn zero_or_negative_bitrate_is_treated_as_unknown() {
        assert_eq!(srt_egress_sndbuf_bytes(Some(0)), 6_250_000);
        assert_eq!(srt_egress_sndbuf_bytes(Some(-1)), 6_250_000);
    }

    #[test]
    fn low_bitrate_clamps_to_the_floor_not_zero() {
        // A trickle audio-only output should never get a near-zero buffer.
        assert_eq!(srt_egress_sndbuf_bytes(Some(64_000)), EGRESS_SNDBUF_FLOOR);
    }

    #[test]
    fn high_bitrate_clamps_to_the_ingest_derived_ceiling() {
        // Well above the documented 40-50 Mbps envelope this repo tests;
        // must never exceed what ingest itself uses (DESIRED_SRT_BUF).
        assert_eq!(
            srt_egress_sndbuf_bytes(Some(1_000_000_000)),
            EGRESS_SNDBUF_CEILING
        );
        assert_eq!(EGRESS_SNDBUF_CEILING, DESIRED_SRT_BUF);
    }

    #[test]
    fn documented_bitrate_envelope_stays_under_current_flat_preset() {
        // docs/mahashivratri-hero-scenario.md's highest tested envelope
        // (40 Mbps) should size well under the old flat 12MB preset, which
        // was this test's whole point: it was never tightly derived for
        // egress, just safely oversized.
        let bytes = srt_egress_sndbuf_bytes(Some(40_000_000));
        assert!(bytes < DESIRED_SRT_BUF);
        assert!(bytes >= EGRESS_SNDBUF_FLOOR);
    }
}
