//! Per-destination egress SRT buffer/FC/latency sizing policy
//! (`EgressBufferOpts`), plus ingest's own latency-scaled receive-buffer
//! sizing (`srt_set_ingest_latency_opts`). Split out of `socket.rs` to stay
//! under the source-audit line cap — see that file for the general SRT
//! socket FFI helpers (crypto, group membership, listener/socket guards,
//! etc.) this module does not own.
//!
//! There is still no *per-caller* ingest equivalent to egress's URL
//! overrides: `SNDBUF`/`RCVBUF`/`FC` are never wire-negotiated (a caller's
//! own `srt://...?rcvbuf=...` URL only configures *their* socket, never
//! ours), and there is no standard SRT mechanism — via streamid or
//! otherwise — for a caller to influence the listener's own values. What
//! ingest does offer is an *operator*-controlled lever
//! (`SrtGlobalIngestConfig`/`SrtPipelineIngestConfig::latency_ms`, resolved
//! in `media::srt_policy` and applied per accepted connection in
//! `media::srt::listener`) — the same trust boundary as egress's URL
//! params (operator configures it, operator bears the consequences), just
//! applied to the listener's own `SRTO_RCVLATENCY` instead of anything a
//! caller supplies. `latency` reaches the negotiated result for free either
//! way, through real handshake negotiation (`SRTO_PEERLATENCY`) — what this
//! module adds is sizing `RCVBUF`/`FC` to actually hold what that latency
//! implies, instead of a flat preset unrelated to it.

use std::os::raw::{c_int, c_void};

use super::socket::{DESIRED_FC, DESIRED_LATENCY_MS, DESIRED_SRT_BUF};
use super::sys::*;

// --- SRT latency: shared ingest/egress bounds -------------------------------
//
// `SRTO_LATENCY`/`SRTO_RCVLATENCY`/`SRTO_PEERLATENCY` are documented in
// milliseconds (`.local/build/static/src/srt/docs/API/API-socket-options.md`)
// and control the TSBPD (TimeStamp-Based Packet Delivery) delay: a packet is
// held until `PTS = ETS + latency`, and — when TLPKTDROP is enabled, the
// live-mode default — a packet that misses that deadline is skipped
// ("too-late packet drop") rather than delivered late
// (`docs/API/API-socket-options.md`'s `SRTO_TLPKTDROP` entry). Larger
// latency tolerates more jitter/loss/retransmission time before a packet
// counts as too late; smaller latency lowers end-to-end delay at the cost of
// that headroom.
//
// The wire protocol itself bounds the negotiated TSBPD delay field to
// `{20..8000}` — that exact range appears twice in
// `.local/build/static/src/srt/docs/features/handshake.md`: once for HSv4's
// combined `TsbPdDelay` field, once for HSv5's separate
// `RcvTsbPdDelay`/`SndTsbPdDelay` fields. This is not a value this repo
// invented; it is the field's own documented operating range. (libsrt's
// `setOpt` does not independently enforce it — checked, no reference to 8000
// anywhere in `srtcore/*.cpp` — so a value outside it would not be rejected,
// just undocumented territory.)
const SRT_LATENCY_MS_FLOOR: i32 = 20;
const SRT_LATENCY_MS_CEILING: i32 = 8_000;

// The receiver's buffer must hold the full latency window's worth of data:
// "The receiver's buffer must be large enough to store the L segment of the
// stream, i.e. L × Bitrate bytes" (`API-socket-options.md`'s
// `SRTO_RCVLATENCY` entry), and `SRTO_SNDBUF`'s own entry says "See
// SRTO_RCVBUF for more information" — the same sizing logic governs both
// directions. Also documented there: "There is a restriction that the
// receiver buffer size (SRTO_RCVBUF) must not be greater than SRTO_FC ...
// it is recommended to set the value of SRTO_FC first, and then the value
// of SRTO_RCVBUF" — every function below that touches both options sets FC
// first for this reason. `SRT_PAYLOAD_SIZE_BYTES` converts a byte ceiling
// into the packet count FC needs to stay above: default MSS 1500 minus 28
// (IP+UDP) and 16 (SRT header) bytes of overhead = 1456, matching both
// RCVBUF's own worked example ("32 buffers (46592 with default SRTO_MSS)" =
// 46592/32 = 1456) and the FC sizing formula's own "(MSS - 44)" denominator.
const SRT_PAYLOAD_SIZE_BYTES: i32 = 1456;
// libsrt converts SRTO_RCVBUF bytes to packets using MSS minus the UDP
// header, not the media payload size used for FC's conservative ceiling.
const SRT_RCVBUF_PACKET_SIZE_BYTES: i32 = 1472;

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
const EGRESS_DEFAULT_ASSUMED_BITRATE_BPS: i64 = 50_000_000; // matches DESIRED_LOSSMAXTTL's assumption
// SRTO_UDP_RCVBUF lives with `srt_set_egress_opts` in socket.rs (the kernel
// UDP layer, not this module's application-layer sizing policy).
const EGRESS_SRT_RCVBUF: i32 = 1024 * 1024; // control-only traffic on a send-dominant socket

// Override clamp ranges (see `EgressBufferOpts::with_overrides`). An output
// URL is operator/API-configured, not anonymous wire input like ingest's
// streamid, but it's still an unvalidated i32 today: nothing stops a typo
// or an untrusted upstream config source from asking for gigabytes per
// destination, and that cost multiplies by output count. Bounding it here
// costs nothing for any legitimate use this repo documents testing.
const EGRESS_RCVBUF_OVERRIDE_FLOOR: i32 = 64 * 1024;
const EGRESS_RCVBUF_OVERRIDE_CEILING: i32 = 4 * 1024 * 1024; // control-only; far below SNDBUF's ceiling
// Must stay >= EGRESS_SNDBUF_FLOOR / SRT_PAYLOAD_SIZE_BYTES (2MB / 1456 ≈
// 1441 packets): SNDBUF/RCVBUF must not exceed FC in packet-count terms
// (shared latency/FC doc block above), so if FC's own floor were allowed to
// sit below what SNDBUF's floor needs, `with_overrides` would have no valid
// value to pick for a caller who pins FC low. 1500 is a round number safely
// above that minimum.
const EGRESS_FC_OVERRIDE_FLOOR: i32 = 1_500;

/// Right-sized SRT send-buffer ceiling for one egress destination at a given
/// effective latency. Pass the output's known/configured bitrate when
/// available; `None` falls back to the same worst-case bitrate assumption
/// DESIRED_LOSSMAXTTL already bakes in, so an unknown-bitrate output is no
/// worse off than today's flat preset — it's still bounded, just no longer
/// multiplied by 12MB per output regardless of what that output actually
/// carries.
///
/// Formula (Haivision/SRT-Alliance guidance, and directly confirmed by
/// libsrt's own docs — `SRTO_RCVLATENCY`'s entry in
/// `.local/build/static/src/srt/docs/API/API-socket-options.md`: "The
/// receiver's buffer must be large enough to store the L segment of the
/// stream, i.e. L × Bitrate bytes" — buffer >= bitrate * latency, with
/// headroom for ARQ retransmission overhead and encoder/network burst):
///   bytes = bitrate_bps * (latency_ms / 1000) * safety_margin / 8
/// At the documented 50 Mbps worst-case bitrate assumption with a 4x safety
/// margin (the conventional top end of published SRT sizing guidance) and
/// the 250ms default latency: 50_000_000 * 0.25 * 4 / 8 = 6.25 MB — the
/// same value this formula has always produced at the default latency,
/// unchanged from before per-destination `latency=` overrides existed.
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
pub(super) fn srt_egress_sndbuf_bytes(bitrate_bps: Option<i64>, latency_ms: i32) -> i32 {
    let bitrate = bitrate_bps
        .filter(|b| *b > 0)
        .unwrap_or(EGRESS_DEFAULT_ASSUMED_BITRATE_BPS);
    let bytes = bitrate
        .saturating_mul(latency_ms as i64)
        .saturating_mul(EGRESS_SAFETY_MARGIN)
        / (1000 * 8);
    bytes
        .clamp(
            EGRESS_SNDBUF_FLOOR as i64,
            latency_scaled_buffer_ceiling_bytes(latency_ms) as i64,
        )
        .max(EGRESS_SNDBUF_FLOOR as i64) as i32
}

/// The SNDBUF ceiling for a given effective latency: the worst-case-bitrate
/// formula's own output at that latency, floored at the pre-per-destination-
/// override flat preset (`DESIRED_SRT_BUF`) so a caller who leaves `latency`
/// at its default sees exactly the same ceiling as before this formula
/// became latency-aware.
///
/// A caller who legitimately requests more latency (a satellite/high-RTT
/// contribution link, say) gets a ceiling that scales up to match. The
/// alternative — a ceiling fixed regardless of latency — would silently
/// under-buffer exactly the requests this parameter exists to serve:
/// `SRTO_RCVLATENCY`'s own doc entry is explicit that the buffer must hold
/// `latency * bitrate` bytes, so capping it at a small fixed number while
/// honoring a caller's genuinely higher latency would reintroduce
/// too-late-drop as a buffer-capacity artifact instead of a real lateness
/// signal — silently defeating the very setting the caller asked for.
fn latency_scaled_buffer_ceiling_bytes(latency_ms: i32) -> i32 {
    let formula = EGRESS_DEFAULT_ASSUMED_BITRATE_BPS
        .saturating_mul(latency_ms as i64)
        .saturating_mul(EGRESS_SAFETY_MARGIN)
        / (1000 * 8);
    formula.max(DESIRED_SRT_BUF as i64) as i32
}

/// The FC ceiling for a given effective latency, derived from the SNDBUF
/// ceiling at that same latency so the two can never end up mismatched
/// (`SRTO_SNDBUF`/`SRTO_RCVBUF` must not exceed `SRTO_FC` in packet-count
/// terms — see the shared latency/FC doc block above) regardless of what
/// latency produced that SNDBUF ceiling. Floored at `DESIRED_FC` so a
/// caller who leaves `latency` at its default sees no change.
fn latency_scaled_fc_ceiling_pkts(latency_ms: i32) -> i32 {
    (latency_scaled_buffer_ceiling_bytes(latency_ms) / SRT_PAYLOAD_SIZE_BYTES).max(DESIRED_FC)
}

/// Applies this pipeline's resolved SRT ingest latency
/// (`SrtGlobalIngestConfig`/`SrtPipelineIngestConfig::latency_ms`, resolved
/// in `media::srt_policy`) to an accepted-but-not-yet-`open()`ed socket:
/// `SRTO_RCVLATENCY`, plus an `RCVBUF`/`FC` pair sized from the *same*
/// latency-scaled formula egress's `SNDBUF` ceiling uses (worst-case
/// assumed bitrate × latency × margin, floored at the historical flat
/// preset — see `latency_scaled_buffer_ceiling_bytes`). Must run inside the
/// accept-hook callback (`listener.rs`'s `srt_listener_policy_callback_inner`),
/// the same PREBIND window `EgressBufferOpts`'s doc block explains — and for
/// the same reason, this can only ever be sized from *our own* configured
/// latency, never the value actually negotiated with the caller:
/// `SRTO_RCVBUF` is locked before `acceptAndRespond` (which processes the
/// peer's proposed `PEERLATENCY` and computes
/// `max(local RCVLATENCY, peer PEERLATENCY)`) ever runs — confirmed
/// directly against the vendored libsrt source
/// (`.local/build/static/src/srt/srtcore/core.cpp`'s `acceptAndRespond`:
/// `interpretSrtHandshake`, which negotiates latency, precedes
/// `prepareBuffers`, which allocates the receive buffer from whatever
/// `SRTO_RCVBUF` was set to, but that value was already locked at PREBIND
/// time, long before either call). A caller who proposes a higher
/// `PEERLATENCY` than we sized for can still push the negotiated result
/// above our buffer's capacity — libsrt's own `processSrtMsg_HSREQ` does no
/// range validation on the peer's proposed value — and nothing on our side
/// can close that gap; it is a protocol-inherent limit, not a bug here.
/// Floored below at `DESIRED_SRT_BUF`/`DESIRED_FC` (this repo's proven-safe
/// baseline for the single always-on ingest socket), and can only grow
/// above that for a caller/pipeline genuinely configured for higher
/// latency — never shrinks the default, since ingest is a single fixed
/// cost, not multiplied by output count the way egress is, so there is no
/// memory-pressure reason to make the common case smaller.
///
/// FC is applied before RCVBUF, matching libsrt's documented required
/// order (see the shared latency/FC doc block above), and RCVBUF is
/// additionally clamped to fit under the resolved FC so the
/// `RCVBUF <= FC` invariant holds even though neither value here is
/// user-supplied (both are formula-derived, but the formula's rounding
/// could otherwise let RCVBUF's byte figure exceed FC's packet-count
/// figure at the margins).
///
/// Pure resolution, split out from `srt_set_ingest_latency_opts` so the
/// clamp/formula math is directly unit/property-testable without a real
/// socket. Returns `(clamped_latency_ms, fc_pkts, rcvbuf_bytes)`.
pub(super) fn resolve_ingest_latency_opts(latency_ms: i32) -> (i32, i32, i32) {
    let latency_ms = latency_ms.clamp(SRT_LATENCY_MS_FLOOR, SRT_LATENCY_MS_CEILING);
    let fc_pkts = latency_scaled_fc_ceiling_pkts(latency_ms);
    let rcvbuf_bytes = latency_scaled_buffer_ceiling_bytes(latency_ms)
        .min(fc_pkts.saturating_mul(SRT_PAYLOAD_SIZE_BYTES));
    (latency_ms, fc_pkts, rcvbuf_bytes)
}

/// Resolve the packet capacities consumed by the Rust SRT core from the same
/// policy that drives native libsrt's `SRTO_FC` and `SRTO_RCVBUF`. The native
/// implementation converts the receive-buffer byte value with `MSS - UDP
/// header` and then caps it by FC; preserving that order keeps the two
/// backends on the same effective packet window without putting native FFI in
/// the protocol crate.
pub(super) fn resolve_ingest_buffer_packets(latency_ms: i32) -> (u32, u32) {
    let (_, fc_pkts, rcvbuf_bytes) = resolve_ingest_latency_opts(latency_ms);
    let receive_buffer_packets = (rcvbuf_bytes / SRT_RCVBUF_PACKET_SIZE_BYTES)
        .clamp(32, fc_pkts)
        .unsigned_abs();
    (fc_pkts as u32, receive_buffer_packets)
}

pub(super) fn srt_set_ingest_latency_opts(sock: SRTSOCKET, latency_ms: i32) {
    let (latency_ms, fc_pkts, rcvbuf_bytes) = resolve_ingest_latency_opts(latency_ms);

    // SAFETY: All srt_setsockopt calls use correctly-sized stack-allocated
    // option values with a valid, not-yet-opened accepted SRT socket.
    unsafe {
        srt_setsockopt(
            sock,
            0,
            SRTO_FC,
            &fc_pkts as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );
        srt_setsockopt(
            sock,
            0,
            SRTO_RCVBUF,
            &rcvbuf_bytes as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );
        srt_setsockopt(
            sock,
            0,
            SRTO_RCVLATENCY,
            &latency_ms as *const _ as *const c_void,
            std::mem::size_of::<c_int>() as c_int,
        );
    }
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
    /// The bitrate assumption `sndbuf_bytes` was last computed from — not
    /// itself a wire socket option, kept only so `with_overrides` can
    /// correctly re-derive `sndbuf_bytes`/`fc_pkts` when a `latency=`
    /// override changes the effective latency (both formulas are
    /// latency-dependent; see the shared latency/FC doc block above).
    assumed_bitrate_bps: Option<i64>,
}

impl EgressBufferOpts {
    /// Formula/constant defaults for an output whose destination didn't ask
    /// for anything different. `bitrate_bps` feeds only the SNDBUF formula
    /// (see `srt_egress_sndbuf_bytes`); the rest are the same constants
    /// every egress socket used before per-destination overrides existed.
    pub(super) fn defaults(bitrate_bps: Option<i64>) -> Self {
        let latency_ms = DESIRED_LATENCY_MS;
        Self {
            sndbuf_bytes: srt_egress_sndbuf_bytes(bitrate_bps, latency_ms),
            rcvbuf_bytes: EGRESS_SRT_RCVBUF,
            latency_ms,
            maxbw_bps: -1, // unlimited/relative — libsrt paces to the receiver's ACKed rate
            fc_pkts: DESIRED_FC,
            assumed_bitrate_bps: bitrate_bps,
        }
    }

    /// Applies explicit `sndbuf=`/`rcvbuf=`/`latency=`/`maxbw=`/`fc=` URL
    /// overrides (each `None` when the query param was absent or
    /// unparseable) on top of the resolved defaults. Every allocation-sized
    /// field is clamped to a bounded range — `url.rs`'s parser only rejects
    /// non-positive/unparseable input, not unreasonably large input, so this
    /// is the actual backstop against a misconfigured or untrusted output
    /// URL demanding gigabytes for one destination (see the override-clamp
    /// constants above). `maxbw_bps` is left unclamped beyond `url.rs`'s
    /// `>= -1` check: it is a pacing rate, not a preallocated buffer, so it
    /// carries no equivalent memory risk.
    ///
    /// Resolution order matters and is deliberate: `latency` first (it
    /// feeds both ceilings below), then `fc` (libsrt requires FC to be set
    /// before SNDBUF/RCVBUF — see the shared latency/FC doc block above),
    /// then `sndbuf`/`rcvbuf`, each additionally bounded by the *final*
    /// `fc_pkts` so the `SNDBUF/RCVBUF <= FC` invariant holds for every
    /// combination of overrides, not just the ones this function happened
    /// to receive together.
    pub(super) fn with_overrides(
        mut self,
        sndbuf_bytes: Option<i32>,
        rcvbuf_bytes: Option<i32>,
        latency_ms: Option<i32>,
        maxbw_bps: Option<i64>,
        fc_pkts: Option<i32>,
    ) -> Self {
        if let Some(v) = latency_ms {
            self.latency_ms = v.clamp(SRT_LATENCY_MS_FLOOR, SRT_LATENCY_MS_CEILING);
            // Both formulas are latency-dependent; `defaults()` computed
            // them at the pre-override default latency, so they need
            // re-deriving now that the effective latency has changed.
            self.fc_pkts = latency_scaled_fc_ceiling_pkts(self.latency_ms);
            self.sndbuf_bytes = srt_egress_sndbuf_bytes(self.assumed_bitrate_bps, self.latency_ms);
        }
        if let Some(v) = fc_pkts {
            self.fc_pkts = v.clamp(
                EGRESS_FC_OVERRIDE_FLOOR,
                latency_scaled_fc_ceiling_pkts(self.latency_ms),
            );
        }
        let sndbuf_ceiling_under_fc = latency_scaled_buffer_ceiling_bytes(self.latency_ms)
            .min(self.fc_pkts.saturating_mul(SRT_PAYLOAD_SIZE_BYTES))
            .max(EGRESS_SNDBUF_FLOOR);
        self.sndbuf_bytes = match sndbuf_bytes {
            Some(v) => v.clamp(EGRESS_SNDBUF_FLOOR, sndbuf_ceiling_under_fc),
            None => self
                .sndbuf_bytes
                .clamp(EGRESS_SNDBUF_FLOOR, sndbuf_ceiling_under_fc),
        };
        if let Some(v) = rcvbuf_bytes {
            let rcvbuf_ceiling_under_fc = EGRESS_RCVBUF_OVERRIDE_CEILING
                .min(self.fc_pkts.saturating_mul(SRT_PAYLOAD_SIZE_BYTES))
                .max(EGRESS_RCVBUF_OVERRIDE_FLOOR);
            self.rcvbuf_bytes = v.clamp(EGRESS_RCVBUF_OVERRIDE_FLOOR, rcvbuf_ceiling_under_fc);
        }
        if let Some(v) = maxbw_bps {
            self.maxbw_bps = v;
        }
        self
    }
}

#[cfg(test)]
mod egress_buffer_sizing_tests {
    use super::*;

    #[test]
    fn unknown_bitrate_falls_back_to_worst_case_default() {
        // 50 Mbps * 250ms * 4x margin / 8 = 6.25 MB — matches
        // DESIRED_LOSSMAXTTL's documented 50 Mbps worst-case assumption.
        assert_eq!(srt_egress_sndbuf_bytes(None, DESIRED_LATENCY_MS), 6_250_000);
    }

    #[test]
    fn zero_or_negative_bitrate_is_treated_as_unknown() {
        assert_eq!(
            srt_egress_sndbuf_bytes(Some(0), DESIRED_LATENCY_MS),
            6_250_000
        );
        assert_eq!(
            srt_egress_sndbuf_bytes(Some(-1), DESIRED_LATENCY_MS),
            6_250_000
        );
    }

    #[test]
    fn low_bitrate_clamps_to_the_floor_not_zero() {
        // A trickle audio-only output should never get a near-zero buffer.
        assert_eq!(
            srt_egress_sndbuf_bytes(Some(64_000), DESIRED_LATENCY_MS),
            EGRESS_SNDBUF_FLOOR
        );
    }

    #[test]
    fn high_bitrate_clamps_to_the_ingest_derived_ceiling_at_default_latency() {
        // Well above the documented 40-50 Mbps envelope this repo tests;
        // at the default latency, must never exceed what ingest itself uses
        // (DESIRED_SRT_BUF) — the ceiling only grows past that for a caller
        // who has also raised latency (see `latency_scaled_buffer_ceiling_bytes`).
        assert_eq!(
            srt_egress_sndbuf_bytes(Some(1_000_000_000), DESIRED_LATENCY_MS),
            DESIRED_SRT_BUF
        );
    }

    #[test]
    fn sndbuf_ceiling_scales_up_for_a_higher_effective_latency() {
        // A caller who legitimately raises latency (e.g. a high-RTT
        // contribution link) must get a correspondingly larger ceiling —
        // the whole point of coupling the two: a flat ceiling would
        // silently under-buffer exactly this request. At 8s latency, 50
        // Mbps, 4x margin: 50_000_000 * 8 * 4 / 8 = 200,000,000 bytes.
        assert_eq!(
            srt_egress_sndbuf_bytes(Some(1_000_000_000), SRT_LATENCY_MS_CEILING),
            200_000_000
        );
    }

    #[test]
    fn documented_bitrate_envelope_stays_under_current_flat_preset() {
        // docs/mahashivratri-hero-scenario.md's highest tested envelope
        // (40 Mbps) should size well under the old flat 12MB preset, which
        // was this test's whole point: it was never tightly derived for
        // egress, just safely oversized.
        let bytes = srt_egress_sndbuf_bytes(Some(40_000_000), DESIRED_LATENCY_MS);
        assert!(bytes < DESIRED_SRT_BUF);
        assert!(bytes >= EGRESS_SNDBUF_FLOOR);
    }

    // Security: `url.rs`'s parser only rejects non-positive/unparseable
    // input, so an operator-supplied (or misconfigured/untrusted-upstream)
    // output URL asking for an absurd sndbuf/rcvbuf/latency/fc must still
    // come out bounded — `with_overrides` is the actual backstop.
    #[test]
    fn with_overrides_clamps_an_oversized_sndbuf_to_the_ceiling() {
        let opts =
            EgressBufferOpts::defaults(None).with_overrides(Some(i32::MAX), None, None, None, None);
        assert_eq!(opts.sndbuf_bytes, DESIRED_SRT_BUF);
    }

    #[test]
    fn with_overrides_clamps_an_oversized_rcvbuf_to_the_ceiling() {
        let opts =
            EgressBufferOpts::defaults(None).with_overrides(None, Some(i32::MAX), None, None, None);
        assert_eq!(opts.rcvbuf_bytes, EGRESS_RCVBUF_OVERRIDE_CEILING);
    }

    #[test]
    fn with_overrides_clamps_an_oversized_latency_to_the_ceiling() {
        let opts =
            EgressBufferOpts::defaults(None).with_overrides(None, None, Some(i32::MAX), None, None);
        assert_eq!(opts.latency_ms, SRT_LATENCY_MS_CEILING);
    }

    #[test]
    fn with_overrides_clamps_an_oversized_fc_to_the_ceiling() {
        let opts =
            EgressBufferOpts::defaults(None).with_overrides(None, None, None, None, Some(i32::MAX));
        assert_eq!(opts.fc_pkts, DESIRED_FC);
    }

    #[test]
    fn with_overrides_clamps_a_near_zero_positive_value_to_the_floor() {
        // url.rs already rejects <= 0, but a small positive value like `1`
        // must still land at a usable floor, not create a degenerate socket.
        let opts = EgressBufferOpts::defaults(None).with_overrides(
            Some(1),
            Some(1),
            Some(1),
            None,
            Some(1),
        );
        assert_eq!(opts.sndbuf_bytes, EGRESS_SNDBUF_FLOOR);
        assert_eq!(opts.rcvbuf_bytes, EGRESS_RCVBUF_OVERRIDE_FLOOR);
        assert_eq!(opts.latency_ms, SRT_LATENCY_MS_FLOOR);
        assert_eq!(opts.fc_pkts, EGRESS_FC_OVERRIDE_FLOOR);
    }

    // Correctness: raising `latency` alone (no explicit `sndbuf`/`fc`) must
    // re-derive both from the new effective latency, not silently keep the
    // values `defaults()` computed at the old default latency — that was
    // the bug this coupling exists to fix (SRTO_RCVLATENCY's own doc entry:
    // the buffer must hold `latency * bitrate` bytes).
    #[test]
    fn latency_only_override_scales_up_sndbuf_and_fc_together() {
        let opts = EgressBufferOpts::defaults(None).with_overrides(
            None,
            None,
            Some(SRT_LATENCY_MS_CEILING),
            None,
            None,
        );
        let unclamped_formula = srt_egress_sndbuf_bytes(None, SRT_LATENCY_MS_CEILING);
        assert!(
            unclamped_formula > DESIRED_SRT_BUF,
            "test setup: 8s latency must actually need more than the flat 12MB preset"
        );
        // Not an exact match against the unclamped formula: the FC ceiling
        // it feeds into is a packet count (`.../SRT_PAYLOAD_SIZE_BYTES`,
        // integer division), so the byte ceiling it implies rounds down
        // slightly. What must hold is that this override scaled the buffer
        // up meaningfully from the flat default, and stayed within a
        // payload-size rounding margin of the ideal formula value.
        assert!(opts.sndbuf_bytes > DESIRED_SRT_BUF);
        assert!(opts.sndbuf_bytes > unclamped_formula - SRT_PAYLOAD_SIZE_BYTES);
        // The FC <= SNDBUF/payload invariant must hold for whatever this
        // resolved to, not just at the untouched default.
        assert!(opts.fc_pkts.saturating_mul(SRT_PAYLOAD_SIZE_BYTES) >= opts.sndbuf_bytes);
    }

    // Correctness: an explicit low `fc` override must cap `sndbuf` down to
    // fit under it, even though `sndbuf` itself was not explicitly
    // overridden — libsrt enforces SNDBUF/RCVBUF <= FC unconditionally, so
    // our own resolved values must never claim more than FC actually allows.
    #[test]
    fn low_fc_override_caps_the_default_sndbuf_to_fit_under_it() {
        let opts = EgressBufferOpts::defaults(None).with_overrides(
            None,
            None,
            None,
            None,
            Some(EGRESS_FC_OVERRIDE_FLOOR),
        );
        assert_eq!(opts.fc_pkts, EGRESS_FC_OVERRIDE_FLOOR);
        assert!(
            opts.sndbuf_bytes <= EGRESS_FC_OVERRIDE_FLOOR.saturating_mul(SRT_PAYLOAD_SIZE_BYTES)
        );
    }

    #[test]
    fn ingest_buffer_packets_match_libsrt_byte_conversion() {
        let (flow_window_packets, receive_buffer_packets) =
            resolve_ingest_buffer_packets(DESIRED_LATENCY_MS);
        assert_eq!(flow_window_packets, DESIRED_FC as u32);
        assert_eq!(receive_buffer_packets, 8_548);
    }
}

/// Property coverage for the invariants this module exists to guarantee,
/// across arbitrary (including wildly out-of-range, adversarial-shaped) i32
/// inputs rather than the hand-picked cases above: every resolved value
/// stays within its documented clamp range, and `SNDBUF/RCVBUF <= FC *
/// SRT_PAYLOAD_SIZE_BYTES` — the libsrt-documented ordering constraint this
/// whole module was restructured around — holds for every combination, not
/// just the ones the example-based tests happened to construct.
#[cfg(test)]
mod buffer_sizing_proptests {
    use proptest::prelude::*;

    use super::*;

    fn option_i32_strategy() -> impl Strategy<Value = Option<i32>> {
        proptest::option::of(any::<i32>())
    }

    fn option_i64_strategy() -> impl Strategy<Value = Option<i64>> {
        proptest::option::of(any::<i64>())
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(512))]

        #[test]
        fn egress_overrides_always_satisfy_range_and_fc_coupling_invariants(
            bitrate_bps in option_i64_strategy(),
            sndbuf_bytes in option_i32_strategy(),
            rcvbuf_bytes in option_i32_strategy(),
            latency_ms in option_i32_strategy(),
            maxbw_bps in option_i64_strategy(),
            fc_pkts in option_i32_strategy(),
        ) {
            let opts = EgressBufferOpts::defaults(bitrate_bps)
                .with_overrides(sndbuf_bytes, rcvbuf_bytes, latency_ms, maxbw_bps, fc_pkts);

            prop_assert!((SRT_LATENCY_MS_FLOOR..=SRT_LATENCY_MS_CEILING).contains(&opts.latency_ms));
            prop_assert!(opts.sndbuf_bytes >= EGRESS_SNDBUF_FLOOR);
            prop_assert!(opts.rcvbuf_bytes >= EGRESS_RCVBUF_OVERRIDE_FLOOR);
            prop_assert!(opts.rcvbuf_bytes <= EGRESS_RCVBUF_OVERRIDE_CEILING);
            prop_assert!(opts.fc_pkts >= EGRESS_FC_OVERRIDE_FLOOR.min(DESIRED_FC));

            // The libsrt-documented ordering constraint: SNDBUF/RCVBUF must
            // never exceed FC in packet-count terms, for any combination.
            let fc_bytes = opts.fc_pkts.saturating_mul(SRT_PAYLOAD_SIZE_BYTES);
            prop_assert!(opts.sndbuf_bytes <= fc_bytes);
            prop_assert!(opts.rcvbuf_bytes <= fc_bytes);
        }

        #[test]
        fn ingest_latency_opts_always_satisfy_range_and_fc_coupling_invariants(
            latency_ms in any::<i32>(),
        ) {
            let (resolved_latency, fc_pkts, rcvbuf_bytes) = resolve_ingest_latency_opts(latency_ms);

            prop_assert!((SRT_LATENCY_MS_FLOOR..=SRT_LATENCY_MS_CEILING).contains(&resolved_latency));
            prop_assert!(fc_pkts >= DESIRED_FC);
            prop_assert!(rcvbuf_bytes >= DESIRED_SRT_BUF.min(fc_pkts.saturating_mul(SRT_PAYLOAD_SIZE_BYTES)));
            prop_assert!(rcvbuf_bytes <= fc_pkts.saturating_mul(SRT_PAYLOAD_SIZE_BYTES));
        }
    }
}
