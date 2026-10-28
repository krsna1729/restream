#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
use std::ops::ControlFlow;

use bytes::Bytes;

use restream_core::{for_each_annexb_nalu, signed_be24, take_u16_prefixed};

/// Enhanced RTMP (E-RTMP v1) video packet types.
pub const EX_SEQUENCE_START: u8 = 0;
pub const EX_CODED_FRAMES: u8 = 1;
pub const EX_CODED_FRAMES_X: u8 = 3;
/// An enhanced RTMP video tag (`IsExHeader` set), borrowed. Only the H.264
/// and H.265 FourCCs (`avc1`, `hvc1`) carry the 24-bit composition time on
/// coded frames, so other FourCCs are not read.
#[derive(Debug, PartialEq, Eq)]
pub struct ExVideoTag<'a> {
    pub fourcc: [u8; 4],
    pub packet_type: u8,
    pub keyframe: bool,
    pub composition_time_ms: i32,
    /// The configuration record (sequence start) or the length-prefixed
    /// NAL units (coded frames).
    pub body: &'a [u8],
}

impl ExVideoTag<'_> {
    pub fn is_hevc(&self) -> bool {
        &self.fourcc == b"hvc1"
    }
}

/// Reads an enhanced RTMP video tag header; `None` for a classic FLV video
/// tag, another codec, or a truncated header.
pub fn ex_video_tag(payload: &[u8]) -> Option<ExVideoTag<'_>> {
    let (&[first, a, b, c, d], rest) = payload.split_first_chunk::<5>()?;
    if first & 0x80 == 0 {
        return None;
    }
    let fourcc = [a, b, c, d];
    if &fourcc != b"hvc1" && &fourcc != b"avc1" {
        return None;
    }
    let packet_type = first & 0x0F;
    let (composition_time_ms, body) = if packet_type == EX_CODED_FRAMES {
        let (&[high, mid, low], body) = rest.split_first_chunk::<3>()?;
        let sign = if high & 0x80 != 0 { 0xFF } else { 0x00 };
        (i32::from_be_bytes([sign, high, mid, low]), body)
    } else {
        (0, rest)
    };
    Some(ExVideoTag {
        fourcc,
        packet_type,
        keyframe: (first >> 4) & 0x07 == 1,
        composition_time_ms,
        body,
    })
}

/// An HEVCDecoderConfigurationRecord (ISO/IEC 14496-15 8.3.3.1), borrowed:
/// the NALU length width and the VPS, SPS and PPS it carries. Truncation
/// anywhere is `None`.
pub struct HvccRecord<'a> {
    /// `lengthSizeMinusOne` (0..=3).
    pub length_size_minus_one: u8,
    pub vps: Vec<&'a [u8]>,
    pub sps: Vec<&'a [u8]>,
    pub pps: Vec<&'a [u8]>,
}

pub fn hvcc_record(record: &[u8]) -> Option<HvccRecord<'_>> {
    let (header, mut rest) = record.split_first_chunk::<23>()?;
    let [.., length_byte, arrays] = *header;
    let mut parsed = HvccRecord {
        length_size_minus_one: length_byte & 0x03,
        vps: Vec::new(),
        sps: Vec::new(),
        pps: Vec::new(),
    };
    for _ in 0..arrays {
        let (&[kind, count_high, count_low], tail) = rest.split_first_chunk::<3>()?;
        rest = tail;
        for _ in 0..u16::from_be_bytes([count_high, count_low]) {
            let nalu = take_u16_prefixed(&mut rest)?;
            match kind & 0x3F {
                32 => parsed.vps.push(nalu),
                33 => parsed.sps.push(nalu),
                34 => parsed.pps.push(nalu),
                _ => {}
            }
        }
    }
    Some(parsed)
}

#[inline]
pub fn hevc_video_for_enhanced_rtmp_with_composition_into(
    payload: &[u8],
    is_keyframe: bool,
    composition_time_ms: i32,
    out: &mut Vec<u8>,
) -> bool {
    let frame_type = if is_keyframe { 1u8 } else { 2u8 };
    let packet_type = if composition_time_ms == 0 { 3u8 } else { 1u8 };
    out.clear();
    out.push(0x80 | (frame_type << 4) | packet_type);
    out.extend_from_slice(b"hvc1");
    if composition_time_ms != 0 {
        out.extend_from_slice(&signed_be24(composition_time_ms));
    }
    h265_annexb_to_length_prefixed_into(payload, out)
}

pub fn build_hevc_enhanced_rtmp_sequence_header(annexb_data: &[u8]) -> Option<Bytes> {
    let hvcc = restream_core::build_hvcc_record(annexb_data)?;
    let mut out = Vec::with_capacity(hvcc.len().saturating_add(5));
    out.push(0x80);
    out.extend_from_slice(b"hvc1");
    out.extend_from_slice(&hvcc);
    Some(Bytes::from(out))
}
/// Annex B H.265 as 4-byte length-prefixed NAL units, without the VPS, SPS,
/// PPS and access unit delimiters; whether a picture slice was written.
pub fn h265_annexb_to_length_prefixed_into(data: &[u8], out: &mut Vec<u8>) -> bool {
    let mut has_vcl = false;
    let _ = for_each_annexb_nalu(data, |nalu| {
        let Some(nal_type) = h265_nal_type(nalu) else {
            return ControlFlow::Continue(());
        };
        if matches!(nal_type, 32..=35) {
            return ControlFlow::Continue(());
        }
        if nal_type <= 31 {
            has_vcl = true;
        }
        if let Ok(len) = u32::try_from(nalu.len()) {
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(nalu);
        }
        ControlFlow::Continue(())
    });
    has_vcl
}

pub use restream_core::{HevcProfileTierLevel, h265_nal_type, parse_hevc_profile_tier_level};
