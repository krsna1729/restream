#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
use std::borrow::Cow;

use restream_core::PayloadFormat;
use restream_rtmp::enhanced_rtmp_hevc::{
    EX_CODED_FRAMES, EX_CODED_FRAMES_X, EX_SEQUENCE_START, ex_video_tag,
};

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
/// use restream_core::PayloadFormat;
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
            if let Some(tag) = ex_video_tag(payload) {
                return match tag.packet_type {
                    EX_SEQUENCE_START => {
                        // Fail closed like the classic header below: a bad
                        // record keeps the cache it had.
                        if let Some((nls, annexb)) = flv_video_config(payload) {
                            *nalu_len_size = nls;
                            *sps_pps_cache = annexb;
                        }
                        None
                    }
                    EX_CODED_FRAMES | EX_CODED_FRAMES_X => length_prefixed_frame_to_annexb(
                        tag.keyframe,
                        tag.body,
                        *nalu_len_size,
                        sps_pps_cache,
                        buf,
                    ),
                    _ => None,
                };
            }
            let (&[tag, packet_type, ..], body) = payload.split_first_chunk::<5>()?;
            if body.is_empty() {
                return None;
            }
            if packet_type == 0 && tag & 0x0F == 12 {
                if let Some((nls, annexb)) = flv_video_config(payload) {
                    *nalu_len_size = nls;
                    *sps_pps_cache = annexb;
                }
                None
            } else if packet_type == 0 {
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
                length_prefixed_frame_to_annexb(
                    (tag & 0xF0) == 0x10,
                    body,
                    *nalu_len_size,
                    sps_pps_cache,
                    buf,
                )
            }
        }
    }
}

/// One frame's length-prefixed NAL units as Annex B, a keyframe led by the
/// cached parameter sets; `None` when no NAL unit parses.
fn length_prefixed_frame_to_annexb<'a>(
    keyframe: bool,
    body: &[u8],
    nalu_len_size: usize,
    sps_pps_cache: &[u8],
    buf: &'a mut Vec<u8>,
) -> Option<&'a [u8]> {
    if keyframe && !sps_pps_cache.is_empty() {
        buf.extend_from_slice(sps_pps_cache);
    }
    let before = buf.len();
    avcc_to_annexb_into(body, nalu_len_size, buf);
    if buf.len() == before {
        return None;
    }
    Some(buf.as_slice())
}

/// The NALU length width and Annex B parameter sets of an FLV video
/// sequence header: a classic H.264 one, or an enhanced RTMP `avc1`/`hvc1`
use restream_rtmp::flv::flv_video_config;

fn refresh_annexb_parameter_set_cache(payload: &[u8], sps_pps_cache: &mut Vec<u8>) -> bool {
    let Some(parameter_sets) = annexb_parameter_sets(payload) else {
        return false;
    };

    *sps_pps_cache = parameter_sets;
    true
}

use restream_core::codec::{
    annexb_parameter_sets, avcc_to_annexb, avcc_to_annexb_into, parse_avcc_config,
    raw_annexb_is_keyframe,
};
