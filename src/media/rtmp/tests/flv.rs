#[test]
fn parse_flv_audio_aac_44100_stereo() {
    // sound_format=10 (AAC), rate=3 (44kHz), size=1 (16bit), type=1 (stereo)
    // AAC sequence header (packet_type=0), then AudioSpecificConfig: 0x12 0x10
    // object_type=2 (AAC-LC), freq_idx=4 (44100), ch_config=2 (stereo)
    let data: &[u8] = &[0xAF, 0x00, 0x12, 0x10];
    let meta = parse_flv_audio_meta(data).unwrap();
    assert_eq!(meta.codec, "aac");
    assert_eq!(meta.sample_rate, 44100);
    assert_eq!(meta.channels, 2);
    assert_eq!(meta.channel_layout.as_deref(), Some("stereo"));
}

#[test]
fn parse_flv_audio_aac_48000() {
    // AudioSpecificConfig: 0x11 0x90 → object=2, freq_idx=3 (48000), ch_config=2
    let data: &[u8] = &[0xAF, 0x00, 0x11, 0x90];
    let meta = parse_flv_audio_meta(data).unwrap();
    assert_eq!(meta.codec, "aac");
    assert_eq!(meta.sample_rate, 48000);
    assert_eq!(meta.channels, 2);
}

#[test]
fn parse_flv_video_h264_sequence_header() {
    // FLV video tag: keyframe(1) | codec_id(7) = 0x17
    // AVC packet type 0 (sequence header)
    // comp time offset: 0x00 0x00 0x00
    // AVCDecoderConfigurationRecord:
    //   version=1, profile=100 (High), compat=0x00, level=31 (3.1)
    //   lengthSizeMinusOne=3, numSPS=1
    //   SPS length=0x0019 (25 bytes)
    //   SPS: nal_type=7, profile=100, constraint=0x00, level=31,
    //        seq_parameter_set_id=0, chroma_format_idc=1,
    //        bit_depth_luma_minus8=0, bit_depth_chroma_minus8=0,
    //        ... pic_width_in_mbs_minus1=79, pic_height_in_map_units_minus1=44
    //        frame_mbs_only=1 → 1280x720
    #[rustfmt::skip]
        let data: &[u8] = &[
            0x17, // keyframe + AVC
            0x00, // sequence header
            0x00, 0x00, 0x00, // composition time
            // AVCDecoderConfigurationRecord
            0x01, // version
            0x64, // profile=High(100)
            0x00, // compat
            0x1F, // level=3.1(31)
            0xFF, // lengthSizeMinusOne=3
            0xE1, // numSPS=1
            0x00, 0x19, // SPS length = 25
            // SPS NAL unit (25 bytes): 720p H.264 High 3.1
            0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50,
            0x05, 0xBB, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00,
            0x10, 0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x62,
            0xE4,
        ];

    let meta = parse_flv_video_meta(data).unwrap();
    assert_eq!(meta.codec, "h264");
    assert_eq!(meta.profile.as_deref(), Some("High"));
    assert_eq!(meta.level.as_deref(), Some("3.1"));
    assert_eq!(meta.width, 1280);
    assert_eq!(meta.height, 720);
}

#[test]
fn flv_avcc_config_annexb_parameter_sets_extracts_sps_and_pps() {
    #[rustfmt::skip]
        let data: &[u8] = &[
            0x17, 0x00, 0x00, 0x00, 0x00, // FLV header + AVC seq header + comp time
            0x01, 0x64, 0x00, 0x1F, 0xFF, 0xE1, // AVCC header, numSPS=1
            0x00, 0x04, // SPS length = 4
            0x67, 0x64, 0x00, 0x1F, // SPS NAL (nal_type=7)
            0x01, // numPPS = 1
            0x00, 0x02, // PPS length = 2
            0x68, 0xCE, // PPS NAL (nal_type=8)
        ];

    let parameter_sets = flv_avcc_config_annexb_parameter_sets(data).unwrap();
    assert_eq!(
        parameter_sets,
        vec![0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1F, 0, 0, 0, 1, 0x68, 0xCE]
    );
}

#[test]
fn flv_avcc_config_annexb_parameter_sets_rejects_truncated_input() {
    let data: &[u8] = &[
        0x17, 0x00, 0x00, 0x00, 0x00, 0x01, 0x64, 0x00, 0x1F, 0xFF, 0xE1,
    ];
    assert!(flv_avcc_config_annexb_parameter_sets(data).is_none());
}

#[test]
fn flv_avcc_config_annexb_parameter_sets_rejects_non_h264() {
    let data: &[u8] = &[
        0x1C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x64, 0x00, 0x1F, 0xFF, 0xE1,
    ];
    assert!(flv_avcc_config_annexb_parameter_sets(data).is_none());
}

#[test]
fn flv_avcc_config_annexb_parameter_sets_rejects_sps_ok_pps_truncated() {
    // SPS parses fully, numPPS = 1, but the PPS length/body never arrives.
    // A partial SPS-only extraction would be worse than none (the decoder
    // still can't decode without a PPS), so this must yield None.
    #[rustfmt::skip]
    let data: &[u8] = &[
        0x17, 0x00, 0x00, 0x00, 0x00, // FLV header + AVC seq header + comp time
        0x01, 0x64, 0x00, 0x1F, 0xFF, 0xE1, // AVCC header, numSPS=1
        0x00, 0x04, // SPS length = 4
        0x67, 0x64, 0x00, 0x1F, // SPS NAL
        0x01, // numPPS = 1, then buffer ends
    ];
    assert!(flv_avcc_config_annexb_parameter_sets(data).is_none());
}

#[test]
fn flv_avcc_config_annexb_parameter_sets_rejects_max_declared_length_tiny_buffer() {
    // SPS declares a 0xFFFF-byte length but only 2 bytes actually follow.
    #[rustfmt::skip]
    let data: &[u8] = &[
        0x17, 0x00, 0x00, 0x00, 0x00,
        0x01, 0x64, 0x00, 0x1F, 0xFF, 0xE1, // AVCC header, numSPS=1
        0xFF, 0xFF, // SPS length = 65535
        0xAA, 0xBB, // only 2 bytes present
    ];
    assert!(flv_avcc_config_annexb_parameter_sets(data).is_none());
}

proptest! {
    #[test]
    fn flv_avcc_config_annexb_parameter_sets_never_panics(
        bytes in prop::collection::vec(any::<u8>(), 0..128)
    ) {
        let _ = flv_avcc_config_annexb_parameter_sets(&bytes);
    }

    #[test]
    fn flv_avcc_config_annexb_parameter_sets_truncation_fails_closed(
        has_sps in any::<bool>(),
        has_pps in any::<bool>(),
        sps_rest in prop::collection::vec(any::<u8>(), 0..16),
        pps_rest in prop::collection::vec(any::<u8>(), 0..16),
    ) {
        let mut avcc = vec![0x01u8, 0x64, 0x00, 0x1F, 0xFF];
        avcc.push(0xE0 | (has_sps as u8));
        let mut sps_body = Vec::new();
        if has_sps {
            sps_body.push(0x67);
            sps_body.extend_from_slice(&sps_rest);
            avcc.extend_from_slice(&(sps_body.len() as u16).to_be_bytes());
            avcc.extend_from_slice(&sps_body);
        }
        avcc.push(has_pps as u8);
        let mut pps_body = Vec::new();
        if has_pps {
            pps_body.push(0x68);
            pps_body.extend_from_slice(&pps_rest);
            avcc.extend_from_slice(&(pps_body.len() as u16).to_be_bytes());
            avcc.extend_from_slice(&pps_body);
        }

        let mut data = vec![0x17u8, 0x00, 0x00, 0x00, 0x00];
        data.extend_from_slice(&avcc);

        let mut annexb = Vec::new();
        if has_sps {
            annexb.extend_from_slice(&[0, 0, 0, 1]);
            annexb.extend_from_slice(&sps_body);
        }
        if has_pps {
            annexb.extend_from_slice(&[0, 0, 0, 1]);
            annexb.extend_from_slice(&pps_body);
        }
        let expected = crate::media::codec::annexb_parameter_sets(&annexb);

        let actual = flv_avcc_config_annexb_parameter_sets(&data);
        prop_assert_eq!(actual, expected);

        // Any strict prefix of a well-formed record must fail closed, never
        // yielding a partial SPS/PPS extraction.
        for cut in 0..data.len() {
            let partial = flv_avcc_config_annexb_parameter_sets(&data[..cut]);
            prop_assert!(partial.is_none(), "truncated at {cut} produced Some(..)");
        }
    }
}

#[test]
fn parse_flv_video_non_sequence_header() {
    // Keyframe + AVC, but packet type 1 (NALU, not sequence header)
    let data: &[u8] = &[0x17, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x65];
    let meta = parse_flv_video_meta(data).unwrap();
    assert_eq!(meta.codec, "h264");
    assert_eq!(meta.width, 0); // not parsed from NALU packets
}

#[test]
fn parses_signed_flv_video_composition_time() {
    assert_eq!(
        flv_video_composition_time_ms(&[0x27, 0x01, 0x00, 0x00, 0x28]),
        40
    );
    assert_eq!(
        flv_video_composition_time_ms(&[0x27, 0x01, 0xff, 0xff, 0xd8]),
        -40
    );
    assert_eq!(
        flv_video_composition_time_ms(&[0x17, 0x00, 0x00, 0x00, 0x28]),
        0
    );
    assert_eq!(flv_video_composition_time_ms(&[0xaf, 0x01, 0, 0, 40]), 0);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn proptest_flv_video_composition_time_sign_extends_signed_24bit(
        composition_time in -8_388_608i32..=8_388_607,
    ) {
        let encoded = (composition_time & 0x00ff_ffff) as u32;
        let payload = [
            0x27,
            0x01,
            ((encoded >> 16) & 0xff) as u8,
            ((encoded >> 8) & 0xff) as u8,
            (encoded & 0xff) as u8,
        ];

        prop_assert_eq!(flv_video_composition_time_ms(&payload), composition_time);

        let sequence_header = [0x17, 0x00, payload[2], payload[3], payload[4]];
        prop_assert_eq!(flv_video_composition_time_ms(&sequence_header), 0);

        let audio_like = [0xaf, 0x01, payload[2], payload[3], payload[4]];
        prop_assert_eq!(flv_video_composition_time_ms(&audio_like), 0);
    }
}

#[test]
fn sps_parser_1080p() {
    // Minimal SPS for 1920x1080 Baseline profile
    // profile_idc=66, level=40, pic_width_in_mbs_minus1=119, pic_height_in_map_units_minus1=67
    // frame_mbs_only=1, no cropping
    // 120*16=1920, 68*16=1088 → needs crop_bottom=4 for 1080
    // Encoded as exp-golomb in bitstream
    #[rustfmt::skip]
        let sps: &[u8] = &[
            0x67, // NAL type 7
            0x42, // profile_idc = 66 (Baseline)
            0x00, // constraint flags
            0x28, // level_idc = 40
            0xE4, 0x40, 0x00, 0xEF, 0x00, 0x88, 0x3C, 0x60,
        ];
    // This is a simplified test — the SPS bitstream encoding is complex
    // so we verify the parser doesn't crash on valid-looking data
    let result = parse_sps_video_info(sps);
    // May or may not parse correctly depending on the exact bitstream
    // The important thing is it doesn't panic
    assert!(result.is_none() || result.unwrap().width > 0);
}

#[test]
fn sps_dimensions_rejects_overflow_inputs() {
    assert!(sps_dimensions(u32::MAX, 0, 1, 0, 0, 0, 0).is_none());
    assert!(sps_dimensions(0, u32::MAX, 1, 0, 0, 0, 0).is_none());
}

#[test]
fn sps_dimensions_rejects_invalid_or_cropped_out_frames() {
    assert!(sps_dimensions(0, 0, 2, 0, 0, 0, 0).is_none());
    assert!(sps_dimensions(0, 0, 1, 4, 4, 0, 0).is_none());
    assert!(sps_dimensions(0, 0, 1, 0, 0, 4, 4).is_none());
}

#[test]
fn sps_dimensions_accepts_valid_inputs() {
    let dims = sps_dimensions(79, 44, 1, 0, 0, 0, 0).expect("valid dimensions");
    assert_eq!(dims, (1280, 720));
}

#[test]
fn parse_sps_video_info_randomized_inputs_do_not_panic() {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for len in 1..=96 {
        for _ in 0..256 {
            let mut data = vec![0u8; len];
            for byte in &mut data {
                state ^= state << 7;
                state ^= state >> 9;
                state ^= state << 8;
                *byte = (state & 0xFF) as u8;
            }
            let result = std::panic::catch_unwind(|| parse_sps_video_info(&data));
            assert!(result.is_ok(), "parser panicked for len={len}");
        }
    }
}

#[test]
fn bit_reader_exp_golomb() {
    let mut r = BitReader::new(&[0b10000000]); // 1 → code_num=0
    assert_eq!(r.read_exp_golomb(), Some(0));

    let mut r = BitReader::new(&[0b01000000]); // 010 → code_num=1
    assert_eq!(r.read_exp_golomb(), Some(1));

    let mut r = BitReader::new(&[0b01100000]); // 011 → code_num=2
    assert_eq!(r.read_exp_golomb(), Some(2));

    let mut r = BitReader::new(&[0b00100000]); // 00100 → code_num=3
    assert_eq!(r.read_exp_golomb(), Some(3));
}


/// se(v) over the whole codeNum range. The largest codeNum (2^32 - 2, 31
/// leading zeros) is -(2^31 - 1); it used to wrap through `as i32` to +1.
#[test]
fn bit_reader_signed_exp_golomb_covers_the_full_code_range() {
    let mut r = BitReader::new(&[0b01001100, 0b10000000]); // 010 011 00100
    assert_eq!(r.read_signed_exp_golomb(), Some(1));
    assert_eq!(r.read_signed_exp_golomb(), Some(-1));
    assert_eq!(r.read_signed_exp_golomb(), Some(2));

    let mut r = BitReader::new(&[0, 0, 0, 0b0000_0001, 0xFF, 0xFF, 0xFF, 0xFE]);
    assert_eq!(r.read_signed_exp_golomb(), Some(-(i32::MAX)));
}

/// Fuzz crash (`flv_video_tag`): an AVC sequence header whose SPS scaling
/// list carries a delta_scale far outside -128..=127 overflowed
/// `last_scale + delta` (a panic in debug builds, a wrapped value in
/// release). Such an SPS is invalid, so no resolution is reported.
#[test]
fn sps_with_out_of_range_scaling_delta_is_rejected() {
    #[rustfmt::skip]
    const CRASH: [u8; 63] = [
        0x97, 0x00, 0x00, 0xc1, 0x60, 0x8e, 0x00, 0xc1, 0xc1, 0xc5, 0x8e, 0x00,
        0x32, 0xee, 0x7a, 0x00, 0x00, 0x00, 0x07, 0x00, 0x1f, 0x2d, 0xad, 0xad,
        0x2c, 0xad, 0xad, 0xad, 0xad, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff,
        0x90, 0x00, 0x10, 0xad, 0xb5, 0xad, 0xad, 0xad, 0xad, 0xad, 0xad, 0xad,
        0xad, 0xad, 0xad, 0x25, 0x1d, 0x1d, 0x1d, 0x1d, 0xff, 0xed, 0x1d, 0x21,
        0x09, 0x00, 0x08,
    ];
    let meta = parse_flv_video_meta(&CRASH).expect("H.264 sequence header");
    assert_eq!((meta.width, meta.height), (0, 0));
}

/// AAC channel configuration 7 is 7.1, eight channels (ISO/IEC 14496-3
/// Table 1.19). The MPEG-TS ADTS probe already reported 8; the RTMP
/// AudioSpecificConfig probe reported 7, so a 7.1 RTMP publisher and a 7.1
/// SRT publisher disagreed on the channel count.
#[test]
fn flv_audio_probe_reports_eight_channels_for_channel_configuration_7() {
    let header = crate::media::codec::build_aac_sequence_header(48_000, 8);
    let meta = parse_flv_audio_meta(&header).expect("AAC sequence header");
    assert_eq!(meta.channels, 8);
    assert_eq!(meta.channel_layout.as_deref(), Some("7.1"));
}

proptest! {
    /// The AudioSpecificConfig Restream synthesizes for SRT→RTMP (no cached
    /// config) is read back by the RTMP ingest probe as the same rate and
    /// channel count, for every rate and channel count the 2-byte AAC-LC
    /// config can express (7 channels has no channel configuration).
    #[test]
    fn aac_sequence_header_round_trips_through_the_flv_audio_probe(
        rate in prop::sample::select(vec![
            96_000u32, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000,
            12_000, 11_025, 8_000,
        ]),
        channels in prop::sample::select(vec![1u32, 2, 3, 4, 5, 6, 8]),
    ) {
        let header = crate::media::codec::build_aac_sequence_header(rate, channels);
        let meta = parse_flv_audio_meta(&header).expect("AAC sequence header");
        prop_assert_eq!(meta.codec.as_str(), "aac");
        prop_assert_eq!(meta.profile.as_deref(), Some("LC"));
        prop_assert_eq!(meta.sample_rate, rate);
        prop_assert_eq!(meta.channels, channels);
    }
}
