use super::*;

#[test]
fn nal_scanner_h264_idr() {
    // Start code + IDR NAL
    let data = [0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB];
    assert!(h264_is_keyframe(&data));

    // Start code + non-IDR slice
    let data2 = [0x00, 0x00, 0x00, 0x01, 0x41, 0xAA, 0xBB];
    assert!(!h264_is_keyframe(&data2));
}

#[test]
fn h265_irap_detection() {
    // H.265 NAL header: byte0 = forbidden(1b) | nal_unit_type(6b) >> ... encoded as (type << 1)
    // IDR_W_RADL = type 19 → byte0 = (19 << 1) = 0x26, byte1 = 0x01 (layer=0, tid=1)
    // for_each_nal_h265 extracts: (byte0 >> 1) & 0x3F = (0x26 >> 1) & 0x3F = 19 ✓
    let idr_nal = vec![0x00, 0x00, 0x00, 0x01, 0x26u8, 0x01, 0xAA, 0xBB];
    assert!(
        h265_is_keyframe(&idr_nal),
        "IDR_W_RADL (type 19) should be a keyframe"
    );

    // IDR_N_LP = type 20 → byte0 = (20 << 1) = 0x28
    let idr_nlp = vec![0x00, 0x00, 0x00, 0x01, 0x28u8, 0x01, 0xCC];
    assert!(
        h265_is_keyframe(&idr_nlp),
        "IDR_N_LP (type 20) should be a keyframe"
    );

    // Non-IRAP: TRAIL_R = type 1 → byte0 = (1 << 1) = 0x02
    let trail_r = vec![0x00, 0x00, 0x00, 0x01, 0x02u8, 0x01, 0xDD];
    assert!(
        !h265_is_keyframe(&trail_r),
        "TRAIL_R (type 1) should not be a keyframe"
    );

    // CRA_NUT = type 21 → byte0 = (21 << 1) = 0x2A
    // CRA is commonly produced by software encoders (ffmpeg, x265) and hardware
    // encoders. Must be treated as a keyframe for ring-buffer overflow recovery.
    let cra = vec![0x00, 0x00, 0x00, 0x01, 0x2Au8, 0x01, 0xEE];
    assert!(
        h265_is_keyframe(&cra),
        "CRA_NUT (type 21) should be a keyframe"
    );

    // BLA_W_LP = type 16 → byte0 = (16 << 1) = 0x20 (low boundary of IRAP range)
    let bla = vec![0x00, 0x00, 0x00, 0x01, 0x20u8, 0x01, 0xFF];
    assert!(
        h265_is_keyframe(&bla),
        "BLA_W_LP (type 16) should be a keyframe"
    );

    // Type 15 (non-IRAP, just below boundary) → byte0 = (15 << 1) = 0x1E
    let non_irap_below = vec![0x00, 0x00, 0x00, 0x01, 0x1Eu8, 0x01, 0x00];
    assert!(
        !h265_is_keyframe(&non_irap_below),
        "Type 15 is non-IRAP, should not be a keyframe"
    );

    // Type 24 (just above IRAP range) → byte0 = (24 << 1) = 0x30
    let non_irap_above = vec![0x00, 0x00, 0x00, 0x01, 0x30u8, 0x01, 0x00];
    assert!(
        !h265_is_keyframe(&non_irap_above),
        "Type 24 is non-IRAP, should not be a keyframe"
    );
}

// --- NAL scanner edge cases ---

#[test]
fn h264_is_keyframe_empty_payload_returns_false() {
    assert!(!h264_is_keyframe(&[]));
}

#[test]
fn h264_is_keyframe_no_start_codes_returns_false() {
    // Non-Annex B data, no 0x000001 or 0x00000001 start code
    assert!(!h264_is_keyframe(&[0x00, 0x01, 0x65, 0x88]));
}

#[test]
fn h265_is_keyframe_empty_payload_returns_false() {
    assert!(!h265_is_keyframe(&[]));
}

#[test]
fn h265_is_keyframe_no_start_codes_returns_false() {
    assert!(!h265_is_keyframe(&[0x00, 0x01, 0x26, 0x01]));
}

#[test]
fn find_h264_sps_no_sps_nal_returns_none() {
    // IDR slice (nal_type=5), no SPS (nal_type=7) present
    let data = [0x00, 0x00, 0x00, 0x01, 0x65u8, 0xAA, 0xBB];
    assert!(find_h264_sps(&data).is_none());
}

#[test]
fn find_h264_sps_empty_returns_none() {
    assert!(find_h264_sps(&[]).is_none());
}

#[test]
fn find_h264_sps_extracts_sps_nal() {
    // SPS NAL: nal_type=7 (byte & 0x1F == 7)
    // find_h264_sps returns NAL data after the first byte (the header byte)
    let data = [0x00, 0x00, 0x00, 0x01, 0x67u8, 0x64, 0x00, 0x1F];
    let sps = find_h264_sps(&data);
    assert!(sps.is_some(), "SPS NAL type 7 must be found");
    // Returns data after the NAL header byte (0x67)
    assert_eq!(sps.unwrap(), vec![0x64, 0x00, 0x1F]);
}

#[test]
fn find_h265_sps_no_sps_returns_none() {
    // H.265 IDR (nal_type 19, byte0=(19<<1)=0x26), not SPS (nal_type 33)
    let data = [0x00, 0x00, 0x00, 0x01, 0x26u8, 0x01, 0xAA];
    assert!(find_h265_sps(&data).is_none());
}

#[test]
fn find_h265_sps_empty_returns_none() {
    assert!(find_h265_sps(&[]).is_none());
}

#[test]
fn find_h265_sps_extracts_sps_payload() {
    // H.265 SPS: nal_unit_type=33 → byte0=(33<<1)=0x42, byte1=nuh_layer/temporal
    // find_h265_sps returns sps[2..] (skips the 2-byte NAL header)
    let data = [0x00, 0x00, 0x00, 0x01, 0x42u8, 0x01, 0xAA, 0xBB, 0xCC];
    let sps = find_h265_sps(&data);
    assert!(sps.is_some(), "H.265 SPS (type 33) must be found");
    assert_eq!(sps.unwrap(), vec![0xAA, 0xBB, 0xCC]);
}

/// Appends `width` bits of `value` (MSB-first) to a bitstream under
/// construction. Shared by SPS-bitstream builders below.
fn push_bits(bits: &mut Vec<bool>, value: u64, width: u32) {
    for shift in (0..width).rev() {
        bits.push((value >> shift) & 1 == 1);
    }
}

/// Appends an Exp-Golomb `ue(v)` encoding of `value`.
fn push_ue(bits: &mut Vec<bool>, value: u32) {
    let code_num = value as u64 + 1;
    let width = u64::BITS - code_num.leading_zeros();
    bits.extend(std::iter::repeat_n(false, (width - 1) as usize));
    push_bits(bits, code_num, width);
}

/// Packs a bitstream into bytes, zero-padding the final byte.
fn pack_bits(bits: &[bool]) -> Vec<u8> {
    bits.chunks(8)
        .map(|chunk| {
            chunk.iter().enumerate().fold(0u8, |byte, (index, bit)| {
                byte | (u8::from(*bit) << (7 - index))
            })
        })
        .collect()
}

/// Inverse of `remove_emulation_prevention`: inserts a 0x03 byte after any
/// `00 00` run followed by a byte <= 3, so a hand-built RBSP round-trips
/// through the parser's emulation-prevention removal unchanged. Needed
/// because a randomly chosen Exp-Golomb field can incidentally contain a
/// `00 00 0x` sequence, which would otherwise either get silently eaten by
/// `remove_emulation_prevention` (for `00 00 03`) or, worse, be mistaken by
/// the Annex-B start-code scanner for a `00 00 01` NAL boundary and truncate
/// the payload before it ever reaches the SPS parser.
fn insert_emulation_prevention(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zero_run = 0u32;
    for &byte in data {
        if zero_run >= 2 && byte <= 3 {
            out.push(3);
            zero_run = 0;
        }
        out.push(byte);
        zero_run = if byte == 0 { zero_run + 1 } else { 0 };
    }
    out
}

/// Appends a well-formed H.265 `profile_tier_level` + SPS id + chroma format
/// prefix (single sub-layer, Main profile, level 3.0, 4:2:0 chroma).
fn push_h265_profile_prefix(bits: &mut Vec<bool>) {
    push_bits(bits, 0, 4); // sps_video_parameter_set_id
    push_bits(bits, 0, 3); // sps_max_sub_layers_minus1
    push_bits(bits, 1, 1); // sps_temporal_id_nesting_flag
    push_bits(bits, 0, 2); // general_profile_space
    push_bits(bits, 0, 1); // general_tier_flag
    push_bits(bits, 1, 5); // general_profile_idc
    push_bits(bits, 0, 32); // compatibility flags
    push_bits(bits, 0, 48); // constraint flags
    push_bits(bits, 90, 8); // general_level_idc
    push_ue(bits, 0); // sps_seq_parameter_set_id
    push_ue(bits, 1); // chroma_format_idc
}

#[test]
fn malformed_sps_bitstreams_fail_closed_without_partial_metadata() {
    let h264_exp_golomb_shift_overflow = [
        0, 0, 0, 1, 0x67, // Annex B start code and H.264 SPS NAL header
        100, 0, 31, // High profile, compatibility, level
        0, 0, 0, 0, 0x80, // 32 zero prefix bits followed by the stop bit
        0, 0, 0, 0, // 32 suffix bits
    ];

    let mut h265_bits = Vec::new();
    push_h265_profile_prefix(&mut h265_bits);
    push_ue(&mut h265_bits, 0); // pic_width_in_luma_samples
    push_ue(&mut h265_bits, 0); // pic_height_in_luma_samples
    push_bits(&mut h265_bits, 1, 1); // conformance_window_flag
    push_ue(&mut h265_bits, 1); // conf_win_left_offset: larger than width
    push_ue(&mut h265_bits, 0);
    push_ue(&mut h265_bits, 0);
    push_ue(&mut h265_bits, 0);
    let mut h265_crop_underflow = vec![
        0, 0, 0, 1, 0x42, 0x01, // Annex B start code and H.265 SPS NAL header
    ];
    h265_crop_underflow.extend(pack_bits(&h265_bits));

    let mut h265_count_bits = Vec::new();
    push_h265_profile_prefix(&mut h265_count_bits);
    push_ue(&mut h265_count_bits, 1_920);
    push_ue(&mut h265_count_bits, 1_080);
    push_bits(&mut h265_count_bits, 0, 1); // conformance_window_flag
    push_ue(&mut h265_count_bits, 0); // bit_depth_luma_minus8
    push_ue(&mut h265_count_bits, 0); // bit_depth_chroma_minus8
    push_ue(&mut h265_count_bits, 0); // log2_max_pic_order_cnt_lsb_minus4
    push_bits(&mut h265_count_bits, 0, 1); // sub_layer_ordering_info_present
    for _ in 0..9 {
        push_ue(&mut h265_count_bits, 0);
    }
    push_bits(&mut h265_count_bits, 0, 4); // scaling, AMP, SAO, and PCM flags
    push_ue(&mut h265_count_bits, 65); // num_short_term_ref_pic_sets
    let mut h265_unbounded_count = vec![0, 0, 0, 1, 0x42, 0x01];
    h265_unbounded_count.extend(pack_bits(&h265_count_bits));

    let outcomes = [
        (
            "h264-exp-golomb-overflow",
            StreamKind::H264,
            h264_exp_golomb_shift_overflow.as_slice(),
        ),
        (
            "h265-crop-underflow",
            StreamKind::H265,
            h265_crop_underflow.as_slice(),
        ),
        (
            "h265-unbounded-short-term-rps-count",
            StreamKind::H265,
            h265_unbounded_count.as_slice(),
        ),
    ]
    .into_iter()
    .map(|(case, kind, bytes)| {
        (
            case,
            std::panic::catch_unwind(|| probe_video(kind, 0x100, None, None, bytes)),
        )
    })
    .collect::<Vec<_>>();

    for (case, result) in outcomes {
        assert!(result.is_ok(), "{case} panicked");
        let meta = result.expect("probe result");
        assert_eq!(meta.width, 0, "{case} published an invalid width");
        assert_eq!(meta.height, 0, "{case} published an invalid height");
        assert!(meta.profile.is_none(), "{case} published partial metadata");
        assert!(meta.level.is_none(), "{case} published partial metadata");
    }
}

#[test]
fn h264_scaling_matrix_uses_4x4_list_length_before_dimensions() {
    let payload = [
        0, 0, 0, 1, 0x67, // Annex B start code and SPS NAL header
        100, 0, 31, // High profile, compatibility, level
        0xAD, 0xFF, 0xFF, 0x80, 0xF0, 0x50, 0x7E, 0x00,
    ];

    let meta = probe_video(StreamKind::H264, 0x100, None, None, &payload);

    assert_eq!(meta.profile.as_deref(), Some("High"));
    assert_eq!((meta.width, meta.height), (320, 240));
}

proptest! {
    #[test]
    fn probe_video_never_panics(
        h265 in any::<bool>(),
        pes_payload in prop::collection::vec(any::<u8>(), 0..512),
    ) {
        let kind = if h265 { StreamKind::H265 } else { StreamKind::H264 };
        let _ = probe_video(kind, 0x100, None, None, &pes_payload);
    }

    #[test]
    fn probe_video_h264_truncation_never_yields_partial_metadata(
        profile_idc in prop::sample::select(vec![66u8, 77, 88, 100, 110, 122, 244]),
        level_idc in 0u8..255,
        width_mbs_minus1 in 0u32..500,
        height_map_units_minus1 in 0u32..500,
    ) {
        let mut bits = Vec::new();
        push_ue(&mut bits, 0); // seq_parameter_set_id
        if matches!(profile_idc, 100 | 110 | 122 | 244) {
            push_ue(&mut bits, 1); // chroma_format_idc (4:2:0)
            push_ue(&mut bits, 0); // bit_depth_luma_minus8
            push_ue(&mut bits, 0); // bit_depth_chroma_minus8
            push_bits(&mut bits, 0, 1); // qpprime_y_zero_transform_bypass_flag
            push_bits(&mut bits, 0, 1); // seq_scaling_matrix_present_flag
        }
        push_ue(&mut bits, 0); // log2_max_frame_num_minus4
        push_ue(&mut bits, 0); // pic_order_cnt_type
        push_ue(&mut bits, 0); // log2_max_pic_order_cnt_lsb_minus4
        push_ue(&mut bits, 0); // max_num_ref_frames
        push_bits(&mut bits, 0, 1); // gaps_in_frame_num_allowed_flag
        push_ue(&mut bits, width_mbs_minus1);
        push_ue(&mut bits, height_map_units_minus1);
        push_bits(&mut bits, 1, 1); // frame_mbs_only_flag
        push_bits(&mut bits, 0, 1); // direct_8x8_inference_flag
        push_bits(&mut bits, 0, 1); // frame_cropping_flag
        push_bits(&mut bits, 0, 1); // vui_parameters_present_flag

        let mut raw_sps = vec![profile_idc, 0, level_idc];
        raw_sps.extend(pack_bits(&bits));

        let mut payload = vec![0, 0, 0, 1, 0x67];
        payload.extend(insert_emulation_prevention(&raw_sps));

        let expected_width = (width_mbs_minus1 + 1) * 16;
        let expected_height = (height_map_units_minus1 + 1) * 16;

        let meta = probe_video(StreamKind::H264, 0x100, None, None, &payload);
        prop_assert_eq!(meta.width, expected_width);
        prop_assert_eq!(meta.height, expected_height);
        prop_assert!(meta.profile.is_some());
        prop_assert!(meta.level.is_some());

        for cut in 0..payload.len() {
            let partial = probe_video(StreamKind::H264, 0x100, None, None, &payload[..cut]);
            let fully_default = partial.width == 0
                && partial.height == 0
                && partial.profile.is_none()
                && partial.level.is_none();
            prop_assert!(
                fully_default,
                "truncating at byte {cut} of {} must fail closed, got {partial:?}",
                payload.len()
            );
        }
    }

    #[test]
    fn probe_video_h265_truncation_never_yields_partial_metadata(
        width in 16u32..4096,
        height in 16u32..2160,
    ) {
        let mut bits = Vec::new();
        push_h265_profile_prefix(&mut bits);
        push_ue(&mut bits, width); // pic_width_in_luma_samples
        push_ue(&mut bits, height); // pic_height_in_luma_samples
        push_bits(&mut bits, 0, 1); // conformance_window_flag
        push_ue(&mut bits, 0); // bit_depth_luma_minus8
        push_ue(&mut bits, 0); // bit_depth_chroma_minus8
        push_ue(&mut bits, 0); // log2_max_pic_order_cnt_lsb_minus4
        push_bits(&mut bits, 1, 1); // sps_sub_layer_ordering_info_present_flag
        push_ue(&mut bits, 0); // sps_max_dec_pic_buffering_minus1[0]
        push_ue(&mut bits, 0); // sps_max_num_reorder_pics[0]
        push_ue(&mut bits, 0); // sps_max_latency_increase_plus1[0]
        push_ue(&mut bits, 0); // log2_min_luma_coding_block_size_minus3
        push_ue(&mut bits, 0); // log2_diff_max_min_luma_coding_block_size
        push_ue(&mut bits, 0); // log2_min_luma_transform_block_size_minus2
        push_ue(&mut bits, 0); // log2_diff_max_min_luma_transform_block_size
        push_ue(&mut bits, 0); // max_transform_hierarchy_depth_inter
        push_ue(&mut bits, 0); // max_transform_hierarchy_depth_intra
        push_bits(&mut bits, 0, 1); // scaling_list_enabled_flag
        push_bits(&mut bits, 0, 1); // amp_enabled_flag
        push_bits(&mut bits, 0, 1); // sample_adaptive_offset_enabled_flag
        push_bits(&mut bits, 0, 1); // pcm_enabled_flag
        push_ue(&mut bits, 0); // num_short_term_ref_pic_sets
        push_bits(&mut bits, 0, 1); // long_term_ref_pics_present_flag
        push_bits(&mut bits, 0, 1); // sps_temporal_mvp_enabled_flag
        push_bits(&mut bits, 0, 1); // strong_intra_smoothing_enabled_flag
        push_bits(&mut bits, 0, 1); // vui_parameters_present_flag

        let mut raw_sps = vec![0x42u8, 0x01];
        raw_sps.extend(pack_bits(&bits));

        let mut payload = vec![0, 0, 0, 1];
        payload.extend(insert_emulation_prevention(&raw_sps));

        let meta = probe_video(StreamKind::H265, 0x100, None, None, &payload);
        prop_assert_eq!(meta.width, width);
        prop_assert_eq!(meta.height, height);
        prop_assert!(meta.profile.is_some());

        for cut in 0..payload.len() {
            let partial = probe_video(StreamKind::H265, 0x100, None, None, &payload[..cut]);
            let fully_default = partial.width == 0
                && partial.height == 0
                && partial.profile.is_none()
                && partial.level.is_none();
            prop_assert!(
                fully_default,
                "truncating at byte {cut} of {} must fail closed, got {partial:?}",
                payload.len()
            );
        }
    }
}
