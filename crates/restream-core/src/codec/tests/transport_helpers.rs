// Pure framing helpers used by transport adapters.

#[test]
fn build_adts_header_all_sample_rates() {
    let rates = [
        (96000, 0),
        (88200, 1),
        (64000, 2),
        (48000, 3),
        (44100, 4),
        (32000, 5),
        (24000, 6),
        (22050, 7),
        (16000, 8),
        (12000, 9),
        (11025, 10),
        (8000, 11),
    ];
    for (rate, expected_freq_idx) in rates {
        let hdr = build_adts_header(100, rate, 2).unwrap();
        let actual = (hdr[2] >> 2) & 0x0F;
        assert_eq!(
            actual, expected_freq_idx,
            "ADTS freq index mismatch for {rate}Hz"
        );
    }
}

/// A header whose length field is below its own 7 bytes ends the count. A
/// zero length would otherwise never advance past the frame.
#[test]
fn adts_frame_count_stops_at_a_length_shorter_than_the_header() {
    let mut payload = Vec::from(build_adts_header(2, 48_000, 2).unwrap());
    payload.extend_from_slice(&[0x11, 0x22]);
    for claimed in [6u8, 0] {
        let mut bad = payload.clone();
        let mut header = build_adts_header(2, 48_000, 2).unwrap();
        header[3] &= 0xFC;
        header[4] = 0;
        header[5] = (claimed << 5) | 0x1F;
        bad.extend_from_slice(&header);
        bad.extend_from_slice(&[0; 8]);
        assert_eq!(adts_frame_count(&bad), 1, "claimed length {claimed}");
    }
}

#[test]
fn build_adts_header_unknown_rate_defaults_to_48k() {
    let hdr = build_adts_header(100, 99999, 2).unwrap();
    assert_eq!((hdr[2] >> 2) & 0x0F, 3); // defaults to 48000
}

#[test]
fn build_adts_header_channels_clamped_to_7() {
    let hdr = build_adts_header(100, 48000, 8).unwrap();
    assert_eq!((hdr[2] & 0x01) << 2 | (hdr[3] >> 6), 7);
}

#[test]
fn strip_adts_crc_variant() {
    let raw = [0xDE, 0xAD, 0xBE];
    // ADTS with CRC: bit 0 of byte 1 = 0 → 9-byte header
    let mut adts = vec![0xFF, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    adts.extend_from_slice(&raw);
    let stripped = strip_adts(&adts);
    assert_eq!(stripped, &raw[..]);
}

#[test]
fn annexb_parameter_sets_rejects_partial_h264_parameter_sets() {
    let payload = [
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x65,
        0x88, 0x80,
    ];

    assert!(
        annexb_parameter_sets(&payload).is_none(),
        "partial H.264 parameter sets should not be cached"
    );
}

#[test]
fn annexb_parameter_sets_rejects_partial_h265_parameter_sets() {
    let payload = [
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
        0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xDD,
    ];

    assert!(
        annexb_parameter_sets(&payload).is_none(),
        "partial HEVC parameter sets should not be cached"
    );
}

#[test]
fn annexb_parameter_sets_accepts_complete_h265_parameter_sets() {
    let payload = [
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
        0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC, 0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xDD,
    ];

    let parameter_sets = annexb_parameter_sets(&payload).expect("complete HEVC headers");
    assert_eq!(
        parameter_sets,
        vec![
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
            0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC
        ]
    );
}

#[test]
fn h264_reference_slices_are_not_taken_for_h265_parameter_sets() {
    // 0x41 / 0x43 / 0x45 are H.264 non-IDR slices with nal_ref_idc = 2; read
    // as an H.265 header they are types 32..=34 (VPS/SPS/PPS). A long-lived
    // accumulator that sees one must still take the next H.264 keyframe's
    // SPS/PPS.
    let keyframe = |sps: u8| {
        vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, sps, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
            0xEE, 0x3C, 0x80, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80,
        ]
    };
    let mut accumulator = AnnexbParameterSetAccumulator::default();
    assert!(accumulator.push_payload(&keyframe(0x1E)).is_some());
    for header in [0x41, 0x43, 0x45] {
        let p_slice = [0x00, 0x00, 0x00, 0x01, header, 0x01, 0x9A, 0x02, 0x03];
        assert!(
            accumulator.push_payload(&p_slice).is_none_or(|sets| sets[4] == 0x67),
            "slice header {header:#04x} must not produce H.265 parameter sets"
        );
    }
    let refreshed = accumulator
        .push_payload(&keyframe(0x28))
        .expect("the next H.264 keyframe still yields SPS/PPS");
    assert_eq!(refreshed[7], 0x28, "the new SPS is taken");
}
#[test]
fn find_annexb_start_codes_no_match_returns_empty() {
    // No 00 00 01 pattern
    let data = [0x41, 0x42, 0x43, 0x44, 0x45];
    assert!(find_annexb_start_codes(&data).is_empty());
}

#[test]
fn split_annexb_nalus_empty_input() {
    assert!(split_annexb_nalus(&[]).is_empty());
}

#[test]
fn split_annexb_nalus_no_start_code_returns_empty() {
    assert!(split_annexb_nalus(&[0x41, 0x42, 0x43]).is_empty());
}

#[test]
fn build_avcc_sequence_header_insufficient_sps() {
    let annexb = [0, 0, 0, 1, 0x67, 0x42]; // SPS with only 2 bytes (need 4+)
    assert!(build_avcc_sequence_header(&annexb).is_none());
}

#[test]
fn build_avcc_sequence_header_no_pps_still_works() {
    // Only SPS, no PPS — should still produce output
    let annexb = [0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0xAB];
    let hdr = build_avcc_sequence_header(&annexb);
    assert!(hdr.is_some());
}

#[test]
fn video_for_rtmp_into_reuses_buffer() {
    let annexb = [0, 0, 0, 1, 0x65, 0x88, 0x80, 0x40];
    let mut buf = vec![0xDE, 0xAD]; // pre-existing
    assert!(video_for_rtmp_into(&annexb, true, &mut buf));
    // buf was cleared, FLV header + AVCC written
    assert_eq!(buf[0], 0x17);
    assert_eq!(buf[1], 1);
}

#[test]
fn audio_for_rtmp_into_reuses_buffer() {
    let raw = [0xDE, 0xAD, 0xBE, 0xEF];
    let mut buf = vec![0x01, 0x02]; // pre-existing
    audio_for_rtmp_into(&raw, &mut buf);
    // buf was cleared, FLV header + raw AAC written
    assert_eq!(buf[0], 0xAF);
    assert_eq!(buf[1], 0x01);
    assert_eq!(&buf[2..], &raw);
}

/// The ADTS length field is 13 bits and includes the header. The largest
/// frame it can describe reads back as exactly one frame; one byte more is
/// refused: written with a truncated length, it would make a TS demuxer lose
/// ADTS sync on the rest of the stream.
#[test]
fn adts_header_refuses_frames_its_length_field_cannot_hold() {
    let mut largest = Vec::from(build_adts_header(MAX_ADTS_PAYLOAD, 48_000, 2).unwrap());
    largest.resize(7 + MAX_ADTS_PAYLOAD, 0x11);
    assert_eq!(adts_frame_count(&largest), 1);
    assert!(build_adts_header(MAX_ADTS_PAYLOAD + 1, 48_000, 2).is_none());
}
