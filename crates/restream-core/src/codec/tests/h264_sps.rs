// The H.264 SPS probe shared by RTMP and MPEG-TS ingest.

struct SpsShape {
    profile: u8,
    /// `Some(chroma_format_idc)` for profiles that carry chroma fields.
    chroma: Option<u64>,
    frame_mbs_only: bool,
    width_mbs_minus1: u64,
    height_map_units_minus1: u64,
    crop: Option<(u64, u64, u64, u64)>,
    /// A first scaling-list `delta_scale`, when given.
    scaling_delta: Option<i64>,
}

impl SpsShape {
    fn progressive(profile: u8, width_mbs_minus1: u64, height_map_units_minus1: u64) -> Self {
        Self {
            profile,
            chroma: None,
            frame_mbs_only: true,
            width_mbs_minus1,
            height_map_units_minus1,
            crop: None,
            scaling_delta: None,
        }
    }
}

fn push_se(bits: &mut Vec<bool>, value: i64) {
    let code = if value > 0 { 2 * value - 1 } else { -2 * value };
    push_ue(bits, code as u64);
}

/// An SPS NAL payload (after the NAL header, emulation prevention applied).
fn h264_sps(shape: &SpsShape) -> Vec<u8> {
    let mut bits = Vec::new();
    push_bits(&mut bits, u64::from(shape.profile), 8);
    push_bits(&mut bits, 0, 8); // constraint flags
    push_bits(&mut bits, 40, 8); // level_idc
    push_ue(&mut bits, 0); // seq_parameter_set_id
    if let Some(chroma) = shape.chroma {
        push_ue(&mut bits, chroma);
        if chroma == 3 {
            push_bits(&mut bits, 0, 1);
        }
        push_ue(&mut bits, 0); // bit_depth_luma_minus8
        push_ue(&mut bits, 0); // bit_depth_chroma_minus8
        push_bits(&mut bits, 0, 1); // qpprime_y_zero_transform_bypass_flag
        match shape.scaling_delta {
            Some(delta) => {
                push_bits(&mut bits, 1, 1); // seq_scaling_matrix_present_flag
                push_bits(&mut bits, 1, 1); // list 0 present
                push_se(&mut bits, delta);
                // A delta of -8 ends list 0 (next_scale 0); then the other
                // seven 4:2:0 list-present flags.
                push_bits(&mut bits, 0, 7);
            }
            None => push_bits(&mut bits, 0, 1),
        }
    }
    push_ue(&mut bits, 0); // log2_max_frame_num_minus4
    push_ue(&mut bits, 2); // pic_order_cnt_type
    push_ue(&mut bits, 1); // max_num_ref_frames
    push_bits(&mut bits, 0, 1); // gaps_in_frame_num_value_allowed_flag
    push_ue(&mut bits, shape.width_mbs_minus1);
    push_ue(&mut bits, shape.height_map_units_minus1);
    push_bits(&mut bits, u64::from(shape.frame_mbs_only), 1);
    if !shape.frame_mbs_only {
        push_bits(&mut bits, 0, 1); // mb_adaptive_frame_field_flag
    }
    push_bits(&mut bits, 1, 1); // direct_8x8_inference_flag
    match shape.crop {
        Some((left, right, top, bottom)) => {
            push_bits(&mut bits, 1, 1);
            for offset in [left, right, top, bottom] {
                push_ue(&mut bits, offset);
            }
        }
        None => push_bits(&mut bits, 0, 1),
    }
    push_bits(&mut bits, 0, 1); // vui_parameters_present_flag
    push_bits(&mut bits, 1, 1); // rbsp_stop_one_bit
    insert_emulation_prevention(&pack_bits(&bits))
}

fn dimensions(shape: &SpsShape) -> Option<(u32, u32)> {
    parse_h264_sps(&h264_sps(shape)).map(|sps| (sps.width, sps.height))
}

#[test]
fn progressive_frames_report_their_cropped_size() {
    assert_eq!(dimensions(&SpsShape::progressive(66, 79, 44)), Some((1280, 720)));
    let cropped_1080 = SpsShape {
        crop: Some((0, 0, 0, 4)),
        ..SpsShape::progressive(66, 119, 67)
    };
    assert_eq!(dimensions(&cropped_1080), Some((1920, 1080)));
}

/// 1080i: coded as 34 field-pair map units (1088 lines) with crop_bottom 2.
/// The vertical crop unit is SubHeightC × (2 − frame_mbs_only) = 4, so the
/// picture is 1080 lines. The former RTMP copy of this parser used a fixed
/// unit of 2 and reported 1084.
#[test]
fn interlaced_crop_uses_the_field_crop_unit() {
    let interlaced = SpsShape {
        frame_mbs_only: false,
        crop: Some((0, 0, 0, 2)),
        ..SpsShape::progressive(66, 119, 33)
    };
    assert_eq!(dimensions(&interlaced), Some((1920, 1080)));
}

/// 4:4:4 has SubWidthC = 1: two crop units on each side remove 4 columns.
#[test]
fn chroma_444_crops_in_single_columns() {
    let shape = SpsShape {
        chroma: Some(3),
        crop: Some((2, 2, 0, 0)),
        ..SpsShape::progressive(244, 79, 44)
    };
    assert_eq!(dimensions(&shape), Some((1276, 720)));
}

/// Profile 138 (and 139, 134, 135) carry chroma and bit-depth fields. The
/// former MPEG-TS copy did not list them and read those fields as the rest
/// of the SPS.
#[test]
fn every_high_profile_reads_its_chroma_fields() {
    for profile in [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135] {
        let shape = SpsShape {
            chroma: Some(1),
            ..SpsShape::progressive(profile, 79, 44)
        };
        assert_eq!(dimensions(&shape), Some((1280, 720)), "profile {profile}");
    }
}

#[test]
fn malformed_parameter_sets_are_rejected() {
    let cropped_out = SpsShape {
        crop: Some((0, 0, 0, 8)),
        ..SpsShape::progressive(66, 0, 0)
    };
    assert_eq!(dimensions(&cropped_out), None);
    let out_of_range_delta = SpsShape {
        chroma: Some(1),
        scaling_delta: Some(1000),
        ..SpsShape::progressive(100, 79, 44)
    };
    assert_eq!(dimensions(&out_of_range_delta), None);
    let in_range_delta = SpsShape {
        scaling_delta: Some(-8),
        ..out_of_range_delta
    };
    assert!(dimensions(&in_range_delta).is_some());
    assert_eq!(parse_h264_sps(&[]), None);
    assert_eq!(parse_h264_sps(&[66, 0]), None);
}

proptest! {
    /// Any SPS shape within the spec's ranges reads back the size it encodes.
    #[test]
    fn sps_shapes_read_back_their_size(
        width_mbs_minus1 in 0u64..512,
        height_map_units_minus1 in 0u64..512,
        frame_mbs_only in any::<bool>(),
        chroma in prop::option::of(0u64..=3),
        crop_right in 0u64..4,
        crop_bottom in 0u64..4,
    ) {
        let profile = if chroma.is_some() { 100 } else { 77 };
        let shape = SpsShape {
            profile,
            chroma,
            frame_mbs_only,
            width_mbs_minus1,
            height_map_units_minus1,
            crop: Some((0, crop_right, 0, crop_bottom)),
            scaling_delta: None,
        };
        let chroma_idc = chroma.unwrap_or(1);
        let sub_width = if matches!(chroma_idc, 1 | 2) { 2 } else { 1 };
        let sub_height = if chroma_idc == 1 { 2 } else { 1 };
        let field_factor = if frame_mbs_only { 1 } else { 2 };
        let width = (width_mbs_minus1 + 1) * 16 - crop_right * sub_width;
        let height = (height_map_units_minus1 + 1) * field_factor * 16
            - crop_bottom * sub_height * field_factor;
        prop_assert_eq!(dimensions(&shape), Some((width as u32, height as u32)));
    }
}
