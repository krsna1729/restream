use super::bit_reader::BitReader;
use super::for_each_nal_raw;
use crate::media::metadata::VideoMeta;

#[inline]
pub(in crate::media::mpegts) fn is_keyframe(payload: &[u8]) -> bool {
    for_each_nal(payload, |nal_type, _nal_data| {
        // H.265 IRAP NAL types: BLA_W_LP(16)..RSV_IRAP_VCL23(23)
        (16..=23).contains(&nal_type)
    })
}

pub(in crate::media::mpegts) fn find_sps(payload: &[u8]) -> Option<Vec<u8>> {
    let mut result = None;
    for_each_nal_raw(payload, |nal_data| {
        if nal_data.len() >= 2 && ((nal_data[0] >> 1) & 0x3F) == 33 {
            result = Some(nal_data[2..].to_vec());
            return true;
        }
        false
    });
    result
}

/// Iterate over Annex B NAL units with H.265 NAL type extraction.
fn for_each_nal<F>(data: &[u8], mut callback: F) -> bool
where
    F: FnMut(u8, &[u8]) -> bool,
{
    for_each_nal_raw(data, |nal_data| {
        if nal_data.is_empty() {
            return false;
        }
        // H.265 NAL header: forbidden(1) + nal_unit_type(6) + nuh_layer_id(6) + nuh_temporal_id_plus1(3)
        let nal_type = (nal_data[0] >> 1) & 0x3F;
        // Skip the 2-byte NAL header for payload
        let payload_start = if nal_data.len() >= 2 {
            2
        } else {
            nal_data.len()
        };
        callback(nal_type, &nal_data[payload_start..])
    })
}

pub(in crate::media::mpegts) fn parse_sps(sps: &[u8], meta: &mut VideoMeta) -> Option<()> {
    const MAX_SHORT_TERM_REF_PIC_SETS: u32 = 64;
    const MAX_DELTA_POCS: u32 = 64;
    const MAX_LONG_TERM_REF_PICS: u32 = 32;

    if sps.len() < 2 {
        return None;
    }

    let mut reader = BitReader::new(sps);
    let _vps_id = reader.read_bits(4)?;
    let max_sub_layers = reader.read_bits(3)?.checked_add(1)?;
    reader.skip(1)?; // temporal_id_nesting

    // profile_tier_level
    reader.skip(2)?; // general_profile_space
    reader.skip(1)?; // general_tier_flag
    let general_profile_idc = reader.read_bits(5)?;
    reader.skip(32)?; // general_profile_compatibility_flags
    reader.skip(48)?; // general_constraint_indicator_flags
    let general_level_idc = reader.read_bits(8)?;

    meta.profile = Some(
        match general_profile_idc {
            1 => "Main",
            2 => "Main 10",
            3 => "Main Still Picture",
            _ => "Unknown",
        }
        .to_string(),
    );
    meta.level = Some(format!(
        "{}.{}",
        general_level_idc / 30,
        (general_level_idc % 30) / 3
    ));

    // Skip sub-layer profile info
    if max_sub_layers > 1 {
        let mut sub_layer_profile_present = [false; 8];
        let mut sub_layer_level_present = [false; 8];
        for i in 0..(max_sub_layers - 1) as usize {
            sub_layer_profile_present[i] = reader.read_bits(1)? == 1;
            sub_layer_level_present[i] = reader.read_bits(1)? == 1;
        }
        if max_sub_layers < 8 {
            reader.skip((8 - max_sub_layers) * 2)?;
        }
        for i in 0..(max_sub_layers - 1) as usize {
            if sub_layer_profile_present[i] {
                reader.skip(88)?; // profile info
            }
            if sub_layer_level_present[i] {
                reader.skip(8)?;
            }
        }
    }

    let _sps_id = reader.read_ue()?;
    let chroma_format_idc = reader.read_ue()?;
    if chroma_format_idc > 3 {
        return None;
    }
    if chroma_format_idc == 3 {
        reader.skip(1)?; // separate_colour_plane_flag
    }

    let width = reader.read_ue()?;
    let height = reader.read_ue()?;
    if width == 0 || height == 0 {
        return None;
    }

    let conformance_window = reader.read_bits(1)?;
    if conformance_window == 1 {
        let sub_wc = if chroma_format_idc == 1 || chroma_format_idc == 2 {
            2
        } else {
            1
        };
        let sub_hc = if chroma_format_idc == 1 { 2 } else { 1 };
        let crop_horizontal = reader
            .read_ue()?
            .checked_add(reader.read_ue()?)?
            .checked_mul(sub_wc)?;
        let crop_vertical = reader
            .read_ue()?
            .checked_add(reader.read_ue()?)?
            .checked_mul(sub_hc)?;
        meta.width = width.checked_sub(crop_horizontal)?;
        meta.height = height.checked_sub(crop_vertical)?;
    } else {
        meta.width = width;
        meta.height = height;
    }
    if meta.width == 0 || meta.height == 0 {
        return None;
    }

    let bit_depth_luma = reader.read_ue()?.checked_add(8)?;
    let bit_depth_chroma = reader.read_ue()?.checked_add(8)?;
    if bit_depth_luma > 16 || bit_depth_chroma > 16 {
        return None;
    }
    let log2_max_pic_order_cnt = reader.read_ue()?.checked_add(4)?;
    if log2_max_pic_order_cnt > 16 {
        return None;
    }

    let sub_layer_ordering_info_present = reader.read_bits(1)?;
    let start = if sub_layer_ordering_info_present == 1 {
        0
    } else {
        max_sub_layers - 1
    };
    for _ in start..max_sub_layers {
        reader.read_ue()?; // max_dec_pic_buffering
        reader.read_ue()?; // max_num_reorder_pics
        reader.read_ue()?; // max_latency_increase
    }

    let _log2_min_luma_coding_block_size = reader.read_ue()?.checked_add(3)?;
    let _log2_diff_max_min_luma_coding_block_size = reader.read_ue()?;
    let _log2_min_luma_transform_block_size = reader.read_ue()?.checked_add(2)?;
    let _log2_diff_max_min_luma_transform_block_size = reader.read_ue()?;
    let _max_transform_hierarchy_depth_inter = reader.read_ue()?;
    let _max_transform_hierarchy_depth_intra = reader.read_ue()?;

    let scaling_list_enabled = reader.read_bits(1)?;
    if scaling_list_enabled == 1 {
        let scaling_list_data_present = reader.read_bits(1)?;
        if scaling_list_data_present == 1 {
            skip_scaling_list_data(&mut reader)?;
        }
    }

    reader.skip(1)?; // amp_enabled_flag
    reader.skip(1)?; // sample_adaptive_offset_enabled_flag

    let pcm_enabled = reader.read_bits(1)?;
    if pcm_enabled == 1 {
        reader.skip(4)?; // pcm_sample_bit_depth_luma_minus1
        reader.skip(4)?; // pcm_sample_bit_depth_chroma_minus1
        reader.read_ue()?; // log2_min_pcm_luma_coding_block_size_minus3
        reader.read_ue()?; // log2_diff_max_min_pcm_luma_coding_block_size
        reader.skip(1)?; // pcm_loop_filter_disabled_flag
    }

    let num_short_term_rps = reader.read_ue()?;
    if num_short_term_rps > MAX_SHORT_TERM_REF_PIC_SETS {
        return None;
    }
    let mut num_delta_pocs = Vec::with_capacity(num_short_term_rps as usize);
    for i in 0..num_short_term_rps {
        let inter_pred = if i > 0 { reader.read_bits(1)? } else { 0 };
        if inter_pred == 1 {
            // delta_idx_minus1 is not present in SPS context (stRpsIdx < num_short_term_rps)
            reader.skip(1)?; // delta_rps_sign
            reader.read_ue()?; // abs_delta_rps_minus1
            // RefRpsIdx = i - 1 (delta_idx_minus1 defaults to 0 in SPS)
            let count = num_delta_pocs[(i - 1) as usize];
            let mut this_count = 0u32;
            for _ in 0..=count {
                let used = reader.read_bits(1)?;
                if used == 0 {
                    let use_delta = reader.read_bits(1)?;
                    if use_delta == 1 {
                        this_count = this_count.checked_add(1)?;
                    }
                } else {
                    this_count = this_count.checked_add(1)?;
                }
            }
            if this_count > MAX_DELTA_POCS {
                return None;
            }
            num_delta_pocs.push(this_count);
        } else {
            let num_negative = reader.read_ue()?;
            let num_positive = reader.read_ue()?;
            let total = num_negative.checked_add(num_positive)?;
            if total > MAX_DELTA_POCS {
                return None;
            }
            for _ in 0..num_negative {
                reader.read_ue()?; // delta_poc_s0_minus1
                reader.skip(1)?; // used_by_curr_pic_s0_flag
            }
            for _ in 0..num_positive {
                reader.read_ue()?; // delta_poc_s1_minus1
                reader.skip(1)?; // used_by_curr_pic_s1_flag
            }
            num_delta_pocs.push(total);
        }
    }

    let long_term_ref_pics_present = reader.read_bits(1)?;
    if long_term_ref_pics_present == 1 {
        let num_long_term_ref_pics = reader.read_ue()?;
        if num_long_term_ref_pics > MAX_LONG_TERM_REF_PICS {
            return None;
        }
        for _ in 0..num_long_term_ref_pics {
            reader.read_bits(log2_max_pic_order_cnt)?; // lt_ref_pic_poc_lsb
            reader.skip(1)?; // used_by_curr_pic_lt_flag
        }
    }

    reader.skip(1)?; // sps_temporal_mvp_enabled_flag
    reader.skip(1)?; // strong_intra_smoothing_enabled_flag

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
            reader.skip(3 + 1)?; // video_format + video_full_range
            let colour_desc_present = reader.read_bits(1)?;
            if colour_desc_present == 1 {
                reader.skip(24)?; // colour_primaries + transfer + matrix
            }
        }
        let chroma_loc_present = reader.read_bits(1)?;
        if chroma_loc_present == 1 {
            reader.read_ue()?;
            reader.read_ue()?;
        }
        reader.skip(3)?; // neutral_chroma, field_seq, frame_field_info

        let default_display_window = reader.read_bits(1)?;
        if default_display_window == 1 {
            reader.read_ue()?;
            reader.read_ue()?;
            reader.read_ue()?;
            reader.read_ue()?;
        }

        let timing_info_present = reader.read_bits(1)?;
        if timing_info_present == 1 {
            let num_units_in_tick = reader.read_bits(32)?;
            let time_scale = reader.read_bits(32)?;
            if num_units_in_tick > 0 && time_scale > 0 {
                meta.fps = time_scale as f64 / num_units_in_tick as f64;
            }
        }
    }

    Some(())
}

/// Skip H.265 scaling_list_data() per ITU-T H.265 §7.3.4.
/// Four sizeId dimensions (0..4), each with 6 matrixId entries
/// (step 3 for sizeId==3). Predicted entries consume 1 ue(v);
/// explicitly-coded entries consume 1 se(v) dc (sizeId>1) + coefNum se(v) values.
fn skip_scaling_list_data(reader: &mut BitReader) -> Option<()> {
    for size_id in 0..4u32 {
        let step = if size_id == 3 { 3 } else { 1 };
        let mut matrix_id = 0u32;
        while matrix_id < 6 {
            let pred_mode = reader.read_bits(1)?;
            if pred_mode == 0 {
                reader.read_ue()?;
            } else {
                let coef_num = std::cmp::min(64, 1u32 << (4 + (size_id << 1)));
                if size_id > 1 {
                    reader.read_se()?;
                }
                for _ in 0..coef_num {
                    reader.read_se()?;
                }
            }
            matrix_id += step;
        }
    }
    Some(())
}
