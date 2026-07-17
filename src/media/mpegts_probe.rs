use super::StreamKind;
use crate::media::engine::{AudioMeta, VideoMeta};

pub(super) fn probe_video(
    kind: StreamKind,
    pid: u16,
    language: Option<String>,
    title: Option<String>,
    pes_payload: &[u8],
) -> VideoMeta {
    let mut meta = VideoMeta {
        codec: kind.codec_name().to_string(),
        width: 0,
        height: 0,
        fps: 0.0,
        bw: None,
        pid: Some(pid),
        language,
        title,
        profile: None,
        level: None,
        pixel_format: None,
    };

    let mut parsed_meta = meta.clone();
    let parsed = match kind {
        StreamKind::H264 => {
            if let Some(ref sps) = find_h264_sps(pes_payload) {
                parse_h264_sps(sps, &mut parsed_meta).is_some()
            } else {
                false
            }
        }
        StreamKind::H265 => {
            if let Some(ref raw_sps) = find_h265_sps(pes_payload) {
                let sps = remove_emulation_prevention(raw_sps);
                parse_h265_sps(&sps, &mut parsed_meta).is_some()
            } else {
                false
            }
        }
        _ => false,
    };
    if parsed {
        meta = parsed_meta;
    }

    meta
}

pub(super) fn video_meta_complete(kind: StreamKind, meta: &VideoMeta) -> bool {
    match kind {
        StreamKind::H264 | StreamKind::H265 => meta.width > 0 && meta.height > 0,
        StreamKind::AacAdts | StreamKind::AacLatm => true,
    }
}

pub(super) fn probe_audio(
    kind: StreamKind,
    track_index: u32,
    pid: u16,
    language: Option<String>,
    title: Option<String>,
    pes_payload: &[u8],
) -> AudioMeta {
    let mut meta = AudioMeta {
        codec: kind.codec_name().to_string(),
        sample_rate: 0,
        channels: 0,
        channel_layout: None,
        track_index,
        pid: Some(pid),
        language,
        title,
        profile: None,
    };

    if kind == StreamKind::AacAdts && pes_payload.len() >= 7 {
        // ADTS header parsing
        if pes_payload[0] == 0xFF && (pes_payload[1] & 0xF0) == 0xF0 {
            let profile_idx = (pes_payload[2] >> 6) as usize;
            meta.profile = match profile_idx {
                0 => Some("Main".to_string()),
                1 => Some("LC".to_string()),
                2 => Some("SSR".to_string()),
                3 => Some("LTP/Reserved".to_string()),
                _ => None,
            };
            let sample_rate_idx = ((pes_payload[2] >> 2) & 0x0F) as usize;
            let channel_config = ((pes_payload[2] & 0x01) << 2) | ((pes_payload[3] >> 6) & 0x03);

            const SAMPLE_RATES: [u32; 13] = [
                96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000,
                7350,
            ];

            if sample_rate_idx < SAMPLE_RATES.len() {
                meta.sample_rate = SAMPLE_RATES[sample_rate_idx];
            }
            meta.channels = channel_config as u32;
            if meta.channels == 7 {
                meta.channels = 8;
            }
        }
    }

    meta
}

pub(super) fn audio_meta_complete(kind: StreamKind, meta: &AudioMeta) -> bool {
    match kind {
        StreamKind::AacAdts => meta.sample_rate > 0 && meta.channels > 0,
        StreamKind::AacLatm | StreamKind::H264 | StreamKind::H265 => true,
    }
}

// --- H.264 NAL unit scanning ---

#[inline]
pub(super) fn h264_is_keyframe(payload: &[u8]) -> bool {
    for_each_nal(payload, |nal_type, _nal_data| {
        // NAL type 5 = IDR slice
        if nal_type == 5 {
            return true;
        }
        false
    })
}

pub(super) fn find_h264_sps(payload: &[u8]) -> Option<Vec<u8>> {
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

fn parse_h264_sps(raw_sps: &[u8], meta: &mut VideoMeta) -> Option<()> {
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

/// Skip H.265 scaling_list_data() per ITU-T H.265 §7.3.4.
/// Four sizeId dimensions (0..4), each with 6 matrixId entries
/// (step 3 for sizeId==3). Predicted entries consume 1 ue(v);
/// explicitly-coded entries consume 1 se(v) dc (sizeId>1) + coefNum se(v) values.
fn skip_h265_scaling_list_data(reader: &mut BitReader) -> Option<()> {
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

// --- H.265 NAL unit scanning ---

#[inline]
pub(super) fn h265_is_keyframe(payload: &[u8]) -> bool {
    for_each_nal_h265(payload, |nal_type, _nal_data| {
        // H.265 IRAP NAL types: BLA_W_LP(16)..RSV_IRAP_VCL23(23)
        (16..=23).contains(&nal_type)
    })
}

pub(super) fn find_h265_sps(payload: &[u8]) -> Option<Vec<u8>> {
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
fn for_each_nal_h265<F>(data: &[u8], mut callback: F) -> bool
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

pub(super) fn parse_h265_sps(sps: &[u8], meta: &mut VideoMeta) -> Option<()> {
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
            skip_h265_scaling_list_data(&mut reader)?;
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

// --- Annex B NAL scanning ---

/// Iterate over Annex B NAL units with H.264 NAL type extraction.
fn for_each_nal<F>(data: &[u8], mut callback: F) -> bool
where
    F: FnMut(u8, &[u8]) -> bool,
{
    for_each_nal_raw(data, |nal_data| {
        if nal_data.is_empty() {
            return false;
        }
        let nal_type = nal_data[0] & 0x1F;
        // Skip the 1-byte NAL header for payload
        callback(nal_type, &nal_data[1..])
    })
}

/// Raw Annex B start-code scanner. Callback receives full NAL data (including header).
fn for_each_nal_raw<F>(data: &[u8], mut callback: F) -> bool
where
    F: FnMut(&[u8]) -> bool,
{
    let starts = crate::media::codec::find_annexb_start_codes(data);
    if starts.is_empty() {
        return false;
    }
    for i in 0..starts.len() {
        let nalu_start = starts[i].1;
        let nalu_end = if i + 1 < starts.len() {
            starts[i + 1].0
        } else {
            data.len()
        };
        if nalu_start < nalu_end && callback(&data[nalu_start..nalu_end]) {
            return true;
        }
    }
    false
}

/// Remove RBSP emulation prevention bytes (0x00 0x00 0x03 → 0x00 0x00).
fn remove_emulation_prevention(data: &[u8]) -> Vec<u8> {
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

// --- Bit reader for exp-golomb ---
struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    fn remaining_bits(&self) -> usize {
        self.data
            .len()
            .saturating_sub(self.byte_pos)
            .saturating_mul(8)
            .saturating_sub(self.bit_pos as usize)
    }

    fn read_bit(&mut self) -> Option<u32> {
        if self.byte_pos >= self.data.len() {
            return None;
        }
        let bit = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1;
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
        Some(bit as u32)
    }

    fn read_bits(&mut self, n: u32) -> Option<u32> {
        if n > 32 || self.remaining_bits() < n as usize {
            return None;
        }
        let mut val = 0u32;
        for _ in 0..n {
            val = (val << 1) | self.read_bit()?;
        }
        Some(val)
    }

    fn skip(&mut self, n: u32) -> Option<()> {
        if self.remaining_bits() < n as usize {
            return None;
        }
        for _ in 0..n {
            self.read_bit()?;
        }
        Some(())
    }

    fn read_ue(&mut self) -> Option<u32> {
        let mut leading_zeros = 0u32;
        while self.read_bit()? == 0 {
            leading_zeros += 1;
            if leading_zeros > 31 {
                return None;
            }
        }
        if leading_zeros == 0 {
            return Some(0);
        }
        let suffix = self.read_bits(leading_zeros)?;
        (1u32.checked_shl(leading_zeros)?)
            .checked_sub(1)?
            .checked_add(suffix)
    }

    fn read_se(&mut self) -> Option<i32> {
        let ue = self.read_ue()?;
        if ue.is_multiple_of(2) {
            let magnitude = i32::try_from(ue / 2).ok()?;
            Some(-magnitude)
        } else {
            i32::try_from(ue / 2 + 1).ok()
        }
    }
}
