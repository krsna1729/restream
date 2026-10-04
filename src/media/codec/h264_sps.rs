//! H.264 sequence parameter set probe (ITU-T H.264 7.3.2.1.1), shared by the
//! RTMP FLV probe and the MPEG-TS probe. There was one copy per ingest path;
//! they drifted (the RTMP one overflowed on scaling lists and computed crop
//! offsets without SubWidthC/SubHeightC, the TS one lacked four high
//! profiles), so both now call this one.
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::bits::{BitReader, rbsp};

/// What the ingest probes report from an SPS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct H264Sps {
    pub(crate) profile_idc: u8,
    pub(crate) level_idc: u8,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Frame rate from VUI timing, 0.0 when absent.
    pub(crate) fps: f64,
}

/// Profiles whose SPS carries chroma format, bit depths and scaling lists.
const HIGH_PROFILES: [u8; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];

pub(crate) fn profile_name(profile_idc: u8) -> &'static str {
    match profile_idc {
        66 => "Baseline",
        77 => "Main",
        88 => "Extended",
        100 => "High",
        110 => "High 10",
        122 => "High 4:2:2",
        244 => "High 4:4:4 Predictive",
        _ => "Unknown",
    }
}

/// `level_idc` as the familiar "major.minor" (31 → "3.1").
pub(crate) fn level_name(level_idc: u8) -> String {
    format!("{}.{}", level_idc / 10, level_idc % 10)
}

/// Parses an SPS NAL unit payload: the bytes after the one-byte NAL header,
/// emulation-prevention bytes still present.
pub(crate) fn parse_h264_sps(payload: &[u8]) -> Option<H264Sps> {
    let sps = rbsp(payload);
    let &[profile_idc, _constraints, level_idc] = sps.first_chunk::<3>()?;
    let mut reader = BitReader::new(sps.get(3..)?);
    let _seq_parameter_set_id = reader.read_ue()?;

    let chroma_format_idc = if HIGH_PROFILES.contains(&profile_idc) {
        let chroma_format_idc = reader.read_ue()?;
        if chroma_format_idc > 3 {
            return None;
        }
        if chroma_format_idc == 3 {
            reader.skip(1)?; // separate_colour_plane_flag
        }
        let bit_depth_luma_minus8 = reader.read_ue()?;
        let bit_depth_chroma_minus8 = reader.read_ue()?;
        if bit_depth_luma_minus8 > 8 || bit_depth_chroma_minus8 > 8 {
            return None;
        }
        reader.skip(1)?; // qpprime_y_zero_transform_bypass_flag
        if reader.read_bits(1)? == 1 {
            let lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for index in 0..lists {
                if reader.read_bits(1)? == 1 {
                    skip_scaling_list(&mut reader, if index < 6 { 16 } else { 64 })?;
                }
            }
        }
        chroma_format_idc
    } else {
        1
    };

    let _log2_max_frame_num_minus4 = reader.read_ue()?;
    match reader.read_ue()? {
        0 => {
            let _log2_max_pic_order_cnt_lsb_minus4 = reader.read_ue()?;
        }
        1 => {
            reader.skip(1)?; // delta_pic_order_always_zero_flag
            reader.read_se()?; // offset_for_non_ref_pic
            reader.read_se()?; // offset_for_top_to_bottom_field
            // num_ref_frames_in_pic_order_cnt_cycle is 0..=255 (7.4.2.1.1).
            let cycle = reader.read_ue()?;
            if cycle > 255 {
                return None;
            }
            for _ in 0..cycle {
                reader.read_se()?;
            }
        }
        2 => {}
        _ => return None,
    }
    let _max_num_ref_frames = reader.read_ue()?;
    reader.skip(1)?; // gaps_in_frame_num_value_allowed_flag
    let width_in_mbs_minus1 = reader.read_ue()?;
    let height_in_map_units_minus1 = reader.read_ue()?;
    let frame_mbs_only = reader.read_bits(1)?;
    if frame_mbs_only == 0 {
        reader.skip(1)?; // mb_adaptive_frame_field_flag
    }
    reader.skip(1)?; // direct_8x8_inference_flag
    let (crop_left, crop_right, crop_top, crop_bottom) = if reader.read_bits(1)? == 1 {
        (
            reader.read_ue()?,
            reader.read_ue()?,
            reader.read_ue()?,
            reader.read_ue()?,
        )
    } else {
        (0, 0, 0, 0)
    };

    // Crop units (Table 6-1 and 7.4.2.1.1): SubWidthC across, SubHeightC
    // times (2 - frame_mbs_only) down; monochrome and 4:4:4 use 1.
    let sub_width = if matches!(chroma_format_idc, 1 | 2) {
        2
    } else {
        1
    };
    let sub_height: u64 = if chroma_format_idc == 1 { 2 } else { 1 };
    let frame_height_factor = 2u64.checked_sub(u64::from(frame_mbs_only))?;
    let width = cropped(
        u64::from(width_in_mbs_minus1)
            .checked_add(1)?
            .checked_mul(16)?,
        crop_left,
        crop_right,
        sub_width,
    )?;
    let height = cropped(
        u64::from(height_in_map_units_minus1)
            .checked_add(1)?
            .checked_mul(frame_height_factor)?
            .checked_mul(16)?,
        crop_top,
        crop_bottom,
        sub_height.checked_mul(frame_height_factor)?,
    )?;

    Some(H264Sps {
        profile_idc,
        level_idc,
        width,
        height,
        // A truncated SPS fails closed: the VUI flag, and the VUI when
        // present, must be complete.
        fps: if reader.read_bits(1)? == 1 {
            vui_frame_rate(&mut reader)?
        } else {
            0.0
        },
    })
}

/// `size - (first + second) * unit`, non-zero and within `u32`.
fn cropped(size: u64, first: u32, second: u32, unit: u64) -> Option<u32> {
    let crop = u64::from(first)
        .checked_add(u64::from(second))?
        .checked_mul(unit)?;
    u32::try_from(size.checked_sub(crop)?)
        .ok()
        .filter(|value| *value > 0)
}

fn skip_scaling_list(reader: &mut BitReader, size: usize) -> Option<()> {
    let mut last_scale = 8i32;
    let mut next_scale = 8i32;
    for _ in 0..size {
        if next_scale != 0 {
            // delta_scale is -128..=127 (7.4.2.1.1.1); anything else is not
            // a valid SPS.
            let delta = reader.read_se()?;
            if !(-128..=127).contains(&delta) {
                return None;
            }
            next_scale = last_scale.checked_add(delta)?.rem_euclid(256);
        }
        if next_scale != 0 {
            last_scale = next_scale;
        }
    }
    Some(())
}

/// Frame rate from the VUI timing information (0.0 when absent); `None`
/// when the VUI is truncated.
fn vui_frame_rate(reader: &mut BitReader) -> Option<f64> {
    if reader.read_bits(1)? == 1 && reader.read_bits(8)? == 255 {
        reader.skip(32)?; // sar_width, sar_height
    }
    if reader.read_bits(1)? == 1 {
        reader.skip(1)?; // overscan_appropriate_flag
    }
    if reader.read_bits(1)? == 1 {
        reader.skip(4)?; // video_format, video_full_range_flag
        if reader.read_bits(1)? == 1 {
            reader.skip(24)?; // colour primaries, transfer, matrix
        }
    }
    if reader.read_bits(1)? == 1 {
        reader.read_ue()?; // chroma_sample_loc_type_top_field
        reader.read_ue()?; // chroma_sample_loc_type_bottom_field
    }
    if reader.read_bits(1)? == 0 {
        return Some(0.0);
    }
    let num_units_in_tick = reader.read_bits(32)?;
    let time_scale = reader.read_bits(32)?;
    Some(if num_units_in_tick > 0 && time_scale > 0 {
        f64::from(time_scale) / (2.0 * f64::from(num_units_in_tick))
    } else {
        0.0
    })
}
