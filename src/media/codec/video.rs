#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
use std::borrow::Cow;
use std::ops::ControlFlow;

use bytes::Bytes;

use crate::media::packet::PayloadFormat;

/// Prepare a video payload for MPEG-TS muxing (Annex B output).
///
/// - **FLV**: strips 5-byte header; sequence headers (packet_type 0) update
///   `*nalu_len_size` and `*sps_pps_cache` (does NOT emit a standalone packet,
///   returns None); data keyframes prepend cached SPS/PPS then AVCC→Annex B;
///   non-keyframes convert AVCC→Annex B.
/// - **Raw**: pass-through (already Annex B with inline SPS/PPS).
pub fn video_for_ts<'a>(
    payload: &'a [u8],
    format: PayloadFormat,
    nalu_len_size: &mut usize,
    sps_pps_cache: &mut Vec<u8>,
) -> Option<Cow<'a, [u8]>> {
    match format {
        PayloadFormat::Raw => {
            if payload.is_empty() {
                None
            } else {
                let refreshed = refresh_annexb_parameter_set_cache(payload, sps_pps_cache);
                if !refreshed
                    && !sps_pps_cache.is_empty()
                    && raw_annexb_is_keyframe(payload)
                    && !payload.starts_with(sps_pps_cache.as_slice())
                {
                    let mut out = sps_pps_cache.clone();
                    out.extend_from_slice(payload);
                    Some(Cow::Owned(out))
                } else {
                    Some(Cow::Borrowed(payload))
                }
            }
        }
        PayloadFormat::Flv => {
            let (&[tag, packet_type, ..], body) = payload.split_first_chunk::<5>()?;
            if body.is_empty() {
                return None;
            }
            if packet_type == 0 {
                // Sequence header — cache SPS/PPS Annex B for inline injection.
                // A malformed/truncated header parses to an empty Vec; only
                // overwrite the cache on success so a bad header can't wipe out
                // a previously cached, still-valid parameter set.
                let (nls, annexb) = parse_avcc_config(body);
                *nalu_len_size = nls;
                if !annexb.is_empty() {
                    *sps_pps_cache = annexb;
                }
                // Don't emit a standalone packet; SPS/PPS will be prepended to IDR frames
                None
            } else {
                let is_keyframe = (tag & 0xF0) == 0x10;
                if is_keyframe && !sps_pps_cache.is_empty() {
                    // Prepend SPS/PPS then append AVCC→Annex B in a single allocation
                    let mut out = sps_pps_cache.clone();
                    avcc_to_annexb_into(body, *nalu_len_size, &mut out);
                    if out.len() == sps_pps_cache.len() {
                        return None; // AVCC body was empty
                    }
                    Some(Cow::Owned(out))
                } else {
                    let annexb = avcc_to_annexb(body, *nalu_len_size);
                    if annexb.is_empty() {
                        return None;
                    }
                    Some(Cow::Owned(annexb))
                }
            }
        }
    }
}

/// Zero-allocation variant of [`video_for_ts`].
///
/// For `Raw` format: returns `Some(payload)` directly — zero-copy.
/// For `Flv` format: strips the FLV header, converts AVCC → Annex B and writes
/// the result into `buf` (which is cleared first). Returns `Some(&buf[..])`.
///
/// Returns `None` if the packet should be skipped (sequence header, empty, etc.).
///
/// # Usage
/// ```
/// use restream::media::codec::video_for_ts_into;
/// use restream::media::packet::PayloadFormat;
///
/// let payload = [0, 0, 1, 9, 0x10];
/// let mut nalu_len_size = 4;
/// let mut sps_pps = Vec::new();
/// let mut conv_buf = Vec::new();
///
/// let slice = video_for_ts_into(
///     &payload,
///     PayloadFormat::Raw,
///     &mut nalu_len_size,
///     &mut sps_pps,
///     &mut conv_buf,
/// )
/// .expect("raw Annex B payload should pass through");
/// assert_eq!(slice, payload);
/// ```
#[inline]
pub fn video_for_ts_into<'a>(
    payload: &'a [u8],
    format: PayloadFormat,
    nalu_len_size: &mut usize,
    sps_pps_cache: &mut Vec<u8>,
    buf: &'a mut Vec<u8>,
) -> Option<&'a [u8]> {
    match format {
        PayloadFormat::Raw => {
            if payload.is_empty() {
                None
            } else {
                let refreshed = refresh_annexb_parameter_set_cache(payload, sps_pps_cache);
                if !refreshed
                    && !sps_pps_cache.is_empty()
                    && raw_annexb_is_keyframe(payload)
                    && !payload.starts_with(sps_pps_cache.as_slice())
                {
                    buf.clear();
                    buf.extend_from_slice(sps_pps_cache);
                    buf.extend_from_slice(payload);
                    Some(buf.as_slice())
                } else {
                    Some(payload)
                }
            }
        }
        PayloadFormat::Flv => {
            buf.clear();
            let (&[tag, packet_type, ..], body) = payload.split_first_chunk::<5>()?;
            if body.is_empty() {
                return None;
            }
            if packet_type == 0 {
                // Sequence header — update SPS/PPS cache, no frame to emit.
                // A malformed/truncated header parses to an empty Vec; only
                // overwrite the cache on success (see the Raw-format sibling
                // above for the matching fail-closed pattern).
                let (nls, annexb) = parse_avcc_config(body);
                *nalu_len_size = nls;
                if !annexb.is_empty() {
                    *sps_pps_cache = annexb;
                }
                None
            } else {
                let is_keyframe = (tag & 0xF0) == 0x10;
                if is_keyframe && !sps_pps_cache.is_empty() {
                    buf.extend_from_slice(sps_pps_cache);
                }
                let before = buf.len();
                avcc_to_annexb_into(body, *nalu_len_size, buf);
                if buf.len() == before {
                    return None; // AVCC body was empty
                }
                Some(buf.as_slice())
            }
        }
    }
}

fn refresh_annexb_parameter_set_cache(payload: &[u8], sps_pps_cache: &mut Vec<u8>) -> bool {
    let Some(parameter_sets) = annexb_parameter_sets(payload) else {
        return false;
    };

    *sps_pps_cache = parameter_sets;
    true
}

pub(crate) fn annexb_parameter_sets(payload: &[u8]) -> Option<Vec<u8>> {
    let mut accumulator = AnnexbParameterSetAccumulator::default();
    accumulator.push_payload(payload)
}

#[derive(Default)]
pub(crate) struct AnnexbParameterSetAccumulator {
    kind: AnnexbCodecKind,
    h264_sps: Option<Vec<u8>>,
    h264_pps: Option<Vec<u8>>,
    h265_vps: Option<Vec<u8>>,
    h265_sps: Option<Vec<u8>>,
    h265_pps: Option<Vec<u8>>,
}

impl AnnexbParameterSetAccumulator {
    pub(crate) fn push_payload(&mut self, payload: &[u8]) -> Option<Vec<u8>> {
        // Runs for every video packet; allocates only when the payload
        // actually carries parameter sets.
        let _ = for_each_annexb_nalu(payload, |nalu| {
            self.push_nalu(nalu);
            ControlFlow::Continue(())
        });
        self.complete()
    }

    fn push_nalu(&mut self, nalu: &[u8]) {
        let Some(&first) = nalu.first() else {
            return;
        };
        let h264_nal_type = first & 0x1F;
        // A base-layer H.265 parameter set has a two-byte header with
        // forbidden_zero_bit = 0, nuh_layer_id = 0 and nuh_temporal_id_plus1
        // >= 1 (7.3.1.2): first byte 0x40/0x42/0x44, second byte 0x01..=0x07.
        // Without the layer check, common H.264 reference slices (0x41, 0x43,
        // 0x45) read as VPS/SPS/PPS.
        let h265_nal_type = match nalu {
            [first, second, ..] if first & 0x81 == 0 && (1..=7).contains(second) => {
                (first >> 1) & 0x3F
            }
            _ => 0,
        };

        if (32..=34).contains(&h265_nal_type) && self.switch_kind(AnnexbCodecKind::H265) {
            match h265_nal_type {
                32 => self.h265_vps = Some(annexb_nalu(nalu)),
                33 => self.h265_sps = Some(annexb_nalu(nalu)),
                34 => self.h265_pps = Some(annexb_nalu(nalu)),
                _ => {}
            }
            return;
        }

        if self.kind != AnnexbCodecKind::H265
            && matches!(h264_nal_type, 7 | 8)
            && self.switch_kind(AnnexbCodecKind::H264)
        {
            if h264_nal_type == 7 {
                self.h264_sps = Some(annexb_nalu(nalu));
            } else {
                self.h264_pps = Some(annexb_nalu(nalu));
            }
        }
    }

    fn switch_kind(&mut self, kind: AnnexbCodecKind) -> bool {
        match self.kind {
            AnnexbCodecKind::Unknown => {
                self.kind = kind;
                true
            }
            existing if existing == kind => true,
            _ => {
                *self = Self {
                    kind,
                    ..Self::default()
                };
                true
            }
        }
    }

    fn complete(&self) -> Option<Vec<u8>> {
        match self.kind {
            AnnexbCodecKind::Unknown => None,
            AnnexbCodecKind::H264 => {
                let (Some(sps), Some(pps)) = (&self.h264_sps, &self.h264_pps) else {
                    return None;
                };
                Some([sps.as_slice(), pps.as_slice()].concat())
            }
            AnnexbCodecKind::H265 => {
                let (Some(vps), Some(sps), Some(pps)) =
                    (&self.h265_vps, &self.h265_sps, &self.h265_pps)
                else {
                    return None;
                };
                Some([vps.as_slice(), sps.as_slice(), pps.as_slice()].concat())
            }
        }
    }
}

fn annexb_nalu(nalu: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nalu.len().saturating_add(4));
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nalu);
    out
}

pub(crate) fn raw_annexb_is_keyframe(payload: &[u8]) -> bool {
    for_each_annexb_nalu(payload, |nalu| {
        let keyframe = match nalu {
            [first, ..] if first & 0x1F == 5 => true,
            [first, _, ..] => matches!((first >> 1) & 0x3F, 16..=23),
            _ => false,
        };
        if keyframe {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AnnexbCodecKind {
    #[default]
    Unknown,
    H264,
    H265,
}

/// Prepare a Raw (Annex B) video payload for RTMP publishing.
///
/// Converts Annex B → AVCC, wraps in 5-byte FLV video tag header.
/// Returns `None` if the converted payload is empty.
pub fn video_for_rtmp(payload: &[u8], is_keyframe: bool) -> Option<Vec<u8>> {
    // Single allocation: write FLV header then AVCC inline — no intermediate Vec.
    let tag = if is_keyframe { 0x17u8 } else { 0x27u8 };
    let mut out = Vec::with_capacity(payload.len().saturating_add(5));
    out.extend_from_slice(&[tag, 1, 0, 0, 0]);
    if !annexb_to_avcc_into(payload, &mut out) {
        return None; // no VCL NALUs found
    }
    Some(out)
}

/// Zero-allocation variant of [`video_for_rtmp`].
///
/// Clears `out` and writes the FLV-framed AVCC payload into it in-place.
/// Returns `true` if data was written, `false` if no VCL NALUs were found.
/// The caller must consume `out` before the next call that clears it.
#[inline]
pub fn video_for_rtmp_into(payload: &[u8], is_keyframe: bool, out: &mut Vec<u8>) -> bool {
    video_for_rtmp_with_composition_into(payload, is_keyframe, 0, out)
}

/// Like [`video_for_rtmp_into`] but preserves the FLV composition offset
/// (`PTS-DTS`) for streams with B-frames.
#[inline]
pub fn video_for_rtmp_with_composition_into(
    payload: &[u8],
    is_keyframe: bool,
    composition_time_ms: i32,
    out: &mut Vec<u8>,
) -> bool {
    let tag = if is_keyframe { 0x17u8 } else { 0x27u8 };
    out.clear();
    out.extend_from_slice(&[tag, 1]);
    out.extend_from_slice(&signed_be24(composition_time_ms));
    annexb_to_avcc_into(payload, out)
}

/// A signed 24-bit big-endian value (FLV composition time), clamped to its
/// range.
pub(super) fn signed_be24(value: i32) -> [u8; 3] {
    let [_, high, mid, low] = value.clamp(-8_388_608, 8_388_607).to_be_bytes();
    [high, mid, low]
}

/// An AVCDecoderConfigurationRecord (ISO/IEC 14496-15 5.3.3.1), borrowed:
/// the one walker for RTMP ingest probes, FLV → TS conversion and the fMP4
/// sample entry. Truncation anywhere is `None`.
pub(crate) struct AvccRecord<'a> {
    /// `lengthSizeMinusOne` (0..=3): NALU length fields are this + 1 bytes.
    pub(crate) length_size_minus_one: u8,
    pub(crate) sps: Vec<&'a [u8]>,
    pub(crate) pps: Vec<&'a [u8]>,
}

pub(crate) fn avcc_record(record: &[u8]) -> Option<AvccRecord<'_>> {
    let (
        &[
            _version,
            _profile,
            _compatibility,
            _level,
            length_byte,
            sps_byte,
        ],
        mut rest,
    ) = record.split_first_chunk::<6>()?;
    let mut sps = Vec::with_capacity(usize::from(sps_byte & 0x1F));
    for _ in 0..(sps_byte & 0x1F) {
        sps.push(take_u16_prefixed(&mut rest)?);
    }
    let (&pps_count, tail) = rest.split_first()?;
    rest = tail;
    let mut pps = Vec::with_capacity(usize::from(pps_count));
    for _ in 0..pps_count {
        pps.push(take_u16_prefixed(&mut rest)?);
    }
    Some(AvccRecord {
        length_size_minus_one: length_byte & 0x03,
        sps,
        pps,
    })
}

/// The next `u16` length-prefixed item, advancing `rest` past it.
pub(crate) fn take_u16_prefixed<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (len, tail) = rest.split_first_chunk::<2>()?;
    let (item, tail) = tail.split_at_checked(usize::from(u16::from_be_bytes(*len)))?;
    *rest = tail;
    Some(item)
}

/// Parse AVCC decoder configuration record.
/// Returns `(nalu_length_size, sps_pps_as_annexb)`.
///
/// Fails closed: if the SPS or PPS list is truncated at any point, the
/// annexb output is empty rather than containing whatever prefix parsed
/// before the truncation. A cached parameter set missing its PPS (or with a
/// PPS but no SPS) is worse than caching nothing, since it would be
/// prepended to keyframes as if it were complete.
pub fn parse_avcc_config(data: &[u8]) -> (usize, Vec<u8>) {
    let Some(&length_byte) = data.get(4).filter(|_| data.len() >= 8) else {
        return (4, Vec::new());
    };
    let nalu_len_size = usize::from(length_byte & 0x03).saturating_add(1);
    let annexb = avcc_record(data)
        .map(|record| {
            let mut out = Vec::new();
            for nalu in record.sps.iter().chain(&record.pps) {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(nalu);
            }
            out
        })
        .unwrap_or_default();
    (nalu_len_size, annexb)
}

/// Convert AVCC-format NALUs to Annex B (start codes).
pub fn avcc_to_annexb(data: &[u8], nalu_len_size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    avcc_to_annexb_into(data, nalu_len_size, &mut out);
    out
}

/// Like `avcc_to_annexb` but appends output into a caller-provided buffer.
/// Callers can reuse the allocation across packets to avoid per-packet heap churn.
#[inline]
///
/// `nalu_len_size` must be 1..=4 (an AVC record's `lengthSizeMinusOne` + 1);
/// any other width produces no output.
pub fn avcc_to_annexb_into(data: &[u8], nalu_len_size: usize, out: &mut Vec<u8>) {
    if !(1..=4).contains(&nalu_len_size) {
        return;
    }
    let mut rest = data;
    while let Some((length, tail)) = rest.split_at_checked(nalu_len_size) {
        let nalu_len = length
            .iter()
            .fold(0usize, |len, &byte| (len << 8) | usize::from(byte));
        let Some((nalu, tail)) = tail.split_at_checked(nalu_len).filter(|_| nalu_len > 0) else {
            break;
        };
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nalu);
        rest = tail;
    }
}

/// Convert Annex B NALUs to AVCC format (4-byte length prefix).
/// Filters out SPS (7), PPS (8), and AUD (9) NALUs.
pub fn annexb_to_avcc(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let _ = annexb_to_avcc_into(data, &mut out);
    out
}

/// Like `annexb_to_avcc` but appends output into a caller-provided buffer.
/// Callers can reuse the allocation across packets to avoid per-packet heap churn.
///
/// # Implementation choice: callback walker (no allocation)
///
/// History: a `memmem::find_iter().peekable()` single-pass variant was
/// measured 25–31% slower than a two-pass split into two small `Vec`s on
/// 2026-06-23 (1-NALU P-frame at 8 KiB, ~890 ns vs ~690 ns), in a
/// single-threaded micro-benchmark where allocation is uncontended. Under
/// production load those per-packet `Vec`s contend on the allocator (arena
/// lock waits on the SRT ingress Owner, runtime-crossings C3), so this walks
/// NALUs with [`for_each_annexb_nalu`]: one pass, no `Peekable`, no
/// allocation. `benches/codec_conversions.rs` compares it with the scratch
/// variant below.
#[inline]
pub fn annexb_to_avcc_into(data: &[u8], out: &mut Vec<u8>) -> bool {
    let mut has_vcl = false;
    let _ = for_each_annexb_nalu(data, |nalu| {
        let Some(&first) = nalu.first() else {
            return ControlFlow::Continue(());
        };
        let nal_type = first & 0x1F;
        if matches!(nal_type, 7..=9) {
            return ControlFlow::Continue(());
        }
        if matches!(nal_type, 1..=5) {
            has_vcl = true;
        }
        let len = nalu.len() as u32;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nalu);
        ControlFlow::Continue(())
    });
    has_vcl
}

/// Like `annexb_to_avcc` but uses caller-provided scratch buffers to avoid the
/// two intermediate Vec allocations (`Vec<(usize,usize)>` for start-code spans
/// and the NALU-split `Vec<&[u8]>`) produced by `split_annexb_nalus`.
///
/// Provide a `sc_scratch: &mut Vec<(usize,usize)>` pre-allocated per consumer
/// (typically stored alongside the `video_conv_buf` scratch). It is cleared and
/// repopulated on every call.
///
/// # Benchmark results (2026-06-23, bench-dev, x86-64 Zen, `annexb_to_avcc` group)
///
/// | Input | `two_pass` | `with_scratch` | Winner |
/// |---|---|---|---|
/// | P-frame 8 KiB, 1 NALU | 2.73 µs | 1.80 µs | **with_scratch +34%** |
/// | P-frame 30 KiB, 3 NALU | 9.83 µs | 8.95 µs | **with_scratch +9%** |
/// | IDR 80 KiB, 1 NALU | 16.98 µs | 24.07 µs | two_pass (scratch slower -42%) |
///
/// Mixed result: `with_scratch` wins for small multi-NALU frames but loses for
/// large single-NALU IDR frames (clearing+repopulating `sc_scratch` dominates).
/// The production `annexb_to_avcc_into` uses `two_pass`. Switch to `with_scratch`
/// only if the workload profile shifts to many small NALUs per frame.
fn get_start_code_finder() -> &'static memchr::memmem::Finder<'static> {
    static FINDER: std::sync::OnceLock<memchr::memmem::Finder<'static>> =
        std::sync::OnceLock::new();
    FINDER.get_or_init(|| memchr::memmem::Finder::new(&[0u8, 0, 1]))
}

pub fn annexb_to_avcc_with_scratch(
    data: &[u8],
    out: &mut Vec<u8>,
    sc_scratch: &mut Vec<(usize, usize)>,
) {
    // Populate start-code spans into the scratch buffer, clearing first.
    sc_scratch.clear();
    let finder = get_start_code_finder();
    for idx in finder.find_iter(data) {
        sc_scratch.push(start_code_span(data, idx));
    }

    // Write AVCC directly from indexed spans — no Vec<&[u8]> allocation.
    let mut spans = sc_scratch.iter().peekable();
    while let Some(&(_, nalu_start)) = spans.next() {
        let nalu_end = spans
            .peek()
            .map_or(data.len(), |&&(next_code, _)| next_code);
        let Some(nalu @ [first, ..]) = data.get(nalu_start..nalu_end) else {
            continue;
        };
        if matches!(first & 0x1F, 7..=9) {
            continue;
        }
        out.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
        out.extend_from_slice(nalu);
    }
}

/// Locate all Annex B start codes (`0x00 0x00 0x01` and `0x00 0x00 0x00 0x01`).
/// Returns a list of `(start_index, end_index)` spans of the start codes themselves.
pub fn find_annexb_start_codes(data: &[u8]) -> Vec<(usize, usize)> {
    let finder = get_start_code_finder();
    finder
        .find_iter(data)
        .map(|idx| start_code_span(data, idx))
        .collect()
}

/// The span of the start code whose `00 00 01` begins at `idx`: it extends
/// back over every preceding zero byte (so `00 00 00 01` and trailing zero
/// padding belong to the start code, not the previous NALU).
fn start_code_span(data: &[u8], idx: usize) -> (usize, usize) {
    let zeros = data.get(..idx).map_or(0, |before| {
        before.iter().rev().take_while(|&&byte| byte == 0).count()
    });
    (idx.saturating_sub(zeros), idx.saturating_add(3))
}

/// Split Annex B byte stream into individual NALUs (without start codes).
pub fn split_annexb_nalus(data: &[u8]) -> Vec<&[u8]> {
    let mut nalus = Vec::new();
    let _ = for_each_annexb_nalu(data, |nalu| {
        nalus.push(nalu);
        ControlFlow::Continue(())
    });
    nalus
}

/// Visit each NALU of an Annex B stream (without its start code), in order,
/// stopping as soon as `visit` breaks. Same boundaries as
/// [`find_annexb_start_codes`] and [`split_annexb_nalus`], in one pass with no
/// allocation: the per-frame `Vec`s the split form builds showed up as
/// allocator-lock waits on the SRT ingress Owner (runtime-crossings C3), so
/// per-packet paths walk NALUs with this instead.
pub fn for_each_annexb_nalu<'a>(
    data: &'a [u8],
    mut visit: impl FnMut(&'a [u8]) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let finder = get_start_code_finder();
    // Payload start of the NALU whose end is the next start code.
    let mut current: Option<usize> = None;
    for idx in finder.find_iter(data) {
        let (start, payload_start) = start_code_span(data, idx);
        if let Some(nalu_start) = current
            && let Some(nalu) = data.get(nalu_start..start)
            && !nalu.is_empty()
        {
            visit(nalu)?;
        }
        current = Some(payload_start);
    }
    if let Some(nalu_start) = current
        && let Some(nalu) = data.get(nalu_start..)
        && !nalu.is_empty()
    {
        visit(nalu)?;
    }
    ControlFlow::Continue(())
}

/// Build an FLV video sequence header (AVCC decoder config) from Annex B keyframe data.
///
/// Returns `None` when the parameter sets do not fit the record: at most 31
/// SPS (5-bit count), 255 PPS, and 65,535 bytes per NALU (16-bit length).
pub fn build_avcc_sequence_header(annexb_data: &[u8]) -> Option<Bytes> {
    let nalus = split_annexb_nalus(annexb_data);
    let nal_type = |nalu: &&[u8]| nalu.first().map(|first| first & 0x1F);
    let sps_list: Vec<&[u8]> = nalus
        .iter()
        .filter(|nalu| nal_type(nalu) == Some(7))
        .copied()
        .collect();
    let pps_list: Vec<&[u8]> = nalus
        .iter()
        .filter(|nalu| nal_type(nalu) == Some(8))
        .copied()
        .collect();

    let &[_, profile, compatibility, level] = sps_list.first()?.first_chunk::<4>()?;
    let sps_count = u8::try_from(sps_list.len())
        .ok()
        .filter(|count| *count <= 0x1F)?;
    let pps_count = u8::try_from(pps_list.len()).ok()?;
    let too_long = |nalu: &&[u8]| u16::try_from(nalu.len()).is_err();
    if sps_list.iter().chain(&pps_list).any(too_long) {
        return None;
    }

    let mut buf = Vec::with_capacity(64);
    // FLV video tag: keyframe(0x17) + sequence header(0x00) + composition time(0,0,0)
    buf.extend_from_slice(&[0x17, 0x00, 0x00, 0x00, 0x00]);
    // AVCDecoderConfigurationRecord
    buf.push(1); // configurationVersion
    buf.push(profile); // AVCProfileIndication
    buf.push(compatibility); // profile_compatibility
    buf.push(level); // AVCLevelIndication
    buf.push(0xFF); // lengthSizeMinusOne = 3 (4 bytes)

    buf.push(0xE0 | sps_count);
    for s in &sps_list {
        buf.extend_from_slice(&(s.len() as u16).to_be_bytes());
        buf.extend_from_slice(s);
    }
    buf.push(pps_count);
    for p in &pps_list {
        buf.extend_from_slice(&(p.len() as u16).to_be_bytes());
        buf.extend_from_slice(p);
    }

    Some(Bytes::from(buf))
}
