#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
use bytes::Bytes;

use super::bits::{BitReader, rbsp};
use super::video::{signed_be24, split_annexb_nalus, take_u16_prefixed};

/// Enhanced RTMP (E-RTMP v1) video packet types.
pub(crate) const EX_SEQUENCE_START: u8 = 0;
pub(crate) const EX_CODED_FRAMES: u8 = 1;
pub(crate) const EX_CODED_FRAMES_X: u8 = 3;

/// An enhanced RTMP video tag (`IsExHeader` set), borrowed. Only the H.264
/// and H.265 FourCCs (`avc1`, `hvc1`) carry the 24-bit composition time on
/// coded frames, so other FourCCs are not read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExVideoTag<'a> {
    pub(crate) fourcc: [u8; 4],
    pub(crate) packet_type: u8,
    pub(crate) keyframe: bool,
    pub(crate) composition_time_ms: i32,
    /// The configuration record (sequence start) or the length-prefixed
    /// NAL units (coded frames).
    pub(crate) body: &'a [u8],
}

impl ExVideoTag<'_> {
    pub(crate) fn is_hevc(&self) -> bool {
        &self.fourcc == b"hvc1"
    }
}

/// Reads an enhanced RTMP video tag header; `None` for a classic FLV video
/// tag, another codec, or a truncated header.
pub(crate) fn ex_video_tag(payload: &[u8]) -> Option<ExVideoTag<'_>> {
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
pub(crate) struct HvccRecord<'a> {
    /// `lengthSizeMinusOne` (0..=3).
    pub(crate) length_size_minus_one: u8,
    pub(crate) vps: Vec<&'a [u8]>,
    pub(crate) sps: Vec<&'a [u8]>,
    pub(crate) pps: Vec<&'a [u8]>,
}

pub(crate) fn hvcc_record(record: &[u8]) -> Option<HvccRecord<'_>> {
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
    let nalus = split_annexb_nalus(annexb_data);
    let vps = first_h265_nalu_of_type(&nalus, 32)?;
    let sps = first_h265_nalu_of_type(&nalus, 33)?;
    let pps = first_h265_nalu_of_type(&nalus, 34)?;
    let profile = parse_hevc_profile_tier_level(sps)?;

    let nalu_bytes = vps
        .len()
        .saturating_add(sps.len())
        .saturating_add(pps.len());
    let mut hvcc = Vec::with_capacity(nalu_bytes.saturating_add(64));
    hvcc.push(1);
    hvcc.push(
        (profile.profile_space << 6) | (u8::from(profile.tier_flag) << 5) | profile.profile_idc,
    );
    hvcc.extend_from_slice(&profile.profile_compatibility_flags.to_be_bytes());
    let [_, _, constraint_indicator_flags @ ..] = profile.constraint_indicator_flags.to_be_bytes();
    hvcc.extend_from_slice(&constraint_indicator_flags);
    hvcc.push(profile.level_idc);
    hvcc.extend_from_slice(&[0xF0, 0x00, 0xFC]);
    hvcc.push(0xFC | (profile.chroma_format_idc & 0x03));
    hvcc.push(0xF8 | (profile.bit_depth_luma_minus8 & 0x07));
    hvcc.push(0xF8 | (profile.bit_depth_chroma_minus8 & 0x07));
    hvcc.extend_from_slice(&[0x00, 0x00]);
    let temporal_layers = profile.max_sub_layers_minus1.saturating_add(1).min(7);
    hvcc.push((temporal_layers << 3) | (u8::from(profile.temporal_id_nested) << 2) | 3);
    hvcc.push(3);
    push_hvcc_array(&mut hvcc, 32, vps)?;
    push_hvcc_array(&mut hvcc, 33, sps)?;
    push_hvcc_array(&mut hvcc, 34, pps)?;

    let mut out = Vec::with_capacity(hvcc.len().saturating_add(5));
    out.push(0x80);
    out.extend_from_slice(b"hvc1");
    out.extend_from_slice(&hvcc);
    Some(Bytes::from(out))
}

fn first_h265_nalu_of_type<'a>(nalus: &'a [&'a [u8]], expected_type: u8) -> Option<&'a [u8]> {
    nalus
        .iter()
        .copied()
        .find(|nalu| h265_nal_type(nalu) == Some(expected_type))
}

fn push_hvcc_array(out: &mut Vec<u8>, nal_type: u8, nalu: &[u8]) -> Option<()> {
    let len = u16::try_from(nalu.len()).ok()?;
    out.push(0x80 | nal_type);
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(nalu);
    Some(())
}

/// Annex B H.265 as 4-byte length-prefixed NAL units, without the VPS, SPS,
/// PPS and access unit delimiters; whether a picture slice was written.
pub(crate) fn h265_annexb_to_length_prefixed_into(data: &[u8], out: &mut Vec<u8>) -> bool {
    let nalus = split_annexb_nalus(data);
    let mut has_vcl = false;
    for nalu in &nalus {
        let Some(nal_type) = h265_nal_type(nalu) else {
            continue;
        };
        if matches!(nal_type, 32..=35) {
            continue;
        }
        if nal_type <= 31 {
            has_vcl = true;
        }
        let Ok(len) = u32::try_from(nalu.len()) else {
            continue;
        };
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nalu);
    }
    has_vcl
}

fn h265_nal_type(nalu: &[u8]) -> Option<u8> {
    match nalu {
        [first, _, ..] => Some((first >> 1) & 0x3F),
        _ => None,
    }
}

pub(crate) struct HevcProfileTierLevel {
    pub(crate) profile_space: u8,
    pub(crate) tier_flag: bool,
    pub(crate) profile_idc: u8,
    pub(crate) profile_compatibility_flags: u32,
    /// The 48 general constraint indicator flags, in the low 48 bits.
    pub(crate) constraint_indicator_flags: u64,
    pub(crate) level_idc: u8,
    /// The picture size after the conformance window.
    pub(crate) width: u32,
    pub(crate) height: u32,
    max_sub_layers_minus1: u8,
    temporal_id_nested: bool,
    chroma_format_idc: u8,
    bit_depth_luma_minus8: u8,
    bit_depth_chroma_minus8: u8,
}

/// The general profile/tier/level and format fields of an H.265 SPS NAL
/// unit (with its two-byte header).
pub(crate) fn parse_hevc_profile_tier_level(sps: &[u8]) -> Option<HevcProfileTierLevel> {
    // Two NAL header bytes, then at least one byte of SPS.
    let payload = sps.get(2..).filter(|payload| !payload.is_empty())?;
    let rbsp = rbsp(payload);
    let mut reader = BitReader::new(&rbsp);
    let _sps_video_parameter_set_id = reader.read_bits(4)?;
    let max_sub_layers_minus1 = u8::try_from(reader.read_bits(3)?).ok()?;
    let temporal_id_nested = reader.read_bits(1)? == 1;
    let profile_space = u8::try_from(reader.read_bits(2)?).ok()?;
    let tier_flag = reader.read_bits(1)? == 1;
    let profile_idc = u8::try_from(reader.read_bits(5)?).ok()?;
    let profile_compatibility_flags = reader.read_bits(32)?;
    let constraint_indicator_flags =
        (u64::from(reader.read_bits(16)?) << 32) | u64::from(reader.read_bits(32)?);
    let level_idc = u8::try_from(reader.read_bits(8)?).ok()?;
    skip_hevc_sub_layer_profile_tier_level(&mut reader, max_sub_layers_minus1)?;
    let _sps_seq_parameter_set_id = reader.read_ue()?;
    let chroma_format_idc = u8::try_from(reader.read_ue()?).ok()?;
    if chroma_format_idc > 3 {
        return None;
    }
    if chroma_format_idc == 3 {
        let _separate_colour_plane_flag = reader.read_bits(1)?;
    }
    let pic_width = reader.read_ue()?;
    let pic_height = reader.read_ue()?;
    let (mut crop_x, mut crop_y) = (0u32, 0u32);
    if reader.read_bits(1)? == 1 {
        let left = reader.read_ue()?;
        let right = reader.read_ue()?;
        let top = reader.read_ue()?;
        let bottom = reader.read_ue()?;
        crop_x = left.saturating_add(right);
        crop_y = top.saturating_add(bottom);
    }
    // Window offsets count chroma samples (H.265 7.4.3.2.1).
    let sub_width = if matches!(chroma_format_idc, 1 | 2) {
        2
    } else {
        1
    };
    let sub_height = if chroma_format_idc == 1 { 2 } else { 1 };
    let width = pic_width.saturating_sub(crop_x.saturating_mul(sub_width));
    let height = pic_height.saturating_sub(crop_y.saturating_mul(sub_height));
    let bit_depth_luma_minus8 = u8::try_from(reader.read_ue()?).ok()?;
    let bit_depth_chroma_minus8 = u8::try_from(reader.read_ue()?).ok()?;
    if bit_depth_luma_minus8 > 7 || bit_depth_chroma_minus8 > 7 {
        return None;
    }

    Some(HevcProfileTierLevel {
        profile_space,
        tier_flag,
        profile_idc,
        profile_compatibility_flags,
        constraint_indicator_flags,
        level_idc,
        width,
        height,
        max_sub_layers_minus1,
        temporal_id_nested,
        chroma_format_idc,
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
    })
}

fn skip_hevc_sub_layer_profile_tier_level(
    reader: &mut BitReader<'_>,
    max_sub_layers_minus1: u8,
) -> Option<()> {
    // max_sub_layers_minus1 is 3 bits, so at most 7 sub-layers.
    let sub_layers = usize::from(max_sub_layers_minus1);
    let mut present = [(false, false); 7];
    for (profile_present, level_present) in present.iter_mut().take(sub_layers) {
        *profile_present = reader.read_bits(1)? == 1;
        *level_present = reader.read_bits(1)? == 1;
    }
    if sub_layers > 0 {
        for _ in sub_layers..8 {
            let _reserved_zero_2bits = reader.read_bits(2)?;
        }
    }
    for &(profile_present, level_present) in present.iter().take(sub_layers) {
        if profile_present {
            let _sub_layer_profile_space = reader.read_bits(2)?;
            let _sub_layer_tier_flag = reader.read_bits(1)?;
            let _sub_layer_profile_idc = reader.read_bits(5)?;
            let _sub_layer_profile_compatibility_flags = reader.read_bits(32)?;
            let _sub_layer_progressive_source_flag = reader.read_bits(1)?;
            let _sub_layer_interlaced_source_flag = reader.read_bits(1)?;
            let _sub_layer_non_packed_constraint_flag = reader.read_bits(1)?;
            let _sub_layer_frame_only_constraint_flag = reader.read_bits(1)?;
            reader.skip(44)?; // sub_layer_reserved_zero_44bits
        }
        if level_present {
            let _sub_layer_level_idc = reader.read_bits(8)?;
        }
    }
    Some(())
}
