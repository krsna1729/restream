#[test]
fn marker_fixture_probe_recovers_two_audio_tracks() {
    let fixture = crate::test_fixtures::av_marker_transport_fixture("h264", true)
        .unwrap_or_else(|e| panic!("{e}"));
    let ts = std::fs::read(&fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let probe = demuxer
        .take_probe()
        .expect("fixture probe should discover stream metadata");

    assert!(
        probe.video.is_some(),
        "fixture should contain a video stream"
    );
    assert_eq!(probe.video_track_count, 1);
    assert_eq!(
        probe.audio_tracks.len(),
        2,
        "marker fixture should expose two audio tracks"
    );
    assert_eq!(
        probe
            .audio_tracks
            .iter()
            .map(|track| track.track_index)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Audio)
            .map(|packet| packet.track_index)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        2,
        "fixture packets should cover both logical audio tracks"
    );
}

#[test]
fn try_build_probe_waits_for_complete_h264_and_aac_metadata() {
    let (complete_video, complete_audio) = first_probe_ready_payloads();
    let mut demuxer = TsDemuxer::new();
    demuxer.streams = vec![h264_stream_info(0x100), aac_adts_stream_info(0x101, 0)];

    demuxer.try_build_probe(0, &[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80]);
    demuxer.try_build_probe(1, &complete_audio);
    assert!(
        demuxer.take_probe().is_none(),
        "probe must wait for complete video dimensions instead of locking in 0x0 metadata"
    );

    demuxer.try_build_probe(0, &complete_video);
    let probe = demuxer
        .take_probe()
        .expect("probe should finalize once both tracks have complete metadata");
    let video = probe.video.expect("probe should include video metadata");
    assert!(video.width > 0);
    assert!(video.height > 0);
    assert_eq!(probe.audio_tracks.len(), 1);
    assert!(probe.audio_tracks[0].sample_rate > 0);
    assert!(probe.audio_tracks[0].channels > 0);
}

#[test]
fn try_build_probe_keeps_complete_payload_when_later_frames_lack_sps() {
    let (complete_video, complete_audio) = first_probe_ready_payloads();
    let mut demuxer = TsDemuxer::new();
    demuxer.streams = vec![h264_stream_info(0x100), aac_adts_stream_info(0x101, 0)];

    demuxer.try_build_probe(0, &complete_video);
    demuxer.try_build_probe(0, &[0x00, 0x00, 0x00, 0x01, 0x41, 0x9A, 0x00]);
    assert!(demuxer.take_probe().is_none());

    demuxer.try_build_probe(1, &complete_audio);
    let probe = demuxer
        .take_probe()
        .expect("probe must survive non-SPS frames after complete video metadata");
    let video = probe.video.expect("probe should include video metadata");
    assert!(video.width > 0);
    assert!(video.height > 0);
}

#[test]
fn try_build_probe_caches_h264_sequence_header() {
    let (complete_video, complete_audio) = first_probe_ready_payloads();
    let mut demuxer = TsDemuxer::new();
    demuxer.streams = vec![h264_stream_info(0x100), aac_adts_stream_info(0x101, 0)];

    demuxer.try_build_probe(0, &complete_video);
    demuxer.try_build_probe(1, &complete_audio);

    let probe = demuxer
        .take_probe()
        .expect("probe should finalize once both tracks are complete");
    let sequence_header = probe
        .video_sequence_header
        .expect("H.264 probe should synthesize an RTMP startup header");
    assert_eq!(sequence_header[0], 0x17);
    assert_eq!(sequence_header[1], 0x00);
}

#[test]
fn adts_probe() {
    // Valid ADTS header: 48kHz, mono
    let adts = [0xFF, 0xF1, 0x4C, 0x40, 0x02, 0x1F, 0xFC];
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &adts);
    assert_eq!(meta.sample_rate, 48000);
    assert_eq!(meta.channels, 1);
}

#[test]
fn adts_probe_boundary_and_malformed_inputs() {
    // Empty payload must not panic and must leave metadata at its unparsed default.
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &[]);
    assert_eq!(meta.sample_rate, 0);
    assert_eq!(meta.channels, 0);
    assert!(!audio_meta_complete(StreamKind::AacAdts, &meta));

    // One byte short of the 7-byte ADTS fixed header: the length guard must
    // reject it even though the sync word and rate/channel bits look valid.
    let short = [0xFF, 0xF1, 0x4C, 0x40, 0x02, 0x1F];
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &short);
    assert_eq!(meta.sample_rate, 0);
    assert_eq!(meta.channels, 0);
    assert!(!audio_meta_complete(StreamKind::AacAdts, &meta));

    // Sync word mismatch (second byte's top nibble isn't 0xF): must not be
    // parsed as ADTS even with an otherwise 7+ byte payload.
    let bad_sync = [0xFF, 0x00, 0x4C, 0x40, 0x02, 0x1F, 0xFC];
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &bad_sync);
    assert_eq!(meta.sample_rate, 0);
    assert_eq!(meta.channels, 0);
    assert_eq!(meta.profile, None);

    // sample_rate_idx = 13 is reserved (only 0..=12 are defined rates): must
    // leave sample_rate at 0 (incomplete), not panic or index out of bounds.
    let reserved_rate = [0xFF, 0xF1, 0x34, 0x00, 0x02, 0x1F, 0xFC];
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &reserved_rate);
    assert_eq!(
        meta.sample_rate, 0,
        "reserved sample rate index must not map to a rate"
    );
    assert_eq!(meta.profile, Some("Main".to_string()));
    assert!(!audio_meta_complete(StreamKind::AacAdts, &meta));

    // channel_config == 7 is the "8 channels" special case per the ADTS spec.
    let eight_channel = [0xFF, 0xF1, 0x4D, 0xC0, 0x02, 0x1F, 0xFC];
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &eight_channel);
    assert_eq!(meta.channels, 8, "channel_config 7 must map to 8 channels");
    assert!(audio_meta_complete(StreamKind::AacAdts, &meta));
}

/// H.264 RBSP writer for test SPSs: MSB-first bits and Exp-Golomb codes,
/// the encoder side of the probe's `BitReader`.
#[derive(Default)]
struct RbspWriter {
    bits: Vec<bool>,
}

impl RbspWriter {
    fn bits(&mut self, value: u64, count: u32) {
        for shift in (0..count).rev() {
            self.bits.push(value.checked_shr(shift).unwrap_or(0) & 1 == 1);
        }
    }

    fn ue(&mut self, value: u32) {
        let coded = u64::from(value) + 1;
        let len = 64 - coded.leading_zeros();
        self.bits(0, len - 1);
        self.bits(coded, len);
    }

    fn se(&mut self, value: i32) {
        let mapped = if value > 0 {
            2 * value.unsigned_abs() - 1
        } else {
            2 * value.unsigned_abs()
        };
        self.ue(mapped);
    }

    /// rbsp_trailing_bits, then emulation prevention: a NAL payload.
    fn into_nal_payload(mut self) -> Vec<u8> {
        self.bits.push(true);
        while !self.bits.len().is_multiple_of(8) {
            self.bits.push(false);
        }
        let rbsp = self
            .bits
            .chunks(8)
            .map(|byte| byte.iter().fold(0u8, |acc, &bit| (acc << 1) | u8::from(bit)));
        let mut nal = Vec::new();
        let mut zeros = 0;
        for byte in rbsp {
            if zeros >= 2 && byte <= 3 {
                nal.push(3);
                zeros = 0;
            }
            nal.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        nal
    }
}

#[derive(Debug, Clone)]
struct SpsCase {
    profile_idc: u8,
    chroma_format_idc: u32,
    scaling_deltas: Option<Vec<i32>>,
    poc_type: u32,
    poc_offsets: Vec<i32>,
    width_mbs: u32,
    height_units: u32,
    frame_mbs_only: bool,
    crop: [u32; 4],
    timing: Option<(u32, u32)>,
}

impl SpsCase {
    fn high_profile(&self) -> bool {
        matches!(self.profile_idc, 100 | 110 | 122 | 244)
    }

    /// Expected size from the spec's crop units (7.4.2.1.1): CropUnitX/Y
    /// follow SubWidthC/SubHeightC of the chroma format, times the field factor.
    fn expected_size(&self) -> Option<(u32, u32)> {
        let chroma = if self.high_profile() { self.chroma_format_idc } else { 1 };
        let (sub_w, sub_h) = match chroma {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        let field = if self.frame_mbs_only { 1 } else { 2 };
        let [left, right, top, bottom] = self.crop;
        let width = (self.width_mbs * 16).checked_sub((left + right) * sub_w)?;
        let height =
            (field * self.height_units * 16).checked_sub((top + bottom) * sub_h * field)?;
        (width > 0 && height > 0).then_some((width, height))
    }

    /// Annex B access unit with this SPS as its only NAL.
    fn access_unit(&self) -> Vec<u8> {
        let mut w = RbspWriter::default();
        w.bits(u64::from(self.profile_idc), 8);
        w.bits(0, 8); // constraint flags + reserved
        w.bits(40, 8); // level_idc
        w.ue(0); // seq_parameter_set_id
        if self.high_profile() {
            w.ue(self.chroma_format_idc);
            if self.chroma_format_idc == 3 {
                w.bits(0, 1); // separate_colour_plane_flag
            }
            w.ue(0); // bit_depth_luma_minus8
            w.ue(0); // bit_depth_chroma_minus8
            w.bits(0, 1); // qpprime_y_zero_transform_bypass_flag
            w.bits(u64::from(self.scaling_deltas.is_some()), 1);
            if let Some(deltas) = &self.scaling_deltas {
                let lists = if self.chroma_format_idc == 3 { 12 } else { 8 };
                for list in 0..lists {
                    // List 0 is sent with its deltas; the rest are absent.
                    w.bits(u64::from(list == 0), 1);
                    if list == 0 {
                        // A 4x4 list: 16 entries, all deltas sent (none hit 0).
                        for delta in deltas {
                            w.se(*delta);
                        }
                    }
                }
            }
        }
        w.ue(0); // log2_max_frame_num_minus4
        w.ue(self.poc_type);
        match self.poc_type {
            0 => w.ue(0),
            1 => {
                w.bits(0, 1); // delta_pic_order_always_zero_flag
                w.se(-2); // offset_for_non_ref_pic
                w.se(3); // offset_for_top_to_bottom_field
                w.ue(self.poc_offsets.len() as u32);
                for offset in &self.poc_offsets {
                    w.se(*offset);
                }
            }
            _ => {}
        }
        w.ue(1); // max_num_ref_frames
        w.bits(0, 1); // gaps_in_frame_num_value_allowed_flag
        w.ue(self.width_mbs - 1);
        w.ue(self.height_units - 1);
        w.bits(u64::from(self.frame_mbs_only), 1);
        if !self.frame_mbs_only {
            w.bits(0, 1); // mb_adaptive_frame_field_flag
        }
        w.bits(1, 1); // direct_8x8_inference_flag
        let cropped = self.crop != [0; 4];
        w.bits(u64::from(cropped), 1);
        if cropped {
            for edge in self.crop {
                w.ue(edge);
            }
        }
        w.bits(u64::from(self.timing.is_some()), 1); // vui_parameters_present_flag
        if let Some((num_units_in_tick, time_scale)) = self.timing {
            w.bits(0, 1); // aspect_ratio_info_present_flag
            w.bits(0, 1); // overscan_info_present_flag
            w.bits(0, 1); // video_signal_type_present_flag
            w.bits(0, 1); // chroma_loc_info_present_flag
            w.bits(1, 1); // timing_info_present_flag
            w.bits(u64::from(num_units_in_tick), 32);
            w.bits(u64::from(time_scale), 32);
        }
        let mut access_unit = vec![0, 0, 0, 1, 0x67];
        access_unit.extend(w.into_nal_payload());
        access_unit
    }
}

fn sps_case() -> impl Strategy<Value = SpsCase> {
    (
        prop::sample::select(vec![66u8, 77, 88, 100, 110, 122, 244]),
        0u32..=3,
        prop::option::of(prop::collection::vec(-100i32..=100, 16)),
        0u32..=2,
        prop::collection::vec(-1000i32..=1000, 0..4),
        1u32..=240,
        1u32..=135,
        any::<bool>(),
        [0u32..=8, 0u32..=8, 0u32..=8, 0u32..=8],
        prop::option::of((1u32..=1001, 1u32..=120_000)),
    )
        .prop_map(
            |(profile_idc, chroma, scaling, poc_type, poc_offsets, w, h, fmo, crop, timing)| {
                SpsCase {
                    profile_idc,
                    chroma_format_idc: chroma,
                    // Each delta keeps the running scale nonzero, so all 16 are read.
                    scaling_deltas: scaling.map(|deltas| {
                        let mut scale = 8i32;
                        deltas
                            .into_iter()
                            .map(|delta| {
                                let next = (scale + delta + 256).rem_euclid(256);
                                let delta = if next == 0 { delta + 1 } else { delta };
                                scale = (scale + delta + 256).rem_euclid(256);
                                delta
                            })
                            .collect()
                    }),
                    poc_type,
                    poc_offsets,
                    width_mbs: w,
                    height_units: h,
                    frame_mbs_only: fmo,
                    crop,
                    timing,
                }
            },
        )
}

proptest! {
    /// A publisher's SPS sets the probed size and frame rate exactly: every
    /// chroma format, scaling matrix, POC type, field coding and crop is
    /// decoded through to the spec's cropped dimensions.
    #[test]
    fn h264_sps_probe_reports_the_encoded_size(case in sps_case()) {
        let meta = probe_video(StreamKind::H264, 0x100, None, None, &case.access_unit());
        let expected = case.expected_size();
        prop_assert_eq!(
            (meta.width, meta.height),
            expected.unwrap_or((0, 0)),
            "{:?}",
            case
        );
        if let (Some(_), Some((num_units_in_tick, time_scale))) = (expected, case.timing) {
            let fps = f64::from(time_scale) / (2.0 * f64::from(num_units_in_tick));
            prop_assert!((meta.fps - fps).abs() < 1e-9, "fps {} != {}", meta.fps, fps);
        }
    }

    /// A truncated SPS never yields a wrong size: every prefix reports the
    /// full answer or nothing.
    #[test]
    fn truncated_h264_sps_never_reports_a_wrong_size(case in sps_case()) {
        let access_unit = case.access_unit();
        let expected = case.expected_size().unwrap_or((0, 0));
        for cut in 5..access_unit.len() {
            let meta = probe_video(StreamKind::H264, 0x100, None, None, &access_unit[..cut]);
            prop_assert!(
                (meta.width, meta.height) == (0, 0) || (meta.width, meta.height) == expected,
                "prefix {} of {:?} reported {}x{}",
                cut,
                case,
                meta.width,
                meta.height
            );
        }
    }
}

#[derive(Debug, Clone)]
struct HevcSpsCase {
    max_sub_layers: u32,
    sub_layer_flags: Vec<(bool, bool)>,
    chroma_format_idc: u32,
    width: u32,
    height: u32,
    window: [u32; 4],
    ordering_info_for_all: bool,
    scaling_lists: Option<Vec<Option<i32>>>,
    pcm: bool,
    short_term_sets: Vec<(u32, u32)>,
    inter_predict_second: bool,
    long_term_refs: Option<u32>,
    timing: Option<(u32, u32)>,
}

impl HevcSpsCase {
    /// Expected size: the conformance window in chroma units (7.4.3.2.1).
    fn expected_size(&self) -> Option<(u32, u32)> {
        let (sub_w, sub_h) = match self.chroma_format_idc {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        let [left, right, top, bottom] = self.window;
        let width = self.width.checked_sub((left + right) * sub_w)?;
        let height = self.height.checked_sub((top + bottom) * sub_h)?;
        (width > 0 && height > 0).then_some((width, height))
    }

    fn profile_tier_level(&self, w: &mut RbspWriter) {
        w.bits(0, 2); // general_profile_space
        w.bits(0, 1); // general_tier_flag
        w.bits(1, 5); // general_profile_idc: Main
        w.bits(0x6000_0000, 32); // general_profile_compatibility_flags
        w.bits(0, 48); // progressive..frame_only flags + reserved
        w.bits(93, 8); // general_level_idc: 3.1
        let minus1 = self.max_sub_layers - 1;
        for &(profile, level) in &self.sub_layer_flags[..minus1 as usize] {
            w.bits(u64::from(profile), 1);
            w.bits(u64::from(level), 1);
        }
        if minus1 > 0 {
            for _ in minus1..8 {
                w.bits(0, 2); // reserved_zero_2bits
            }
        }
        for &(profile, level) in &self.sub_layer_flags[..minus1 as usize] {
            if profile {
                w.bits(0, 88);
            }
            if level {
                w.bits(93, 8);
            }
        }
    }

    fn rps_and_refs(&self, w: &mut RbspWriter, log2_max_poc_lsb: u32) {
        w.ue(self.short_term_sets.len() as u32);
        let mut delta_pocs = Vec::new();
        for (index, &(negative, positive)) in self.short_term_sets.iter().enumerate() {
            if index > 0 {
                w.bits(u64::from(self.inter_predict_second), 1);
            }
            if index > 0 && self.inter_predict_second {
                w.bits(0, 1); // delta_rps_sign
                w.ue(0); // abs_delta_rps_minus1
                let previous: u32 = delta_pocs[index - 1];
                for _ in 0..=previous {
                    w.bits(1, 1); // used_by_curr_pic_flag
                }
                delta_pocs.push(previous + 1);
            } else {
                w.ue(negative);
                w.ue(positive);
                for _ in 0..negative + positive {
                    w.ue(0); // delta_poc_s*_minus1
                    w.bits(1, 1); // used_by_curr_pic_s*_flag
                }
                delta_pocs.push(negative + positive);
            }
        }
        w.bits(u64::from(self.long_term_refs.is_some()), 1);
        if let Some(count) = self.long_term_refs {
            w.ue(count);
            for picture in 0..count {
                w.bits(u64::from(picture), log2_max_poc_lsb); // lt_ref_pic_poc_lsb_sps
                w.bits(1, 1); // used_by_curr_pic_lt_sps_flag
            }
        }
    }

    fn vui(&self, w: &mut RbspWriter) {
        w.bits(u64::from(self.timing.is_some()), 1); // vui_parameters_present_flag
        if let Some((num_units_in_tick, time_scale)) = self.timing {
            w.bits(0, 1); // aspect_ratio_info_present_flag
            w.bits(0, 1); // overscan_info_present_flag
            w.bits(0, 1); // video_signal_type_present_flag
            w.bits(0, 1); // chroma_loc_info_present_flag
            w.bits(0, 3); // neutral_chroma, field_seq, frame_field_info
            w.bits(0, 1); // default_display_window_flag
            w.bits(1, 1); // vui_timing_info_present_flag
            w.bits(u64::from(num_units_in_tick), 32);
            w.bits(u64::from(time_scale), 32);
        }
    }

    /// Annex B access unit with this SPS as its only NAL (type 33).
    fn access_unit(&self) -> Vec<u8> {
        let mut w = RbspWriter::default();
        w.bits(0, 4); // sps_video_parameter_set_id
        w.bits(u64::from(self.max_sub_layers - 1), 3);
        w.bits(1, 1); // sps_temporal_id_nesting_flag
        self.profile_tier_level(&mut w);
        w.ue(0); // sps_seq_parameter_set_id
        w.ue(self.chroma_format_idc);
        if self.chroma_format_idc == 3 {
            w.bits(0, 1); // separate_colour_plane_flag
        }
        w.ue(self.width);
        w.ue(self.height);
        let windowed = self.window != [0; 4];
        w.bits(u64::from(windowed), 1);
        if windowed {
            for edge in self.window {
                w.ue(edge);
            }
        }
        w.ue(0); // bit_depth_luma_minus8
        w.ue(0); // bit_depth_chroma_minus8
        let log2_max_poc_lsb = 8;
        w.ue(log2_max_poc_lsb - 4);
        w.bits(u64::from(self.ordering_info_for_all), 1);
        let layers = if self.ordering_info_for_all { self.max_sub_layers } else { 1 };
        for _ in 0..layers {
            w.ue(4); // sps_max_dec_pic_buffering_minus1
            w.ue(2); // sps_max_num_reorder_pics
            w.ue(0); // sps_max_latency_increase_plus1
        }
        for _ in 0..6 {
            w.ue(1); // coding/transform block sizes and depths
        }
        w.bits(u64::from(self.scaling_lists.is_some()), 1); // scaling_list_enabled_flag
        if let Some(lists) = &self.scaling_lists {
            w.bits(1, 1); // sps_scaling_list_data_present_flag
            let mut lists = lists.iter();
            for size_id in 0..4u32 {
                let step = if size_id == 3 { 3 } else { 1 };
                for _ in (0..6).step_by(step) {
                    match lists.next().copied().flatten() {
                        None => {
                            w.bits(0, 1); // scaling_list_pred_mode_flag
                            w.ue(0); // scaling_list_pred_matrix_id_delta
                        }
                        Some(delta) => {
                            w.bits(1, 1);
                            if size_id > 1 {
                                w.se(delta); // scaling_list_dc_coef_minus8
                            }
                            for _ in 0..std::cmp::min(64, 1 << (4 + (size_id << 1))) {
                                w.se(delta);
                            }
                        }
                    }
                }
            }
        }
        w.bits(1, 1); // amp_enabled_flag
        w.bits(1, 1); // sample_adaptive_offset_enabled_flag
        w.bits(u64::from(self.pcm), 1);
        if self.pcm {
            w.bits(7, 4);
            w.bits(7, 4);
            w.ue(0);
            w.ue(1);
            w.bits(0, 1);
        }
        self.rps_and_refs(&mut w, log2_max_poc_lsb);
        w.bits(1, 1); // sps_temporal_mvp_enabled_flag
        w.bits(1, 1); // strong_intra_smoothing_enabled_flag
        self.vui(&mut w);
        let mut access_unit = vec![0, 0, 0, 1, 0x42, 0x01];
        access_unit.extend(w.into_nal_payload());
        access_unit
    }
}

fn hevc_sps_case() -> impl Strategy<Value = HevcSpsCase> {
    (
        (1u32..=8, prop::collection::vec(any::<(bool, bool)>(), 7)),
        0u32..=3,
        (8u32..=4096, 8u32..=2304),
        [0u32..=8, 0u32..=8, 0u32..=8, 0u32..=8],
        any::<bool>(),
        prop::option::of(prop::collection::vec(prop::option::of(-8i32..=8), 20)),
        any::<bool>(),
        (prop::collection::vec((0u32..=2, 0u32..=2), 0..=2), any::<bool>()),
        prop::option::of(0u32..=2),
        prop::option::of((1u32..=1001, 1u32..=120_000)),
    )
        .prop_map(
            |(
                (max_sub_layers, sub_layer_flags),
                chroma_format_idc,
                (width, height),
                window,
                ordering_info_for_all,
                scaling_lists,
                pcm,
                (short_term_sets, inter_predict_second),
                long_term_refs,
                timing,
            )| HevcSpsCase {
                max_sub_layers,
                sub_layer_flags,
                chroma_format_idc,
                width,
                height,
                window,
                ordering_info_for_all,
                scaling_lists,
                pcm,
                short_term_sets,
                inter_predict_second,
                long_term_refs,
                timing,
            },
        )
}

proptest! {
    /// An H.265 SPS sets the probed size and frame rate exactly, with any
    /// number of temporal sub-layers, chroma format, conformance window,
    /// scaling lists, PCM, short- and long-term reference sets and VUI.
    #[test]
    fn h265_sps_probe_reports_the_encoded_size(case in hevc_sps_case()) {
        let meta = probe_video(StreamKind::H265, 0x100, None, None, &case.access_unit());
        let expected = case.expected_size();
        prop_assert_eq!(
            (meta.width, meta.height),
            expected.unwrap_or((0, 0)),
            "{:?}",
            case
        );
        if let (Some(_), Some((num_units_in_tick, time_scale))) = (expected, case.timing) {
            let fps = f64::from(time_scale) / f64::from(num_units_in_tick);
            prop_assert!((meta.fps - fps).abs() < 1e-9, "fps {} != {}", meta.fps, fps);
        }
    }

    /// A truncated H.265 SPS reports the full size or nothing.
    #[test]
    fn truncated_h265_sps_never_reports_a_wrong_size(case in hevc_sps_case()) {
        let access_unit = case.access_unit();
        let expected = case.expected_size().unwrap_or((0, 0));
        for cut in 6..access_unit.len() {
            let meta = probe_video(StreamKind::H265, 0x100, None, None, &access_unit[..cut]);
            prop_assert!(
                (meta.width, meta.height) == (0, 0) || (meta.width, meta.height) == expected,
                "prefix {} of {:?} reported {}x{}",
                cut,
                case,
                meta.width,
                meta.height
            );
        }
    }
}

// --- Helpers shared by PMT version tests ---
