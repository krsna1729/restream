#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
use bytes::Bytes;

use super::bits::{BitReader, rbsp};
use super::video::{signed_be24, split_annexb_nalus};

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

fn h265_annexb_to_length_prefixed_into(data: &[u8], out: &mut Vec<u8>) -> bool {
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

struct HevcProfileTierLevel {
    profile_space: u8,
    tier_flag: bool,
    profile_idc: u8,
    profile_compatibility_flags: u32,
    constraint_indicator_flags: u64,
    level_idc: u8,
    max_sub_layers_minus1: u8,
    temporal_id_nested: bool,
    chroma_format_idc: u8,
    bit_depth_luma_minus8: u8,
    bit_depth_chroma_minus8: u8,
}

fn parse_hevc_profile_tier_level(sps: &[u8]) -> Option<HevcProfileTierLevel> {
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
    let _pic_width_in_luma_samples = reader.read_ue()?;
    let _pic_height_in_luma_samples = reader.read_ue()?;
    if reader.read_bits(1)? == 1 {
        let _conf_win_left_offset = reader.read_ue()?;
        let _conf_win_right_offset = reader.read_ue()?;
        let _conf_win_top_offset = reader.read_ue()?;
        let _conf_win_bottom_offset = reader.read_ue()?;
    }
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
