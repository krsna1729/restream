use bytes::Bytes;

use super::video::{split_annexb_nalus, write_signed_be24};

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
        out.extend_from_slice(&[0, 0, 0]);
        write_signed_be24(composition_time_ms, &mut out[5..8]);
    }
    h265_annexb_to_length_prefixed_into(payload, out)
}

pub fn build_hevc_enhanced_rtmp_sequence_header(annexb_data: &[u8]) -> Option<Bytes> {
    let nalus = split_annexb_nalus(annexb_data);
    let vps = first_h265_nalu_of_type(&nalus, 32)?;
    let sps = first_h265_nalu_of_type(&nalus, 33)?;
    let pps = first_h265_nalu_of_type(&nalus, 34)?;
    let profile = parse_hevc_profile_tier_level(sps)?;

    let mut hvcc = Vec::with_capacity(64 + vps.len() + sps.len() + pps.len());
    hvcc.push(1);
    hvcc.push(
        (profile.profile_space << 6) | (u8::from(profile.tier_flag) << 5) | profile.profile_idc,
    );
    hvcc.extend_from_slice(&profile.profile_compatibility_flags.to_be_bytes());
    hvcc.extend_from_slice(&profile.constraint_indicator_flags.to_be_bytes()[2..8]);
    hvcc.push(profile.level_idc);
    hvcc.extend_from_slice(&[0xF0, 0x00, 0xFC]);
    hvcc.push(0xFC | (profile.chroma_format_idc & 0x03));
    hvcc.push(0xF8 | (profile.bit_depth_luma_minus8 & 0x07));
    hvcc.push(0xF8 | (profile.bit_depth_chroma_minus8 & 0x07));
    hvcc.extend_from_slice(&[0x00, 0x00]);
    let temporal_layers = (profile.max_sub_layers_minus1 + 1).min(7);
    hvcc.push((temporal_layers << 3) | (u8::from(profile.temporal_id_nested) << 2) | 3);
    hvcc.push(3);
    push_hvcc_array(&mut hvcc, 32, vps)?;
    push_hvcc_array(&mut hvcc, 33, sps)?;
    push_hvcc_array(&mut hvcc, 34, pps)?;

    let mut out = Vec::with_capacity(hvcc.len() + 5);
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
    (nalu.len() >= 2).then_some((nalu[0] >> 1) & 0x3F)
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
    if sps.len() < 3 {
        return None;
    }
    let rbsp = remove_emulation_prevention_bytes(&sps[2..]);
    let mut reader = BitReader::new(&rbsp);
    let _sps_video_parameter_set_id = reader.read_bits(4)?;
    let max_sub_layers_minus1 = u8::try_from(reader.read_bits(3)?).ok()?;
    let temporal_id_nested = reader.read_bits(1)? == 1;
    let profile_space = u8::try_from(reader.read_bits(2)?).ok()?;
    let tier_flag = reader.read_bits(1)? == 1;
    let profile_idc = u8::try_from(reader.read_bits(5)?).ok()?;
    let profile_compatibility_flags = u32::try_from(reader.read_bits(32)?).ok()?;
    let constraint_indicator_flags = reader.read_bits(48)?;
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
    let mut profile_present = [false; 8];
    let mut level_present = [false; 8];
    for i in 0..usize::from(max_sub_layers_minus1) {
        profile_present[i] = reader.read_bits(1)? == 1;
        level_present[i] = reader.read_bits(1)? == 1;
    }
    if max_sub_layers_minus1 > 0 {
        for _ in usize::from(max_sub_layers_minus1)..8 {
            let _reserved_zero_2bits = reader.read_bits(2)?;
        }
    }
    for i in 0..usize::from(max_sub_layers_minus1) {
        if profile_present[i] {
            let _sub_layer_profile_space = reader.read_bits(2)?;
            let _sub_layer_tier_flag = reader.read_bits(1)?;
            let _sub_layer_profile_idc = reader.read_bits(5)?;
            let _sub_layer_profile_compatibility_flags = reader.read_bits(32)?;
            let _sub_layer_progressive_source_flag = reader.read_bits(1)?;
            let _sub_layer_interlaced_source_flag = reader.read_bits(1)?;
            let _sub_layer_non_packed_constraint_flag = reader.read_bits(1)?;
            let _sub_layer_frame_only_constraint_flag = reader.read_bits(1)?;
            let _sub_layer_reserved_zero_44bits = reader.read_bits(44)?;
        }
        if level_present[i] {
            let _sub_layer_level_idc = reader.read_bits(8)?;
        }
    }
    Some(())
}

fn remove_emulation_prevention_bytes(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 3 {
            out.push(0);
            out.push(0);
            i += 3;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl BitReader<'_> {
    fn new(data: &[u8]) -> BitReader<'_> {
        BitReader { data, bit_pos: 0 }
    }

    fn read_bits(&mut self, count: usize) -> Option<u64> {
        if count > 64 || self.bit_pos + count > self.data.len().checked_mul(8)? {
            return None;
        }
        let mut value = 0u64;
        for _ in 0..count {
            let byte = self.data[self.bit_pos / 8];
            let shift = 7usize.saturating_sub(self.bit_pos % 8);
            value = (value << 1) | u64::from((byte >> shift) & 1);
            self.bit_pos += 1;
        }
        Some(value)
    }

    fn read_ue(&mut self) -> Option<u64> {
        let mut leading_zeros = 0usize;
        while self.read_bits(1)? == 0 {
            leading_zeros += 1;
            if leading_zeros > 31 {
                return None;
            }
        }
        if leading_zeros == 0 {
            return Some(0);
        }
        let suffix = self.read_bits(leading_zeros)?;
        (1u64.checked_shl(u32::try_from(leading_zeros).ok()?)?)
            .checked_sub(1)?
            .checked_add(suffix)
    }
}
