use super::for_each_nal_raw;
use restream_core::{BitReader, VideoMeta};

#[inline]
pub fn is_keyframe(payload: &[u8]) -> bool {
    for_each_nal(payload, |nal_type, _nal_data| {
        // H.265 IRAP NAL types: BLA_W_LP(16)..RSV_IRAP_VCL23(23)
        (16..=23).contains(&nal_type)
    })
}

pub fn find_sps(payload: &[u8]) -> Option<Vec<u8>> {
    let mut result = None;
    for_each_nal_raw(payload, |nal_data| {
        if let Some((&[header, _], payload)) = nal_data.split_first_chunk::<2>()
            && (header >> 1) & 0x3F == 33
        {
            result = Some(payload.to_vec());
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
        let Some(&header) = nal_data.first() else {
            return false;
        };
        // H.265 NAL header: forbidden(1) + nal_unit_type(6) + nuh_layer_id(6) + nuh_temporal_id_plus1(3)
        let nal_type = (header >> 1) & 0x3F;
        // Skip the 2-byte NAL header for payload
        callback(nal_type, nal_data.get(2..).unwrap_or_default())
    })
}

pub fn parse_sps(sps: &[u8], meta: &mut VideoMeta) -> Option<()> {
    if sps.len() < 2 {
        return None;
    }

    let mut reader = BitReader::new(sps);
    let _vps_id = reader.read_bits(4)?;
    let max_sub_layers = reader.read_bits(3)?.checked_add(1)?;
    reader.skip(1)?; // temporal_id_nesting
    read_profile_tier_level(&mut reader, max_sub_layers, meta)?;
    let chroma_format_idc = read_chroma_format(&mut reader)?;
    read_picture_size(&mut reader, chroma_format_idc, meta)?;
    let log2_max_pic_order_cnt = read_bit_depths_and_poc_size(&mut reader)?;
    skip_coding_structure(&mut reader, max_sub_layers)?;
    skip_short_term_ref_pic_sets(&mut reader)?;
    skip_long_term_ref_pics(&mut reader, log2_max_pic_order_cnt)?;

    reader.skip(1)?; // sps_temporal_mvp_enabled_flag
    reader.skip(1)?; // strong_intra_smoothing_enabled_flag

    if flag(&mut reader)? {
        read_vui_frame_rate(&mut reader, meta)?;
    }
    Some(())
}

fn flag(reader: &mut BitReader) -> Option<bool> {
    Some(reader.read_bits(1)? == 1)
}

fn skip_ue(reader: &mut BitReader, count: u32) -> Option<()> {
    for _ in 0..count {
        reader.read_ue()?;
    }
    Some(())
}

/// sps_seq_parameter_set_id and chroma_format_idc (0..=3), skipping
/// separate_colour_plane_flag for 4:4:4.
fn read_chroma_format(reader: &mut BitReader) -> Option<u32> {
    let _sps_id = reader.read_ue()?;
    let chroma_format_idc = reader.read_ue()?;
    if chroma_format_idc > 3 {
        return None;
    }
    if chroma_format_idc == 3 {
        reader.skip(1)?; // separate_colour_plane_flag
    }
    Some(chroma_format_idc)
}

/// Luma and chroma bit depths (at most 16) and log2_max_pic_order_cnt
/// (at most 16), which is returned.
fn read_bit_depths_and_poc_size(reader: &mut BitReader) -> Option<u32> {
    let bit_depth_luma = reader.read_ue()?.checked_add(8)?;
    let bit_depth_chroma = reader.read_ue()?.checked_add(8)?;
    if bit_depth_luma > 16 || bit_depth_chroma > 16 {
        return None;
    }
    let log2_max_pic_order_cnt = reader.read_ue()?.checked_add(4)?;
    if log2_max_pic_order_cnt > 16 {
        return None;
    }
    Some(log2_max_pic_order_cnt)
}

/// Skips the SPS fields between the POC size and the reference picture
/// sets: sub-layer ordering info, block sizes, scaling lists, AMP/SAO and
/// PCM.
fn skip_coding_structure(reader: &mut BitReader, max_sub_layers: u32) -> Option<()> {
    let start = if flag(reader)? {
        0
    } else {
        max_sub_layers.checked_sub(1)?
    };
    for _ in start..max_sub_layers {
        // max_dec_pic_buffering, max_num_reorder_pics, max_latency_increase
        skip_ue(reader, 3)?;
    }

    let _log2_min_luma_coding_block_size = reader.read_ue()?.checked_add(3)?;
    let _log2_diff_max_min_luma_coding_block_size = reader.read_ue()?;
    let _log2_min_luma_transform_block_size = reader.read_ue()?.checked_add(2)?;
    // log2_diff_max_min_luma_transform_block_size,
    // max_transform_hierarchy_depth_inter and _intra
    skip_ue(reader, 3)?;
    skip_coding_tools(reader)
}

/// Scaling lists, AMP/SAO and PCM.
fn skip_coding_tools(reader: &mut BitReader) -> Option<()> {
    let scaling_list_enabled = flag(reader)?;
    if scaling_list_enabled && flag(reader)? {
        skip_scaling_list_data(reader)?;
    }

    reader.skip(1)?; // amp_enabled_flag
    reader.skip(1)?; // sample_adaptive_offset_enabled_flag

    if flag(reader)? {
        reader.skip(4)?; // pcm_sample_bit_depth_luma_minus1
        reader.skip(4)?; // pcm_sample_bit_depth_chroma_minus1
        // log2_min_pcm_luma_coding_block_size_minus3,
        // log2_diff_max_min_pcm_luma_coding_block_size
        skip_ue(reader, 2)?;
        reader.skip(1)?; // pcm_loop_filter_disabled_flag
    }
    Some(())
}

/// profile_tier_level() (H.265 §7.3.3): records the general profile and
/// level, skips the sub-layer profile info.
fn read_profile_tier_level(
    reader: &mut BitReader,
    max_sub_layers: u32,
    meta: &mut VideoMeta,
) -> Option<()> {
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

    if max_sub_layers > 1 {
        skip_sub_layer_profiles(reader, max_sub_layers)?;
    }
    Some(())
}

/// The sub-layer profile and level presence flags, their reserved bits and
/// the profiles and levels they announce (H.265 §7.3.3).
fn skip_sub_layer_profiles(reader: &mut BitReader, max_sub_layers: u32) -> Option<()> {
    // max_sub_layers is 2..=8 here (3 bits plus one).
    let sub_layers = usize::try_from(max_sub_layers.checked_sub(1)?).ok()?;
    let mut present = [(false, false); 8];
    for (profile, level) in present.iter_mut().take(sub_layers) {
        *profile = flag(reader)?;
        *level = flag(reader)?;
    }
    // reserved_zero_2bits for i = sps_max_sub_layers_minus1 .. 7 (H.265 7.3.3).
    reader.skip(9u32.checked_sub(max_sub_layers)?.checked_mul(2)?)?;
    for &(profile, level) in present.iter().take(sub_layers) {
        if profile {
            reader.skip(88)?; // profile info
        }
        if level {
            reader.skip(8)?;
        }
    }
    Some(())
}

/// pic_width/height_in_luma_samples less the conformance window; `None`
/// for a zero or over-cropped size.
fn read_picture_size(
    reader: &mut BitReader,
    chroma_format_idc: u32,
    meta: &mut VideoMeta,
) -> Option<()> {
    let width = reader.read_ue()?;
    let height = reader.read_ue()?;
    if width == 0 || height == 0 {
        return None;
    }
    let (crop_horizontal, crop_vertical) = if flag(reader)? {
        read_conformance_crop(reader, chroma_format_idc)?
    } else {
        (0, 0)
    };
    meta.width = width.checked_sub(crop_horizontal)?;
    meta.height = height.checked_sub(crop_vertical)?;
    if meta.width == 0 || meta.height == 0 {
        return None;
    }
    Some(())
}

/// conf_win_{left,right,top,bottom}_offset scaled to luma samples:
/// (horizontal, vertical) crop.
fn read_conformance_crop(reader: &mut BitReader, chroma_format_idc: u32) -> Option<(u32, u32)> {
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
    Some((crop_horizontal, crop_vertical))
}

/// The most delta POCs one short-term reference picture set may carry.
const MAX_DELTA_POCS: u32 = 64;

/// Skips the SPS's st_ref_pic_set()s (H.265 §7.3.7), bounded so a hostile
/// SPS cannot make the parser loop or allocate without limit.
fn skip_short_term_ref_pic_sets(reader: &mut BitReader) -> Option<()> {
    const MAX_SHORT_TERM_REF_PIC_SETS: u32 = 64;

    let num_short_term_rps = reader.read_ue()?;
    if num_short_term_rps > MAX_SHORT_TERM_REF_PIC_SETS {
        return None;
    }
    let mut num_delta_pocs = Vec::with_capacity(num_short_term_rps as usize);
    for i in 0..num_short_term_rps {
        let inter_pred = i > 0 && flag(reader)?;
        let count = if inter_pred {
            // delta_idx_minus1 is not present in SPS context (stRpsIdx < num_short_term_rps)
            reader.skip(1)?; // delta_rps_sign
            reader.read_ue()?; // abs_delta_rps_minus1
            // RefRpsIdx = i - 1 (delta_idx_minus1 defaults to 0 in SPS)
            let reference = *num_delta_pocs.get(usize::try_from(i.checked_sub(1)?).ok()?)?;
            skip_inter_ref_pic_set(reader, reference)?
        } else {
            skip_explicit_ref_pic_set(reader)?
        };
        if count > MAX_DELTA_POCS {
            return None;
        }
        num_delta_pocs.push(count);
    }
    Some(())
}

/// An explicitly coded set: delta_poc_s0_minus1 + used_by_curr_pic_s0_flag
/// for each negative picture, then the same (s1) for each positive one.
/// Returns its delta POC count.
fn skip_explicit_ref_pic_set(reader: &mut BitReader) -> Option<u32> {
    let num_negative = reader.read_ue()?;
    let num_positive = reader.read_ue()?;
    let total = num_negative.checked_add(num_positive)?;
    if total > MAX_DELTA_POCS {
        return None;
    }
    for _ in 0..total {
        reader.read_ue()?;
        reader.skip(1)?;
    }
    Some(total)
}

/// An inter-predicted set's used/use-delta flags over its reference set's
/// `reference_count + 1` entries; returns its own delta POC count.
fn skip_inter_ref_pic_set(reader: &mut BitReader, reference_count: u32) -> Option<u32> {
    let mut count = 0u32;
    for _ in 0..=reference_count {
        // used_by_curr_pic_flag, else use_delta_flag.
        if flag(reader)? || flag(reader)? {
            count = count.checked_add(1)?;
        }
    }
    Some(count)
}

/// Skips the SPS's long-term reference pictures, bounded.
fn skip_long_term_ref_pics(reader: &mut BitReader, log2_max_pic_order_cnt: u32) -> Option<()> {
    const MAX_LONG_TERM_REF_PICS: u32 = 32;

    if flag(reader)? {
        let num_long_term_ref_pics = reader.read_ue()?;
        if num_long_term_ref_pics > MAX_LONG_TERM_REF_PICS {
            return None;
        }
        for _ in 0..num_long_term_ref_pics {
            reader.read_bits(log2_max_pic_order_cnt)?; // lt_ref_pic_poc_lsb
            reader.skip(1)?; // used_by_curr_pic_lt_flag
        }
    }
    Some(())
}

/// vui_parameters() up to the timing info (H.265 Annex E): records the
/// frame rate when both timing fields are non-zero.
fn read_vui_frame_rate(reader: &mut BitReader, meta: &mut VideoMeta) -> Option<()> {
    skip_vui_picture_info(reader)?;
    if flag(reader)? {
        let num_units_in_tick = reader.read_bits(32)?;
        let time_scale = reader.read_bits(32)?;
        if num_units_in_tick > 0 && time_scale > 0 {
            meta.fps = time_scale as f64 / num_units_in_tick as f64;
        }
    }
    Some(())
}

/// The VUI fields before the timing info: aspect ratio, overscan, video
/// signal type, chroma location, the field flags and the default display
/// window.
fn skip_vui_picture_info(reader: &mut BitReader) -> Option<()> {
    if flag(reader)? && reader.read_bits(8)? == 255 {
        reader.skip(32)?; // sar_width + sar_height
    }
    if flag(reader)? {
        reader.skip(1)?; // overscan_appropriate_flag
    }
    if flag(reader)? {
        skip_video_signal_type(reader)?;
    }
    if flag(reader)? {
        skip_ue(reader, 2)?; // chroma_sample_loc_type_{top,bottom}_field
    }
    reader.skip(3)?; // neutral_chroma, field_seq, frame_field_info
    if flag(reader)? {
        skip_ue(reader, 4)?; // def_disp_win_{left,right,top,bottom}_offset
    }
    Some(())
}

/// video_format, video_full_range_flag and the optional colour
/// description.
fn skip_video_signal_type(reader: &mut BitReader) -> Option<()> {
    reader.skip(3 + 1)?; // video_format + video_full_range
    if flag(reader)? {
        reader.skip(24)?; // colour_primaries + transfer + matrix
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
                let coef_num = 1u32
                    .checked_shl(size_id.checked_mul(2)?.checked_add(4)?)?
                    .min(64);
                if size_id > 1 {
                    reader.read_se()?;
                }
                for _ in 0..coef_num {
                    reader.read_se()?;
                }
            }
            matrix_id = matrix_id.checked_add(step)?;
        }
    }
    Some(())
}
