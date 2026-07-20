use super::bit_reader::BitReader;
use super::{for_each_nal_raw, remove_emulation_prevention};
use crate::media::metadata::VideoMeta;

#[inline]
pub(in crate::media::mpegts) fn is_keyframe(payload: &[u8]) -> bool {
    for_each_nal(payload, |nal_type, _nal_data| {
        // NAL type 5 = IDR slice
        if nal_type == 5 {
            return true;
        }
        false
    })
}

pub(in crate::media::mpegts) fn find_sps(payload: &[u8]) -> Option<Vec<u8>> {
    let mut result = None;
    for_each_nal_raw(payload, |nal_data| {
        if !nal_data.is_empty() && (nal_data[0] & 0x1F) == 7 {
            result = Some(nal_data[1..].to_vec());
            return true;
        }
        false
    });
    result
}

fn for_each_nal<F>(data: &[u8], mut callback: F) -> bool
where
    F: FnMut(u8, &[u8]) -> bool,
{
    for_each_nal_raw(data, |nal_data| {
        if nal_data.is_empty() {
            return false;
        }
        callback(nal_data[0] & 0x1f, &nal_data[1..])
    })
}

pub(super) fn parse_sps(raw_sps: &[u8], meta: &mut VideoMeta) -> Option<()> {
    if raw_sps.len() < 4 {
        return None;
    }

    // Remove emulation prevention bytes (0x00 0x00 0x03 → 0x00 0x00)
    let sps = remove_emulation_prevention(raw_sps);

    let profile_idc = sps[0];
    let level_idc = sps[2];

    meta.profile = Some(
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
        .to_string(),
    );
    meta.level = Some(format!("{}.{}", level_idc / 10, level_idc % 10));

    // Parse SPS via exp-golomb for resolution
    let mut reader = BitReader::new(&sps[3..]);
    let _seq_parameter_set_id = reader.read_ue()?;

    let chroma_format_idc;
    if profile_idc == 100
        || profile_idc == 110
        || profile_idc == 122
        || profile_idc == 244
        || profile_idc == 44
        || profile_idc == 83
        || profile_idc == 86
        || profile_idc == 118
        || profile_idc == 128
    {
        chroma_format_idc = reader.read_ue()?;
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
        let scaling_matrix_present = reader.read_bits(1)?;
        if scaling_matrix_present == 1 {
            let count = if chroma_format_idc != 3 { 8 } else { 12 };
            for index in 0..count {
                let present = reader.read_bits(1)?;
                if present == 1 {
                    let size = if index < 6 { 16 } else { 64 };
                    skip_scaling_list(&mut reader, size)?;
                }
            }
        }
    } else {
        chroma_format_idc = 1;
    }

    let _log2_max_frame_num = reader.read_ue()?; // + 4
    let pic_order_cnt_type = reader.read_ue()?;
    if pic_order_cnt_type == 0 {
        let _log2_max_pic_order_cnt_lsb = reader.read_ue()?; // + 4
    } else if pic_order_cnt_type == 1 {
        reader.skip(1)?; // delta_pic_order_always_zero_flag
        reader.read_se()?; // offset_for_non_ref_pic
        reader.read_se()?; // offset_for_top_to_bottom_field
        let num = reader.read_ue()?;
        if num > 255 {
            return None;
        }
        for _ in 0..num {
            reader.read_se()?;
        }
    } else if pic_order_cnt_type > 2 {
        return None;
    }

    let _max_num_ref_frames = reader.read_ue()?;
    reader.skip(1)?; // gaps_in_frame_num_allowed
    let pic_width = reader.read_ue()?;
    let pic_height = reader.read_ue()?;
    let frame_mbs_only = reader.read_bits(1)?;

    let sub_wc = if chroma_format_idc == 1 || chroma_format_idc == 2 {
        2
    } else {
        1
    };
    let sub_hc = if chroma_format_idc == 1 { 2 } else { 1 };

    let mut crop_left = 0u32;
    let mut crop_right = 0u32;
    let mut crop_top = 0u32;
    let mut crop_bottom = 0u32;

    if frame_mbs_only == 0 {
        reader.skip(1)?; // mb_adaptive_frame_field_flag
    }
    reader.skip(1)?; // direct_8x8_inference_flag

    let frame_cropping = reader.read_bits(1)?;
    if frame_cropping == 1 {
        crop_left = reader.read_ue()?;
        crop_right = reader.read_ue()?;
        crop_top = reader.read_ue()?;
        crop_bottom = reader.read_ue()?;
    }

    let frame_factor = 2u64.checked_sub(frame_mbs_only as u64)?;
    let width_base = (pic_width as u64).checked_add(1)?.checked_mul(16)?;
    let width_crop = (crop_left as u64)
        .checked_add(crop_right as u64)?
        .checked_mul(sub_wc)?;
    let height_base = frame_factor
        .checked_mul((pic_height as u64).checked_add(1)?)?
        .checked_mul(16)?;
    let height_crop = (crop_top as u64)
        .checked_add(crop_bottom as u64)?
        .checked_mul(sub_hc)?
        .checked_mul(frame_factor)?;
    let width = u32::try_from(width_base.checked_sub(width_crop)?).ok()?;
    let height = u32::try_from(height_base.checked_sub(height_crop)?).ok()?;
    if width == 0 || height == 0 {
        return None;
    }

    meta.width = width;
    meta.height = height;

    // VUI for frame rate
    let vui_present = reader.read_bits(1)?;
    if vui_present == 1 {
        let aspect_ratio_present = reader.read_bits(1)?;
        if aspect_ratio_present == 1 {
            let sar_idx = reader.read_bits(8)?;
            if sar_idx == 255 {
                reader.skip(32)?; // sar_width + sar_height
            }
        }
        let overscan_present = reader.read_bits(1)?;
        if overscan_present == 1 {
            reader.skip(1)?;
        }
        let video_signal_present = reader.read_bits(1)?;
        if video_signal_present == 1 {
            reader.skip(3)?; // video_format
            reader.skip(1)?; // video_full_range
            let colour_desc_present = reader.read_bits(1)?;
            if colour_desc_present == 1 {
                reader.skip(24)?; // primaries + transfer + matrix
            }
        }
        let chroma_loc_present = reader.read_bits(1)?;
        if chroma_loc_present == 1 {
            reader.read_ue()?;
            reader.read_ue()?;
        }
        let timing_info_present = reader.read_bits(1)?;
        if timing_info_present == 1 {
            let num_units_in_tick = reader.read_bits(32)?;
            let time_scale = reader.read_bits(32)?;
            if num_units_in_tick > 0 && time_scale > 0 {
                meta.fps = time_scale as f64 / (2.0 * num_units_in_tick as f64);
            }
        }
    }

    Some(())
}

fn skip_scaling_list(reader: &mut BitReader, size: usize) -> Option<()> {
    let mut last_scale = 8i64;
    let mut next_scale = 8i64;
    for _ in 0..size {
        if next_scale != 0 {
            let delta = i64::from(reader.read_se()?);
            next_scale = (last_scale + delta + 256) % 256;
        }
        last_scale = if next_scale == 0 {
            last_scale
        } else {
            next_scale
        };
    }
    Some(())
}
