use std::borrow::Cow;

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
            if payload.len() <= 5 {
                return None;
            }
            if payload[1] == 0 {
                // Sequence header — cache SPS/PPS Annex B for inline injection.
                // A malformed/truncated header parses to an empty Vec; only
                // overwrite the cache on success so a bad header can't wipe out
                // a previously cached, still-valid parameter set.
                let (nls, annexb) = parse_avcc_config(&payload[5..]);
                *nalu_len_size = nls;
                if !annexb.is_empty() {
                    *sps_pps_cache = annexb;
                }
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
            if payload.len() <= 5 {
                return None;
            }
            if payload[1] == 0 {
                // Sequence header — update SPS/PPS cache, no frame to emit.
                // A malformed/truncated header parses to an empty Vec; only
                // overwrite the cache on success (see the Raw-format sibling
                // above for the matching fail-closed pattern).
                let (nls, annexb) = parse_avcc_config(&payload[5..]);
                *nalu_len_size = nls;
                if !annexb.is_empty() {
                    *sps_pps_cache = annexb;
                }
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

pub(super) fn write_signed_be24(value: i32, out: &mut [u8]) {
    debug_assert!(out.len() >= 3);
    let clamped = value.clamp(-8_388_608, 8_388_607);
    let encoded = (clamped as u32) & 0x00FF_FFFF;
    out[0] = (encoded >> 16) as u8;
    out[1] = (encoded >> 8) as u8;
    out[2] = encoded as u8;
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
