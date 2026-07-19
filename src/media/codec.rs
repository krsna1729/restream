//! Payload format conversions for the 2×3×2 ingest/egress matrix.
//!
//! Four entry points cover every path:
//!   - `video_for_ts` / `audio_for_ts`  — prepare payloads for MPEG-TS muxing (SRT/HLS egress, transcoder feeder)
//!   - `video_for_rtmp` / `audio_for_rtmp` — prepare payloads for RTMP publishing
//!
//! Lower-level helpers (`avcc_to_annexb`, `annexb_to_avcc`, etc.) are also public
//! for use in sequence header synthesis and tests.

use bytes::Bytes;
use std::borrow::Cow;

use crate::media::ring_buffer::PayloadFormat;

mod enhanced_rtmp_hevc;

pub use enhanced_rtmp_hevc::{
    build_hevc_enhanced_rtmp_sequence_header, hevc_video_for_enhanced_rtmp_with_composition_into,
};

// ---------------------------------------------------------------------------
// High-level: payload → MPEG-TS ready
// ---------------------------------------------------------------------------

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
                refresh_annexb_parameter_set_cache(payload, sps_pps_cache);
                if !sps_pps_cache.is_empty()
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
            if payload.len() <= 5 {
                return None;
            }
            if payload[1] == 0 {
                // Sequence header — cache SPS/PPS Annex B for inline injection
                let (nls, annexb) = parse_avcc_config(&payload[5..]);
                *nalu_len_size = nls;
                *sps_pps_cache = annexb;
                // Don't emit a standalone packet; SPS/PPS will be prepended to IDR frames
                None
            } else {
                let is_keyframe = (payload[0] & 0xF0) == 0x10;
                if is_keyframe && !sps_pps_cache.is_empty() {
                    // Prepend SPS/PPS then append AVCC→Annex B in a single allocation
                    let mut out = sps_pps_cache.clone();
                    avcc_to_annexb_into(&payload[5..], *nalu_len_size, &mut out);
                    if out.len() == sps_pps_cache.len() {
                        return None; // AVCC body was empty
                    }
                    Some(Cow::Owned(out))
                } else {
                    let annexb = avcc_to_annexb(&payload[5..], *nalu_len_size);
                    if annexb.is_empty() {
                        return None;
                    }
                    Some(Cow::Owned(annexb))
                }
            }
        }
    }
}

/// Prepare an audio payload for MPEG-TS muxing (ADTS-wrapped output).
///
/// - **FLV**: strips 2-byte header, skips config packets (packet_type 0),
///   prepends a 7-byte ADTS header to the raw AAC frame.
/// - **Raw with ADTS** (from SRT ingest): pass-through.
/// - **Raw without ADTS** (from transcoder/FFmpeg): prepends ADTS header.
pub fn audio_for_ts<'a>(
    payload: &'a [u8],
    format: PayloadFormat,
    sample_rate: u32,
    channels: u32,
) -> Option<Cow<'a, [u8]>> {
    match format {
        PayloadFormat::Raw => {
            if payload.is_empty() {
                return None;
            }
            if has_adts_sync(payload) {
                Some(Cow::Borrowed(payload))
            } else {
                Some(Cow::Owned(prepend_adts(payload, sample_rate, channels)))
            }
        }
        PayloadFormat::Flv => {
            if payload.len() <= 2 || payload[1] == 0 {
                return None;
            }
            let raw_aac = &payload[2..];
            Some(Cow::Owned(prepend_adts(raw_aac, sample_rate, channels)))
        }
    }
}

// ---------------------------------------------------------------------------
// Zero-allocation _into variants — use per-task reusable Vec<u8> buffers
// ---------------------------------------------------------------------------

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
/// use restream::media::ring_buffer::PayloadFormat;
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
                refresh_annexb_parameter_set_cache(payload, sps_pps_cache);
                if !sps_pps_cache.is_empty()
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
            if payload.len() <= 5 {
                return None;
            }
            if payload[1] == 0 {
                // Sequence header — update SPS/PPS cache, no frame to emit
                let (nls, annexb) = parse_avcc_config(&payload[5..]);
                *nalu_len_size = nls;
                *sps_pps_cache = annexb;
                None
            } else {
                let is_keyframe = (payload[0] & 0xF0) == 0x10;
                if is_keyframe && !sps_pps_cache.is_empty() {
                    buf.extend_from_slice(sps_pps_cache);
                }
                let before = buf.len();
                avcc_to_annexb_into(&payload[5..], *nalu_len_size, buf);
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
        let nalus = split_annexb_nalus(payload);
        if nalus.is_empty() {
            return self.complete();
        }

        for nalu in nalus {
            self.push_nalu(nalu);
        }

        self.complete()
    }

    fn push_nalu(&mut self, nalu: &[u8]) {
        if nalu.is_empty() {
            return;
        }
        let h264_nal_type = nalu[0] & 0x1F;
        let h265_nal_type = if nalu.len() >= 2 {
            (nalu[0] >> 1) & 0x3F
        } else {
            0
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
    let mut out = Vec::with_capacity(nalu.len() + 4);
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nalu);
    out
}

pub(crate) fn raw_annexb_is_keyframe(payload: &[u8]) -> bool {
    split_annexb_nalus(payload).iter().any(|nalu| {
        if nalu.is_empty() {
            return false;
        }

        let h264_nal_type = nalu[0] & 0x1F;
        if h264_nal_type == 5 {
            return true;
        }

        if nalu.len() < 2 {
            return false;
        }
        matches!((nalu[0] >> 1) & 0x3F, 16..=23)
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AnnexbCodecKind {
    #[default]
    Unknown,
    H264,
    H265,
}

/// Zero-allocation variant of [`audio_for_ts`].
///
/// For `Raw` with ADTS: returns `Some(payload)` directly — zero-copy.
/// All other cases write into `buf` (cleared first) and return `Some(&buf[..])`.
/// Returns `None` for config/sequence packets.
#[inline]
pub fn audio_for_ts_into<'a>(
    payload: &'a [u8],
    format: PayloadFormat,
    sample_rate: u32,
    channels: u32,
    buf: &'a mut Vec<u8>,
) -> Option<&'a [u8]> {
    match format {
        PayloadFormat::Raw => {
            if payload.is_empty() {
                return None;
            }
            if has_adts_sync(payload) {
                Some(payload) // zero-copy, buf untouched
            } else {
                buf.clear();
                prepend_adts_into(payload, sample_rate, channels, buf);
                Some(buf.as_slice())
            }
        }
        PayloadFormat::Flv => {
            if payload.len() <= 2 || payload[1] == 0 {
                return None;
            }
            let raw_aac = &payload[2..];
            buf.clear();
            prepend_adts_into(raw_aac, sample_rate, channels, buf);
            Some(buf.as_slice())
        }
    }
}

// ---------------------------------------------------------------------------
// High-level: payload → RTMP/FLV ready
// ---------------------------------------------------------------------------

/// Prepare a Raw (Annex B) video payload for RTMP publishing.
///
/// Converts Annex B → AVCC, wraps in 5-byte FLV video tag header.
/// Returns `None` if the converted payload is empty.
pub fn video_for_rtmp(payload: &[u8], is_keyframe: bool) -> Option<Vec<u8>> {
    // Single allocation: write FLV header then AVCC inline — no intermediate Vec.
    let tag = if is_keyframe { 0x17u8 } else { 0x27u8 };
    let mut out = Vec::with_capacity(payload.len() + 5);
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
    out.extend_from_slice(&[tag, 1, 0, 0, 0]);
    write_signed_be24(composition_time_ms, &mut out[2..5]);
    annexb_to_avcc_into(payload, out)
}

fn write_signed_be24(value: i32, out: &mut [u8]) {
    debug_assert!(out.len() >= 3);
    let clamped = value.clamp(-8_388_608, 8_388_607);
    let encoded = (clamped as u32) & 0x00FF_FFFF;
    out[0] = (encoded >> 16) as u8;
    out[1] = (encoded >> 8) as u8;
    out[2] = encoded as u8;
}

/// Prepare a Raw audio payload for RTMP publishing.
///
/// Strips ADTS header if present, prepends 2-byte FLV audio header `[0xAF, 0x01]`.
pub fn audio_for_rtmp(payload: &[u8]) -> Vec<u8> {
    let raw = strip_adts(payload);
    let mut out = Vec::with_capacity(raw.len() + 2);
    out.extend_from_slice(&[0xAF, 0x01]);
    out.extend_from_slice(raw);
    out
}

/// Zero-allocation variant of [`audio_for_rtmp`].
///
/// Clears `out` and writes the FLV-wrapped raw AAC into it in-place.
#[inline]
pub fn audio_for_rtmp_into(payload: &[u8], out: &mut Vec<u8>) {
    let raw = strip_adts(payload);
    out.clear();
    out.reserve(raw.len() + 2);
    out.extend_from_slice(&[0xAF, 0x01]);
    out.extend_from_slice(raw);
}

// ---------------------------------------------------------------------------
// AVCC ↔ Annex B conversion
// ---------------------------------------------------------------------------

/// Parse AVCC decoder configuration record.
/// Returns `(nalu_length_size, sps_pps_as_annexb)`.
///
/// Fails closed: if the SPS or PPS list is truncated at any point, the
/// annexb output is empty rather than containing whatever prefix parsed
/// before the truncation. A cached parameter set missing its PPS (or with a
/// PPS but no SPS) is worse than caching nothing, since it would be
/// prepended to keyframes as if it were complete.
pub fn parse_avcc_config(data: &[u8]) -> (usize, Vec<u8>) {
    if data.len() < 8 {
        return (4, Vec::new());
    }
    let nalu_len_size = ((data[4] & 0x03) + 1) as usize;
    let annexb = parse_avcc_sps_pps(data).unwrap_or_default();
    (nalu_len_size, annexb)
}

fn parse_avcc_sps_pps(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let num_sps = (data[5] & 0x1F) as usize;
    let mut pos = 6usize;
    for _ in 0..num_sps {
        let len = u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]) as usize;
        pos += 2;
        let sps = data.get(pos..pos + len)?;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(sps);
        pos += len;
    }
    let num_pps = *data.get(pos)? as usize;
    pos += 1;
    for _ in 0..num_pps {
        let len = u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]) as usize;
        pos += 2;
        let pps = data.get(pos..pos + len)?;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(pps);
        pos += len;
    }
    Some(out)
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
pub fn avcc_to_annexb_into(data: &[u8], nalu_len_size: usize, out: &mut Vec<u8>) {
    let mut pos = 0;
    while pos + nalu_len_size <= data.len() {
        let nalu_len = match nalu_len_size {
            1 => data[pos] as usize,
            2 => u16::from_be_bytes([data[pos], data[pos + 1]]) as usize,
            3 => {
                ((data[pos] as usize) << 16)
                    | ((data[pos + 1] as usize) << 8)
                    | (data[pos + 2] as usize)
            }
            _ => u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                as usize,
        };
        pos += nalu_len_size;
        if nalu_len == 0 || pos + nalu_len > data.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[pos..pos + nalu_len]);
        pos += nalu_len;
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
/// # Implementation choice: two-pass (split_annexb_nalus) over single-pass (Peekable)
///
/// A streaming single-pass variant using `memmem::find_iter().peekable()` was
/// benchmarked on 2026-06-23 (bench-dev, x86-64, Zen-family) and was
/// **25–31% slower** for the dominant 1-NALU P-frame case (~890 ns vs ~690 ns at
/// 8 KiB). The `Peekable` iterator wrapper adds per-call overhead that exceeds
/// the cost of the two small intermediate Vecs allocated by `split_annexb_nalus`.
/// Re-benchmark on hardware with slower allocators or if NALU counts grow
/// significantly (>4 per frame) where the allocation cost might dominate.
#[inline]
pub fn annexb_to_avcc_into(data: &[u8], out: &mut Vec<u8>) -> bool {
    let nalus = split_annexb_nalus(data);
    let mut has_vcl = false;
    for nalu in &nalus {
        if nalu.is_empty() {
            continue;
        }
        let nal_type = nalu[0] & 0x1F;
        if matches!(nal_type, 7..=9) {
            continue;
        }
        if matches!(nal_type, 1..=5) {
            has_vcl = true;
        }
        let len = nalu.len() as u32;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nalu);
    }
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
        let mut start = idx;
        while start > 0 && data[start - 1] == 0 {
            start -= 1;
        }
        sc_scratch.push((start, start + (idx - start) + 3));
    }

    // Write AVCC directly from indexed spans — no Vec<&[u8]> allocation.
    for i in 0..sc_scratch.len() {
        let nalu_start = sc_scratch[i].1;
        let nalu_end = sc_scratch.get(i + 1).map(|s| s.0).unwrap_or(data.len());
        if nalu_start >= nalu_end {
            continue;
        }
        let nalu = &data[nalu_start..nalu_end];
        if nalu.is_empty() {
            continue;
        }
        let nal_type = nalu[0] & 0x1F;
        if matches!(nal_type, 7..=9) {
            continue;
        }
        out.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
        out.extend_from_slice(nalu);
    }
}

/// Locate all Annex B start codes (`0x00 0x00 0x01` and `0x00 0x00 0x00 0x01`).
/// Returns a list of `(start_index, end_index)` spans of the start codes themselves.
pub fn find_annexb_start_codes(data: &[u8]) -> Vec<(usize, usize)> {
    let mut matches = Vec::new();
    let finder = get_start_code_finder();
    for idx in finder.find_iter(data) {
        let mut start = idx;
        while start > 0 && data[start - 1] == 0 {
            start -= 1;
        }
        let sc_len = idx - start + 3;
        matches.push((start, start + sc_len));
    }
    matches
}

/// Split Annex B byte stream into individual NALUs (without start codes).
pub fn split_annexb_nalus(data: &[u8]) -> Vec<&[u8]> {
    let mut nalus = Vec::new();
    let starts = find_annexb_start_codes(data);
    for i in 0..starts.len() {
        let nalu_start = starts[i].1;
        let nalu_end = if i + 1 < starts.len() {
            starts[i + 1].0
        } else {
            data.len()
        };
        if nalu_start < nalu_end {
            nalus.push(&data[nalu_start..nalu_end]);
        }
    }
    nalus
}

/// Build an FLV video sequence header (AVCC decoder config) from Annex B keyframe data.
pub fn build_avcc_sequence_header(annexb_data: &[u8]) -> Option<Bytes> {
    let nalus = split_annexb_nalus(annexb_data);
    let sps_list: Vec<&[u8]> = nalus
        .iter()
        .filter(|n| !n.is_empty() && (n[0] & 0x1F) == 7)
        .copied()
        .collect();
    let pps_list: Vec<&[u8]> = nalus
        .iter()
        .filter(|n| !n.is_empty() && (n[0] & 0x1F) == 8)
        .copied()
        .collect();

    let sps = sps_list.first()?;
    if sps.len() < 4 {
        return None;
    }

    let mut buf = Vec::with_capacity(64);
    // FLV video tag: keyframe(0x17) + sequence header(0x00) + composition time(0,0,0)
    buf.extend_from_slice(&[0x17, 0x00, 0x00, 0x00, 0x00]);
    // AVCDecoderConfigurationRecord
    buf.push(1); // configurationVersion
    buf.push(sps[1]); // AVCProfileIndication
    buf.push(sps[2]); // profile_compatibility
    buf.push(sps[3]); // AVCLevelIndication
    buf.push(0xFF); // lengthSizeMinusOne = 3 (4 bytes)

    buf.push(0xE0 | sps_list.len() as u8);
    for s in &sps_list {
        buf.extend_from_slice(&(s.len() as u16).to_be_bytes());
        buf.extend_from_slice(s);
    }
    buf.push(pps_list.len() as u8);
    for p in &pps_list {
        buf.extend_from_slice(&(p.len() as u16).to_be_bytes());
        buf.extend_from_slice(p);
    }

    Some(Bytes::from(buf))
}

/// Build an FLV audio sequence header (AudioSpecificConfig) from sample rate
/// and channel count. Used for the SRT→RTMP Raw path where no cached
/// AudioSpecificConfig exists — the 2-byte config is synthesized from the
/// audio metadata that is always available.
pub fn build_aac_sequence_header(sample_rate: u32, channels: u32) -> Bytes {
    let freq_idx: u8 = match sample_rate {
        96000 => 0,
        88200 => 1,
        64000 => 2,
        48000 => 3,
        44100 => 4,
        32000 => 5,
        24000 => 6,
        22050 => 7,
        16000 => 8,
        12000 => 9,
        11025 => 10,
        8000 => 11,
        _ => 3,
    };
    let chan_cfg = channels.min(7) as u8;
    let audio_object_type: u8 = 2; // AAC-LC

    // AudioSpecificConfig (2 bytes for AAC-LC without extension)
    // byte0: bits[7:3] = audioObjectType, bits[2:0] = samplingFrequencyIndex top 3 bits
    let asc_byte0 = (audio_object_type << 3) | (freq_idx >> 1);
    // byte1: bit[7] = samplingFrequencyIndex bottom bit, bits[6:3] = channelConfiguration
    let asc_byte1 = ((freq_idx & 0x01) << 7) | (chan_cfg << 3);

    let mut out = Vec::with_capacity(4);
    // FLV audio tag: AAC (0xAF) + packet_type=0 (sequence header)
    out.extend_from_slice(&[0xAF, 0x00]);
    out.extend_from_slice(&[asc_byte0, asc_byte1]);
    Bytes::from(out)
}

// ---------------------------------------------------------------------------
// ADTS helpers
// ---------------------------------------------------------------------------

/// Build a 7-byte ADTS header for an AAC frame.
pub fn build_adts_header(frame_len: usize, sample_rate: u32, channels: u32) -> [u8; 7] {
    let freq_idx: u8 = match sample_rate {
        96000 => 0,
        88200 => 1,
        64000 => 2,
        48000 => 3,
        44100 => 4,
        32000 => 5,
        24000 => 6,
        22050 => 7,
        16000 => 8,
        12000 => 9,
        11025 => 10,
        8000 => 11,
        _ => 3,
    };
    let chan_cfg = channels.min(7) as u8;
    let total_len = (frame_len + 7) as u16;
    let mut hdr = [0u8; 7];
    hdr[0] = 0xFF;
    hdr[1] = 0xF1; // MPEG-4, Layer 0, no CRC
    hdr[2] = (1 << 6) | (freq_idx << 2) | (chan_cfg >> 2); // AAC-LC profile
    hdr[3] = ((chan_cfg & 0x03) << 6) | ((total_len >> 11) as u8 & 0x03);
    hdr[4] = (total_len >> 3) as u8;
    hdr[5] = ((total_len & 0x07) << 5) as u8 | 0x1F;
    hdr[6] = 0xFC;
    hdr
}

fn has_adts_sync(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0xFF && (data[1] & 0xF0) == 0xF0
}

/// Count complete AAC ADTS frames in a payload.
pub fn adts_frame_count(data: &[u8]) -> usize {
    let mut pos = 0usize;
    let mut count = 0usize;
    while pos + 7 <= data.len() {
        let frame = &data[pos..];
        if !has_adts_sync(frame) {
            break;
        }
        let frame_len = (((frame[3] & 0x03) as usize) << 11)
            | ((frame[4] as usize) << 3)
            | (((frame[5] & 0xE0) as usize) >> 5);
        if frame_len < 7 || pos + frame_len > data.len() {
            break;
        }
        count += 1;
        pos += frame_len;
    }
    count
}

fn prepend_adts(raw_aac: &[u8], sample_rate: u32, channels: u32) -> Vec<u8> {
    let adts = build_adts_header(raw_aac.len(), sample_rate, channels);
    let mut out = Vec::with_capacity(7 + raw_aac.len());
    out.extend_from_slice(&adts);
    out.extend_from_slice(raw_aac);
    out
}

/// Like [`prepend_adts`] but writes into a caller-provided reusable buffer.
fn prepend_adts_into(raw_aac: &[u8], sample_rate: u32, channels: u32, out: &mut Vec<u8>) {
    let adts = build_adts_header(raw_aac.len(), sample_rate, channels);
    out.reserve(7 + raw_aac.len());
    out.extend_from_slice(&adts);
    out.extend_from_slice(raw_aac);
}

/// Strip ADTS header if present, returning the raw AAC frame data.
pub fn strip_adts(data: &[u8]) -> &[u8] {
    if has_adts_sync(data) && data.len() >= 7 {
        // protection_absent bit (byte 1, bit 0): 1 = no CRC (7-byte header), 0 = CRC (9-byte)
        let hdr_len = if data[1] & 0x01 == 1 { 7 } else { 9 };
        if data.len() > hdr_len {
            return &data[hdr_len..];
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn push_bits(bits: &mut Vec<bool>, value: u64, width: usize) {
        for shift in (0..width).rev() {
            bits.push(((value >> shift) & 1) == 1);
        }
    }

    fn push_ue(bits: &mut Vec<bool>, value: u64) {
        let code_num = value + 1;
        let width = 64 - code_num.leading_zeros() as usize;
        bits.extend(std::iter::repeat_n(false, width.saturating_sub(1)));
        push_bits(bits, code_num, width);
    }

    fn pack_bits(bits: &[bool]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| {
                chunk.iter().enumerate().fold(0u8, |byte, (index, bit)| {
                    byte | (u8::from(*bit) << (7 - index))
                })
            })
            .collect()
    }

    fn insert_emulation_prevention(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let mut zero_run = 0u8;
        for &byte in data {
            if zero_run >= 2 && byte <= 3 {
                out.push(3);
                zero_run = 0;
            }
            out.push(byte);
            zero_run = if byte == 0 {
                zero_run.saturating_add(1)
            } else {
                0
            };
        }
        out
    }

    fn minimal_hevc_sps_nalu(chroma_format_idc: u64, bit_depth_minus8: u64) -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, 0, 4);
        push_bits(&mut bits, 0, 3);
        push_bits(&mut bits, 1, 1);
        push_bits(&mut bits, 0, 2);
        push_bits(&mut bits, 0, 1);
        push_bits(&mut bits, 2, 5);
        push_bits(&mut bits, 0, 32);
        push_bits(&mut bits, 0, 48);
        push_bits(&mut bits, 0x7b, 8);
        push_ue(&mut bits, 0);
        push_ue(&mut bits, chroma_format_idc);
        push_ue(&mut bits, 1920);
        push_ue(&mut bits, 1080);
        push_bits(&mut bits, 0, 1);
        push_ue(&mut bits, bit_depth_minus8);
        push_ue(&mut bits, bit_depth_minus8);

        let mut sps = vec![0x42, 0x01];
        sps.extend(insert_emulation_prevention(&pack_bits(&bits)));
        sps
    }

    #[test]
    fn avcc_annexb_round_trip() {
        // SPS (type 7) + PPS (type 8) + IDR (type 5) as Annex B
        let annexb = [
            0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0xAB, // SPS
            0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80, // PPS
            0, 0, 0, 1, 0x65, 0x88, 0x80, 0x40, // IDR slice
        ];

        // annexb_to_avcc should filter SPS/PPS/AUD and keep only IDR
        let avcc = annexb_to_avcc(&annexb);
        assert!(!avcc.is_empty());
        // First 4 bytes = length of the IDR NALU
        let nalu_len = u32::from_be_bytes([avcc[0], avcc[1], avcc[2], avcc[3]]) as usize;
        assert_eq!(nalu_len, 4); // IDR data: 0x65 0x88 0x80 0x40
        assert_eq!(avcc[4] & 0x1F, 5); // IDR NAL type

        // Convert back
        let back = avcc_to_annexb(&avcc, 4);
        assert_eq!(&back[..4], &[0, 0, 0, 1]); // start code
        assert_eq!(back[4] & 0x1F, 5); // IDR
    }

    #[test]
    fn parse_avcc_config_extracts_sps_pps() {
        // Minimal AVCC config: version=1, profile=66, compat=0, level=30, len_size=4
        let mut config = vec![
            1, 66, 0, 30, 0xFF, // lengthSizeMinusOne = 3 → 4 bytes
        ];
        // 1 SPS
        let sps = [0x67, 0x42, 0x00, 0x1E];
        config.push(0xE1); // num_sps = 1
        config.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        config.extend_from_slice(&sps);
        // 1 PPS
        let pps = [0x68, 0xCE, 0x38, 0x80];
        config.push(1); // num_pps = 1
        config.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        config.extend_from_slice(&pps);

        let (nls, annexb) = parse_avcc_config(&config);
        assert_eq!(nls, 4);
        // Should contain start_code + SPS + start_code + PPS
        assert!(annexb.len() > 8);
        assert_eq!(&annexb[..4], &[0, 0, 0, 1]);
        assert_eq!(annexb[4], 0x67); // SPS NAL type
    }

    #[test]
    fn adts_round_trip() {
        let raw_aac = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02];
        let with_adts = prepend_adts(&raw_aac, 48000, 2);
        assert_eq!(with_adts.len(), 7 + raw_aac.len());
        assert!(has_adts_sync(&with_adts));
        let stripped = strip_adts(&with_adts);
        assert_eq!(stripped, &raw_aac[..]);
    }

    #[test]
    fn adts_frame_count_counts_complete_frames() {
        let mut payload = Vec::new();
        let frame_a = build_adts_header(2, 48000, 2);
        payload.extend_from_slice(&frame_a);
        payload.extend_from_slice(&[0x11, 0x22]);
        let frame_b = build_adts_header(3, 48000, 2);
        payload.extend_from_slice(&frame_b);
        payload.extend_from_slice(&[0x33, 0x44, 0x55]);

        assert_eq!(adts_frame_count(&payload), 2);
        payload.pop();
        assert_eq!(
            adts_frame_count(&payload),
            1,
            "truncated trailing frames must not be counted"
        );
    }

    proptest! {
        #[test]
        fn adts_frame_count_matches_generated_complete_frames(
            frame_sizes in proptest::collection::vec(1usize..64, 0..12),
            truncate_tail in 0usize..8,
        ) {
            let mut payload = Vec::new();
            for (idx, frame_size) in frame_sizes.iter().copied().enumerate() {
                let frame = build_adts_header(frame_size, 48000, 2);
                payload.extend_from_slice(&frame);
                payload.extend(std::iter::repeat_n(idx as u8, frame_size));
            }
            let mut expected = frame_sizes.len();
            if truncate_tail > 0 && !payload.is_empty() {
                let remove = truncate_tail.min(payload.len());
                payload.truncate(payload.len() - remove);
                expected = expected.saturating_sub(1);
            }
            prop_assert_eq!(adts_frame_count(&payload), expected);
        }
    }

    #[test]
    fn video_for_ts_flv_passthrough_raw() {
        let annexb_payload = vec![0, 0, 0, 1, 0x65, 0x88];
        let mut nls = 4;
        let mut cache = Vec::new();
        let result = video_for_ts(&annexb_payload, PayloadFormat::Raw, &mut nls, &mut cache);
        assert!(result.is_some());
        // Raw should be zero-copy
        assert!(matches!(result, Some(Cow::Borrowed(_))));
        assert_eq!(&*result.unwrap(), &annexb_payload[..]);
    }

    #[test]
    fn audio_for_ts_adds_adts_for_raw_without() {
        let raw_aac = vec![0xDE, 0xAD];
        let result = audio_for_ts(&raw_aac, PayloadFormat::Raw, 48000, 2);
        assert!(result.is_some());
        let data = result.unwrap();
        assert!(has_adts_sync(&data));
        assert_eq!(&data[7..], &raw_aac[..]);
    }

    #[test]
    fn audio_for_ts_passes_through_existing_adts() {
        let mut with_adts = Vec::from(build_adts_header(4, 48000, 2));
        with_adts.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        let result = audio_for_ts(&with_adts, PayloadFormat::Raw, 48000, 2);
        assert!(matches!(result, Some(Cow::Borrowed(_))));
    }

    #[test]
    fn build_avcc_seq_header_from_annexb() {
        let annexb = [
            0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0xAB, // SPS
            0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80, // PPS
            0, 0, 0, 1, 0x65, 0x88, 0x80, 0x40, // IDR
        ];
        let seq_hdr = build_avcc_sequence_header(&annexb).unwrap();
        // FLV tag: keyframe + seq header
        assert_eq!(seq_hdr[0], 0x17);
        assert_eq!(seq_hdr[1], 0x00);
        // AVCC config version
        assert_eq!(seq_hdr[5], 1);
    }

    #[test]
    fn video_for_rtmp_converts_annexb() {
        let annexb = [0, 0, 0, 1, 0x65, 0x88, 0x80, 0x40];
        let result = video_for_rtmp(&annexb, true).unwrap();
        assert_eq!(result[0], 0x17); // keyframe tag
        assert_eq!(result[1], 1); // data packet
        // AVCC data starts at offset 5
        let nalu_len = u32::from_be_bytes([result[5], result[6], result[7], result[8]]) as usize;
        assert_eq!(nalu_len, 4);
    }

    #[test]
    fn video_for_rtmp_preserves_positive_composition_time() {
        let annexb = [0, 0, 0, 1, 0x65, 0x88, 0x80, 0x40];
        let mut out = Vec::new();

        assert!(video_for_rtmp_with_composition_into(
            &annexb, true, 40, &mut out
        ));

        assert_eq!(&out[..5], &[0x17, 0x01, 0x00, 0x00, 0x28]);
    }

    #[test]
    fn video_for_rtmp_preserves_negative_composition_time() {
        let annexb = [0, 0, 0, 1, 0x41, 0x88, 0x80, 0x40];
        let mut out = Vec::new();

        assert!(video_for_rtmp_with_composition_into(
            &annexb, false, -40, &mut out
        ));

        assert_eq!(&out[..5], &[0x27, 0x01, 0xff, 0xff, 0xd8]);
    }

    #[test]
    fn hevc_video_for_enhanced_rtmp_uses_coded_frames_x_for_zero_composition() {
        let annexb = [
            0, 0, 0, 1, 0x40, 0x01, 0xAA, 0, 0, 0, 1, 0x42, 0x01, 0x01, 0x01, 0x60, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0x78, 0, 0, 0, 1, 0x44, 0x01, 0xCC, 0, 0, 0, 1, 0x26, 0x01, 0xDE, 0xAD,
        ];
        let mut out = Vec::new();

        assert!(hevc_video_for_enhanced_rtmp_with_composition_into(
            &annexb, true, 0, &mut out
        ));

        assert_eq!(&out[..5], &[0x93, b'h', b'v', b'c', b'1']);
        let nalu_len = u32::from_be_bytes([out[5], out[6], out[7], out[8]]);
        assert_eq!(nalu_len, 4);
        assert_eq!(&out[9..], &[0x26, 0x01, 0xDE, 0xAD]);
    }

    #[test]
    fn hevc_video_for_enhanced_rtmp_writes_composition_for_nonzero_offset() {
        let annexb = [0, 0, 0, 1, 0x26, 0x01, 0xDE, 0xAD];
        let mut out = Vec::new();

        assert!(hevc_video_for_enhanced_rtmp_with_composition_into(
            &annexb, true, 40, &mut out
        ));

        assert_eq!(&out[..8], &[0x91, b'h', b'v', b'c', b'1', 0, 0, 40]);
        let nalu_len = u32::from_be_bytes([out[8], out[9], out[10], out[11]]);
        assert_eq!(nalu_len, 4);
        assert_eq!(&out[12..], &[0x26, 0x01, 0xDE, 0xAD]);
    }

    #[test]
    fn hevc_enhanced_rtmp_sequence_header_uses_hvc1_fourcc() {
        let sps = minimal_hevc_sps_nalu(1, 2);
        let mut annexb = vec![0, 0, 0, 1, 0x40, 0x01, 0xAA];
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(&sps);
        annexb.extend_from_slice(&[0, 0, 0, 1, 0x44, 0x01, 0xCC]);

        let seq_hdr = build_hevc_enhanced_rtmp_sequence_header(&annexb).unwrap();

        assert_eq!(&seq_hdr[..5], &[0x80, b'h', b'v', b'c', b'1']);
        assert_eq!(seq_hdr[5], 1);
        assert_eq!(seq_hdr[6] & 0x1f, 2);
        assert_eq!(seq_hdr[17], 0x7b);
        assert_eq!(seq_hdr[21], 0xfd);
        assert_eq!(seq_hdr[22], 0xfa);
        assert_eq!(seq_hdr[23], 0xfa);
    }

    #[test]
    fn hevc_enhanced_rtmp_sequence_header_builds_from_bf0_fixture() {
        let fixture = crate::test_fixtures::av_marker_transport_fixture_for_bframes(
            "h265",
            false,
            crate::test_fixtures::AvMarkerBframeMode::Bf0,
        )
        .expect("checked-in HEVC BF0 fixture");
        let bytes = std::fs::read(fixture).expect("read HEVC BF0 fixture");
        let mut demuxer = crate::media::mpegts::TsDemuxer::new();
        let mut packets = Vec::new();
        for chunk in bytes.chunks(1316) {
            demuxer.feed(chunk);
        }
        demuxer.flush();
        demuxer.drain_into(&mut packets);
        let first_video_prefix = packets
            .iter()
            .find(|packet| packet.media_type == crate::media::ring_buffer::MediaType::Video)
            .map(|packet| {
                packet
                    .payload
                    .iter()
                    .take(16)
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_else(|| "none".to_string());
        let parameter_sets = packets
            .iter()
            .find_map(|packet| {
                (packet.media_type == crate::media::ring_buffer::MediaType::Video)
                    .then(|| annexb_parameter_sets(&packet.payload))
                    .flatten()
            })
            .unwrap_or_else(|| {
                panic!(
                    "fixture should carry HEVC parameter sets; packets={} first_video={}",
                    packets.len(),
                    first_video_prefix
                )
            });

        let seq_hdr = build_hevc_enhanced_rtmp_sequence_header(&parameter_sets)
            .expect("fixture HEVC parameter sets should build Enhanced RTMP hvcC");

        assert_eq!(&seq_hdr[..5], &[0x80, b'h', b'v', b'c', b'1']);
        assert_eq!(seq_hdr[21], 0xfd);
    }

    #[test]
    fn video_for_rtmp_rejects_non_vcl_annexb_payload() {
        let sei_only = [0, 0, 0, 1, 0x06, 0x05, 0xff, 0xff];
        let mut out = Vec::new();

        assert!(!video_for_rtmp_with_composition_into(
            &sei_only, true, 0, &mut out
        ));
        assert_eq!(&out[..5], &[0x17, 0x01, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn build_aac_seq_header_synthesizes_correct_config() {
        // AAC-LC (audioObjectType=2), 48000Hz (freq_idx=3), stereo (ch=2)
        // asc_byte0 = (2 << 3) | (3 >> 1) = 16 | 1 = 0x11
        // asc_byte1 = ((3 & 1) << 7) | (2 << 3) = 128 | 16 = 0x90
        let hdr = build_aac_sequence_header(48000, 2);
        assert_eq!(hdr.len(), 4);
        assert_eq!(hdr[0], 0xAF); // AAC, 44kHz, 16-bit, stereo
        assert_eq!(hdr[1], 0x00); // packet_type = 0 (sequence header)
        assert_eq!(hdr[2], 0x11);
        assert_eq!(hdr[3], 0x90);

        // AAC-LC, 44100Hz (freq_idx=4), mono (ch=1)
        // asc_byte0 = (2 << 3) | (4 >> 1) = 16 | 2 = 0x12
        // asc_byte1 = ((4 & 1) << 7) | (1 << 3) = 0 | 8 = 0x08
        let hdr2 = build_aac_sequence_header(44100, 1);
        assert_eq!(hdr2.len(), 4);
        assert_eq!(hdr2[0], 0xAF);
        assert_eq!(hdr2[1], 0x00);
        assert_eq!(hdr2[2], 0x12);
        assert_eq!(hdr2[3], 0x08);
    }

    #[test]
    fn audio_for_rtmp_strips_adts() {
        let raw = [0xDE, 0xAD, 0xBE, 0xEF];
        let mut with_adts = Vec::from(build_adts_header(raw.len(), 48000, 2));
        with_adts.extend_from_slice(&raw);

        let result = audio_for_rtmp(&with_adts);
        assert_eq!(result[0], 0xAF);
        assert_eq!(result[1], 0x01);
        assert_eq!(&result[2..], &raw);
    }

    // -----------------------------------------------------------------------
    // annexb_to_avcc_into oracle: streaming implementation must match the
    // two-pass split_annexb_nalus reference for all inputs.
    // -----------------------------------------------------------------------

    /// Reference implementation that uses two intermediate Vecs (the original path).
    fn annexb_to_avcc_reference(data: &[u8]) -> Vec<u8> {
        let nalus = split_annexb_nalus(data);
        let mut out = Vec::new();
        for nalu in &nalus {
            if nalu.is_empty() {
                continue;
            }
            let nal_type = nalu[0] & 0x1F;
            if matches!(nal_type, 7..=9) {
                continue;
            }
            out.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
            out.extend_from_slice(nalu);
        }
        out
    }

    #[test]
    fn annexb_to_avcc_into_matches_reference_single_nalu_4byte_sc() {
        let data = [0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB];
        let reference = annexb_to_avcc_reference(&data);
        let mut streaming = Vec::new();
        annexb_to_avcc_into(&data, &mut streaming);
        assert_eq!(streaming, reference);
    }

    #[test]
    fn annexb_to_avcc_into_matches_reference_single_nalu_3byte_sc() {
        let data = [0x00, 0x00, 0x01, 0x41, 0xCC, 0xDD];
        let reference = annexb_to_avcc_reference(&data);
        let mut streaming = Vec::new();
        annexb_to_avcc_into(&data, &mut streaming);
        assert_eq!(streaming, reference);
    }

    #[test]
    fn annexb_to_avcc_into_matches_reference_multiple_nalus_mixed_sc() {
        // 3-byte SC + IDR, 4-byte SC + P-slice, 3-byte SC + P-slice
        let data = [
            0x00, 0x00, 0x01, 0x65, 0x11, 0x22, // IDR (3-byte SC)
            0x00, 0x00, 0x00, 0x01, 0x41, 0x33, 0x44, // P-slice (4-byte SC)
            0x00, 0x00, 0x01, 0x41, 0x55, // P-slice (3-byte SC)
        ];
        let reference = annexb_to_avcc_reference(&data);
        let mut streaming = Vec::new();
        annexb_to_avcc_into(&data, &mut streaming);
        assert_eq!(streaming, reference);
    }

    #[test]
    fn annexb_to_avcc_into_matches_reference_filters_sps_pps_aud() {
        // SPS (7), PPS (8), AUD (9), IDR (5) — only IDR should appear in output
        let data = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, // SPS
            0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, // PPS
            0x00, 0x00, 0x00, 0x01, 0x09, 0xF0, // AUD
            0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80, // IDR
        ];
        let reference = annexb_to_avcc_reference(&data);
        let mut streaming = Vec::new();
        annexb_to_avcc_into(&data, &mut streaming);
        assert_eq!(streaming, reference);
        // Only IDR should remain
        assert_eq!(&streaming[4] & 0x1F, 5);
    }

    #[test]
    fn annexb_to_avcc_into_appends_to_existing_content() {
        let marker = b"MARKER".to_vec();
        let data = [0x00, 0x00, 0x00, 0x01, 0x41, 0xBB];
        let mut out = marker.clone();
        annexb_to_avcc_into(&data, &mut out);
        assert!(out.starts_with(&marker));
        assert!(out.len() > marker.len());
    }

    #[test]
    fn annexb_to_avcc_with_scratch_matches_reference() {
        // Same oracle tests as annexb_to_avcc_into — with_scratch must produce
        // identical output.
        let cases: &[&[u8]] = &[
            &[0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB],
            &[0x00, 0x00, 0x01, 0x41, 0xCC, 0xDD],
            &[
                0x00, 0x00, 0x01, 0x65, 0x11, 0x22, 0x00, 0x00, 0x00, 0x01, 0x41, 0x33, 0x44, 0x00,
                0x00, 0x01, 0x41, 0x55,
            ],
            &[
                0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x00, 0x00,
                0x00, 0x01, 0x09, 0xF0, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80,
            ],
        ];
        for data in cases {
            let reference = annexb_to_avcc_reference(data);
            let mut scratch_out = Vec::new();
            let mut sc = Vec::new();
            annexb_to_avcc_with_scratch(data, &mut scratch_out, &mut sc);
            assert_eq!(
                scratch_out,
                reference,
                "with_scratch mismatch for input len={}",
                data.len()
            );
        }
    }

    #[test]
    fn annexb_to_avcc_with_scratch_reuses_sc_buffer() {
        let data = [0x00, 0x00, 0x00, 0x01, 0x41, 0xBB];
        let mut out = Vec::new();
        let mut sc: Vec<(usize, usize)> = Vec::new();
        // First call: sc grows
        annexb_to_avcc_with_scratch(&data, &mut out, &mut sc);
        let cap_after_first = sc.capacity();
        out.clear();
        // Second call: sc should reuse its allocation (capacity unchanged or larger)
        annexb_to_avcc_with_scratch(&data, &mut out, &mut sc);
        assert!(
            sc.capacity() >= cap_after_first,
            "sc scratch should not shrink"
        );
    }

    // --- Edge-case tests ---

    #[test]
    fn parse_avcc_config_truncated_input() {
        assert_eq!(parse_avcc_config(&[]), (4, Vec::new()));
        assert_eq!(parse_avcc_config(&[1, 66, 0, 30]), (4, Vec::new()));
        // Below the 8-byte header floor entirely; body loop never runs.
        assert_eq!(
            parse_avcc_config(&[1, 66, 0, 30, 0xFF, 0xE1]),
            (4, Vec::new())
        );
    }

    #[test]
    fn parse_avcc_config_zero_sps_pps() {
        // 8 bytes: past the header floor, so this exercises the real
        // num_sps/num_pps == 0 loop bodies rather than the length<8 gate.
        let config = [1, 66, 0, 30, 0xFF, 0x00, 0x00, 0x00];
        let (nls, annexb) = parse_avcc_config(&config);
        assert_eq!(nls, 4);
        assert!(annexb.is_empty());
    }

    #[test]
    fn parse_avcc_config_sps_ok_but_missing_pps_count_byte_yields_no_partial_state() {
        // num_sps = 1, one valid 4-byte SPS, then the buffer ends before the
        // mandatory numPPS byte. The old implementation returned the SPS
        // alone; a decoder handed SPS with no PPS cannot decode either, so
        // the parser must fail closed and cache nothing.
        let config = [
            1, 66, 0, 30, 0xFF, // header (nalu_len_size = 4)
            0xE1, // num_sps = 1
            0x00, 0x04, // SPS length = 4
            0x67, 0x42, 0x00, 0x1E, // SPS body
        ];
        assert_eq!(parse_avcc_config(&config), (4, Vec::new()));
    }

    #[test]
    fn parse_avcc_config_sps_ok_but_pps_length_truncated_yields_no_partial_state() {
        // SPS parses cleanly, numPPS = 1, but the PPS length/body never
        // arrives. Must not leak the SPS-only prefix.
        let config = [
            1, 66, 0, 30, 0xFF, // header
            0xE1, // num_sps = 1
            0x00, 0x04, // SPS length = 4
            0x67, 0x42, 0x00, 0x1E, // SPS body
            0x01, // num_pps = 1, then buffer ends
        ];
        assert_eq!(parse_avcc_config(&config), (4, Vec::new()));
    }

    #[test]
    fn parse_avcc_config_max_declared_length_with_tiny_buffer_rejected() {
        // SPS declares a length of 0xFFFF (max u16) but only 2 bytes of body
        // actually follow; must reject without allocating for the declared
        // length or panicking on the out-of-bounds slice.
        let config = [
            1, 66, 0, 30, 0xFF, // header
            0xE1, // num_sps = 1
            0xFF, 0xFF, // SPS length = 65535
            0xAA, 0xBB, // only 2 bytes actually present
        ];
        assert_eq!(parse_avcc_config(&config), (4, Vec::new()));
    }

    proptest! {
        #[test]
        fn parse_avcc_config_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..128)) {
            let _ = parse_avcc_config(&bytes);
        }

        #[test]
        fn parse_avcc_config_truncation_always_fails_closed(
            header in any::<u8>(),
            sps_bodies in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..16), 0..3),
            pps_bodies in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..16), 0..3),
        ) {
            let mut config = vec![1u8, 66, 0, 30, header];
            config.push(0xE0 | (sps_bodies.len() as u8 & 0x1F));
            for sps in &sps_bodies {
                config.extend_from_slice(&(sps.len() as u16).to_be_bytes());
                config.extend_from_slice(sps);
            }
            config.push(pps_bodies.len() as u8);
            for pps in &pps_bodies {
                config.extend_from_slice(&(pps.len() as u16).to_be_bytes());
                config.extend_from_slice(pps);
            }

            let mut expected = Vec::new();
            for sps in &sps_bodies {
                expected.extend_from_slice(&[0, 0, 0, 1]);
                expected.extend_from_slice(sps);
            }
            for pps in &pps_bodies {
                expected.extend_from_slice(&[0, 0, 0, 1]);
                expected.extend_from_slice(pps);
            }
            let (_, full_annexb) = parse_avcc_config(&config);
            prop_assert_eq!(full_annexb, expected);

            // Any strict prefix of a well-formed buffer must fail closed
            // (empty annexb), never a partial SPS/PPS prefix.
            for cut in 0..config.len() {
                let (_, partial) = parse_avcc_config(&config[..cut]);
                prop_assert!(partial.is_empty(), "truncated at {cut} produced non-empty output");
            }
        }
    }

    #[test]
    fn build_adts_header_all_sample_rates() {
        let rates = [
            (96000, 0),
            (88200, 1),
            (64000, 2),
            (48000, 3),
            (44100, 4),
            (32000, 5),
            (24000, 6),
            (22050, 7),
            (16000, 8),
            (12000, 9),
            (11025, 10),
            (8000, 11),
        ];
        for (rate, expected_freq_idx) in rates {
            let hdr = build_adts_header(100, rate, 2);
            let actual = (hdr[2] >> 2) & 0x0F;
            assert_eq!(
                actual, expected_freq_idx,
                "ADTS freq index mismatch for {rate}Hz"
            );
        }
    }

    #[test]
    fn build_adts_header_unknown_rate_defaults_to_48k() {
        let hdr = build_adts_header(100, 99999, 2);
        assert_eq!((hdr[2] >> 2) & 0x0F, 3); // defaults to 48000
    }

    #[test]
    fn build_adts_header_channels_clamped_to_7() {
        let hdr = build_adts_header(100, 48000, 8);
        assert_eq!((hdr[2] & 0x01) << 2 | (hdr[3] >> 6), 7);
    }

    #[test]
    fn strip_adts_crc_variant() {
        let raw = [0xDE, 0xAD, 0xBE];
        // ADTS with CRC: bit 0 of byte 1 = 0 → 9-byte header
        let mut adts = vec![0xFF, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        adts.extend_from_slice(&raw);
        let stripped = strip_adts(&adts);
        assert_eq!(stripped, &raw[..]);
    }

    #[test]
    fn video_for_ts_flv_too_short_payload() {
        let mut nls = 4;
        let mut cache = Vec::new();
        assert!(video_for_ts(&[0x17, 1, 0, 0], PayloadFormat::Flv, &mut nls, &mut cache).is_none());
    }

    #[test]
    fn video_for_ts_flv_sequence_header_returns_none() {
        let mut nls = 4;
        let mut cache = Vec::new();
        // FLV video tag: frame_type=1(keyframe), codec=7(AVC), packet_type=0(seq hdr)
        // AVCC config: version=1, profile=66, compat=0, level=30, nalu_len=4, num_sps=0
        let flv_seq = [
            0x17, 0x00, 0x00, 0x00, 0x00, // FLV header
            0x01, 0x42, 0x00, 0x1E, 0xFF, 0x00, // AVCC config (8+ bytes)
            0x00, // num_pps=0
        ];
        assert!(video_for_ts(&flv_seq, PayloadFormat::Flv, &mut nls, &mut cache).is_none());
        // nal_size may be updated
        assert_eq!(nls, 4);
    }

    #[test]
    fn video_for_ts_raw_empty_returns_none() {
        let mut nls = 4;
        let mut cache = Vec::new();
        assert!(video_for_ts(&[], PayloadFormat::Raw, &mut nls, &mut cache).is_none());
    }

    #[test]
    fn video_for_ts_raw_h264_keyframe_prepends_cached_parameter_sets() {
        let payload = [0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80];
        let mut nls = 4;
        let mut cache = vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
            0xCE, 0x38, 0x80,
        ];

        let result = video_for_ts(&payload, PayloadFormat::Raw, &mut nls, &mut cache)
            .expect("cached SPS/PPS should be prepended to raw keyframes");

        assert!(matches!(result, Cow::Owned(_)));
        assert!(result.starts_with(&cache));
        assert!(result.ends_with(&payload));
    }

    #[test]
    fn video_for_ts_into_raw_h265_keyframe_prepends_cached_parameter_sets() {
        let payload = [0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xAA];
        let mut nls = 4;
        let mut cache = vec![
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
            0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
        ];
        let mut buf = Vec::new();

        let result =
            video_for_ts_into(&payload, PayloadFormat::Raw, &mut nls, &mut cache, &mut buf)
                .expect("cached VPS/SPS/PPS should be prepended to raw keyframes")
                .to_vec();

        assert_eq!(result, buf);
        assert!(result.starts_with(&cache));
        assert!(result.ends_with(&payload));
    }

    #[test]
    fn video_for_ts_raw_inline_parameter_sets_refresh_cache_without_duplication() {
        let payload = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
            0xCE, 0x38, 0x80, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80,
        ];
        let mut nls = 4;
        let mut cache = Vec::new();

        let result = video_for_ts(&payload, PayloadFormat::Raw, &mut nls, &mut cache)
            .expect("inline SPS/PPS should still pass through");

        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(cache, payload[..17]);
        assert_eq!(&*result, &payload);
    }

    #[test]
    fn annexb_parameter_sets_rejects_partial_h264_parameter_sets() {
        let payload = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x65,
            0x88, 0x80,
        ];

        assert!(
            annexb_parameter_sets(&payload).is_none(),
            "partial H.264 parameter sets should not be cached"
        );
    }

    #[test]
    fn annexb_parameter_sets_rejects_partial_h265_parameter_sets() {
        let payload = [
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
            0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xDD,
        ];

        assert!(
            annexb_parameter_sets(&payload).is_none(),
            "partial HEVC parameter sets should not be cached"
        );
    }

    #[test]
    fn annexb_parameter_sets_accepts_complete_h265_parameter_sets() {
        let payload = [
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
            0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC, 0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xDD,
        ];

        let parameter_sets = annexb_parameter_sets(&payload).expect("complete HEVC headers");
        assert_eq!(
            parameter_sets,
            vec![
                0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
                0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC
            ]
        );
    }

    #[test]
    fn audio_for_ts_flv_config_packet_returns_none() {
        // packet_type 0 (AAC sequence header) — should be dropped
        assert!(audio_for_ts(&[0xAF, 0x00, 0x12, 0x10], PayloadFormat::Flv, 48000, 2).is_none());
    }

    #[test]
    fn audio_for_ts_flv_too_short_returns_none() {
        assert!(audio_for_ts(&[0xAF], PayloadFormat::Flv, 48000, 2).is_none());
        assert!(audio_for_ts(&[0xAF, 0x01], PayloadFormat::Flv, 48000, 2).is_none());
    }

    #[test]
    fn video_for_ts_into_reuses_buffer() {
        let mut nls = 4;
        let mut sps_pps = Vec::new();
        let mut buf = vec![0xDE, 0xAD]; // pre-existing content
        // Raw passthrough — should not touch buf
        let result = video_for_ts_into(
            &[0, 0, 0, 1, 0x41, 0xBB],
            PayloadFormat::Raw,
            &mut nls,
            &mut sps_pps,
            &mut buf,
        );
        assert!(result.is_some());
        // buf is not cleared for Raw
        assert!(!buf.is_empty());
    }

    #[test]
    fn audio_for_ts_into_reuses_buffer() {
        let mut buf = vec![0xDE];
        let result = audio_for_ts_into(
            &[0xAF, 0x01, 0xDE, 0xAD],
            PayloadFormat::Flv,
            48000,
            2,
            &mut buf,
        );
        assert!(result.is_some());
        // buf was cleared and repopulated with ADTS + raw AAC
        assert!(has_adts_sync(&buf));
    }

    #[test]
    fn find_annexb_start_codes_no_match_returns_empty() {
        // No 00 00 01 pattern
        let data = [0x41, 0x42, 0x43, 0x44, 0x45];
        assert!(find_annexb_start_codes(&data).is_empty());
    }

    #[test]
    fn split_annexb_nalus_empty_input() {
        assert!(split_annexb_nalus(&[]).is_empty());
    }

    #[test]
    fn split_annexb_nalus_no_start_code_returns_empty() {
        assert!(split_annexb_nalus(&[0x41, 0x42, 0x43]).is_empty());
    }

    #[test]
    fn build_avcc_sequence_header_insufficient_sps() {
        let annexb = [0, 0, 0, 1, 0x67, 0x42]; // SPS with only 2 bytes (need 4+)
        assert!(build_avcc_sequence_header(&annexb).is_none());
    }

    #[test]
    fn build_avcc_sequence_header_no_pps_still_works() {
        // Only SPS, no PPS — should still produce output
        let annexb = [0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0xAB];
        let hdr = build_avcc_sequence_header(&annexb);
        assert!(hdr.is_some());
    }

    #[test]
    fn video_for_rtmp_into_reuses_buffer() {
        let annexb = [0, 0, 0, 1, 0x65, 0x88, 0x80, 0x40];
        let mut buf = vec![0xDE, 0xAD]; // pre-existing
        assert!(video_for_rtmp_into(&annexb, true, &mut buf));
        // buf was cleared, FLV header + AVCC written
        assert_eq!(buf[0], 0x17);
        assert_eq!(buf[1], 1);
    }

    #[test]
    fn audio_for_rtmp_into_reuses_buffer() {
        let raw = [0xDE, 0xAD, 0xBE, 0xEF];
        let mut buf = vec![0x01, 0x02]; // pre-existing
        audio_for_rtmp_into(&raw, &mut buf);
        // buf was cleared, FLV header + raw AAC written
        assert_eq!(buf[0], 0xAF);
        assert_eq!(buf[1], 0x01);
        assert_eq!(&buf[2..], &raw);
    }

    #[test]
    fn audio_for_rtmp_no_adts_passthrough() {
        let raw = [0xDE, 0xAD, 0xBE, 0xEF];
        let result = audio_for_rtmp(&raw);
        assert_eq!(result[0], 0xAF);
        assert_eq!(result[1], 0x01);
        assert_eq!(&result[2..], &raw);
    }
}
