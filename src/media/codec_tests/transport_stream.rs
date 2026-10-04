
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

/// The ADTS length field is 13 bits and includes the header. The largest
/// frame it can describe reads back as exactly one frame; one byte more is
/// refused. It used to be written with a truncated length, so a TS demuxer
/// lost ADTS sync on the rest of the stream.
#[test]
fn adts_header_refuses_frames_its_length_field_cannot_hold() {
    let mut largest = Vec::from(build_adts_header(MAX_ADTS_PAYLOAD, 48_000, 2).unwrap());
    largest.resize(7 + MAX_ADTS_PAYLOAD, 0x11);
    assert_eq!(adts_frame_count(&largest), 1);
    assert!(build_adts_header(MAX_ADTS_PAYLOAD + 1, 48_000, 2).is_none());

    let mut flv = vec![0xAF, 0x01];
    flv.resize(2 + MAX_ADTS_PAYLOAD + 1, 0x22);
    assert!(audio_for_ts(&flv, PayloadFormat::Flv, 48_000, 2).is_none());
    let raw = vec![0x22; MAX_ADTS_PAYLOAD + 1];
    assert!(audio_for_ts(&raw, PayloadFormat::Raw, 48_000, 2).is_none());
    let mut buf = Vec::new();
    assert!(audio_for_ts_into(&raw, PayloadFormat::Raw, 48_000, 2, &mut buf).is_none());
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
fn video_for_ts_flv_too_short_payload() {
    let mut nls = 4;
    let mut cache = Vec::new();
    assert!(video_for_ts(&[0x17, 1, 0, 0], PayloadFormat::Flv, &mut nls, &mut cache).is_none());
}

#[test]
fn video_for_ts_flv_sequence_header_returns_none() {
    let mut nls = 4;
    let mut cache = Vec::new();
    // FLV video tag: frame_type=1(keyframe), codec=7(AVC), packet_type=0(seq hdr)
    // AVCC config: version=1, profile=66, compat=0, level=30, nalu_len=4, num_sps=0
    let flv_seq = [
        0x17, 0x00, 0x00, 0x00, 0x00, // FLV header
        0x01, 0x42, 0x00, 0x1E, 0xFF, 0x00, // AVCC config (8+ bytes)
        0x00, // num_pps=0
    ];
    assert!(video_for_ts(&flv_seq, PayloadFormat::Flv, &mut nls, &mut cache).is_none());
    // nal_size may be updated
    assert_eq!(nls, 4);
}

// Adversarial hunt: a reconnecting RTMP publisher can send a malformed or
// truncated sequence header (e.g. declares a SPS length but never
// supplies the SPS bytes). `parse_avcc_config` correctly fails closed and
// returns an empty Vec for this — but both `video_for_ts` and
// `video_for_ts_into` unconditionally assigned that empty result to
// `*sps_pps_cache`, wiping out a previously cached, still-valid set of
// parameter sets. Every keyframe muxed after the malformed header (until
// a valid one eventually arrives) would then be missing SPS/PPS and fail
// to decode.
#[test]
fn video_for_ts_flv_malformed_sequence_header_does_not_wipe_existing_cache() {
    let mut nls = 4;
    let mut cache = vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
        0xCE, 0x38, 0x80,
    ];
    let original_cache = cache.clone();
    let malformed_seq_hdr = [
        0x17, 0x00, 0x00, 0x00, 0x00, // FLV header: keyframe, seq header
        1, 66, 0, 30, 0xFF, // AVCC version/profile/compat/level, len_size=4
        0xE1, 0x00, 0x05, // num_sps=1, sps_len=5 — but no SPS bytes follow
    ];

    assert!(
        video_for_ts(&malformed_seq_hdr, PayloadFormat::Flv, &mut nls, &mut cache).is_none(),
        "a sequence header packet never emits a standalone TS payload"
    );
    assert_eq!(
        cache, original_cache,
        "a malformed sequence header must not destroy a previously cached, \
         still-valid set of parameter sets — the stream should keep \
         decoding with the last known-good SPS/PPS until a valid \
         replacement arrives"
    );
}

#[test]
fn video_for_ts_into_flv_malformed_sequence_header_does_not_wipe_existing_cache() {
    let mut nls = 4;
    let mut cache = vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
        0xCE, 0x38, 0x80,
    ];
    let original_cache = cache.clone();
    let mut buf = Vec::new();
    let malformed_seq_hdr = [
        0x17, 0x00, 0x00, 0x00, 0x00, // FLV header: keyframe, seq header
        1, 66, 0, 30, 0xFF, // AVCC version/profile/compat/level, len_size=4
        0xE1, 0x00, 0x05, // num_sps=1, sps_len=5 — but no SPS bytes follow
    ];

    assert!(
        video_for_ts_into(
            &malformed_seq_hdr,
            PayloadFormat::Flv,
            &mut nls,
            &mut cache,
            &mut buf
        )
        .is_none(),
        "a sequence header packet never emits a standalone TS payload"
    );
    assert_eq!(
        cache, original_cache,
        "zero-allocation variant must not destroy the cache either"
    );
}

#[test]
fn video_for_ts_raw_empty_returns_none() {
    let mut nls = 4;
    let mut cache = Vec::new();
    assert!(video_for_ts(&[], PayloadFormat::Raw, &mut nls, &mut cache).is_none());
}

#[test]
fn video_for_ts_raw_h264_keyframe_prepends_cached_parameter_sets() {
    let payload = [0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80];
    let mut nls = 4;
    let mut cache = vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
        0xCE, 0x38, 0x80,
    ];

    let result = video_for_ts(&payload, PayloadFormat::Raw, &mut nls, &mut cache)
        .expect("cached SPS/PPS should be prepended to raw keyframes");

    assert!(matches!(result, Cow::Owned(_)));
    assert!(result.starts_with(&cache));
    assert!(result.ends_with(&payload));
}

#[test]
fn video_for_ts_into_raw_h265_keyframe_prepends_cached_parameter_sets() {
    let payload = [0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xAA];
    let mut nls = 4;
    let mut cache = vec![
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
        0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
    ];
    let mut buf = Vec::new();

    let result =
        video_for_ts_into(&payload, PayloadFormat::Raw, &mut nls, &mut cache, &mut buf)
            .expect("cached VPS/SPS/PPS should be prepended to raw keyframes")
            .to_vec();

    assert_eq!(result, buf);
    assert!(result.starts_with(&cache));
    assert!(result.ends_with(&payload));
}

#[test]
fn video_for_ts_raw_inline_parameter_sets_refresh_cache_without_duplication() {
    let payload = [
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
        0xCE, 0x38, 0x80, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80,
    ];
    let mut nls = 4;
    let mut cache = Vec::new();

    let result = video_for_ts(&payload, PayloadFormat::Raw, &mut nls, &mut cache)
        .expect("inline SPS/PPS should still pass through");

    assert!(matches!(result, Cow::Borrowed(_)));
    assert_eq!(cache, payload[..17]);
    assert_eq!(&*result, &payload);
}

// Adversarial hunt: the "without duplication" guarantee above was only
// ever proven for 4-byte start codes. `annexb_nalu` always normalizes
// cached parameter sets to 4-byte start codes, but `refresh_annexb_parameter_set_cache`
// followed by `payload.starts_with(sps_pps_cache)` compares raw bytes —
// so a payload whose own inline SPS/PPS use 3-byte start codes (legal
// Annex B, common from many encoders) never matches the 4-byte-normalized
// cache and gets a second, redundant copy of SPS/PPS prepended to every
// keyframe.
#[test]
fn video_for_ts_raw_inline_parameter_sets_with_3byte_start_codes_not_duplicated() {
    let payload = [
        0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0xAB, // SPS (3-byte start code)
        0, 0, 1, 0x68, 0xCE, 0x38, 0x80, // PPS (3-byte start code)
        0, 0, 1, 0x65, 0x88, 0x80, // IDR (3-byte start code)
    ];
    let mut nls = 4;
    let mut cache = Vec::new();

    let result = video_for_ts(&payload, PayloadFormat::Raw, &mut nls, &mut cache)
        .expect("inline SPS/PPS with 3-byte start codes should still pass through");

    assert_eq!(
        &*result, &payload,
        "payload already carries its own inline SPS/PPS; a second, \
         4-byte-normalized copy must not be prepended just because the \
         cache's start-code length differs from the source's"
    );
}

#[test]
fn video_for_ts_into_raw_inline_parameter_sets_with_3byte_start_codes_not_duplicated() {
    let payload = [
        0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0xAB, // SPS (3-byte start code)
        0, 0, 1, 0x68, 0xCE, 0x38, 0x80, // PPS (3-byte start code)
        0, 0, 1, 0x65, 0x88, 0x80, // IDR (3-byte start code)
    ];
    let mut nls = 4;
    let mut cache = Vec::new();
    let mut buf = Vec::new();

    let result =
        video_for_ts_into(&payload, PayloadFormat::Raw, &mut nls, &mut cache, &mut buf)
            .expect("inline SPS/PPS with 3-byte start codes should still pass through");

    assert_eq!(
        result, &payload,
        "zero-allocation variant must not duplicate parameter sets either"
    );
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
fn audio_for_ts_flv_config_packet_returns_none() {
    // packet_type 0 (AAC sequence header) — should be dropped
    assert!(audio_for_ts(&[0xAF, 0x00, 0x12, 0x10], PayloadFormat::Flv, 48000, 2).is_none());
}

#[test]
fn audio_for_ts_flv_too_short_returns_none() {
    assert!(audio_for_ts(&[0xAF], PayloadFormat::Flv, 48000, 2).is_none());
    assert!(audio_for_ts(&[0xAF, 0x01], PayloadFormat::Flv, 48000, 2).is_none());
}

#[test]
fn video_for_ts_into_reuses_buffer() {
    let mut nls = 4;
    let mut sps_pps = Vec::new();
    let mut buf = vec![0xDE, 0xAD]; // pre-existing content
    // Raw passthrough — should not touch buf
    let result = video_for_ts_into(
        &[0, 0, 0, 1, 0x41, 0xBB],
        PayloadFormat::Raw,
        &mut nls,
        &mut sps_pps,
        &mut buf,
    );
    assert!(result.is_some());
    // buf is not cleared for Raw
    assert!(!buf.is_empty());
}

#[test]
fn audio_for_ts_into_reuses_buffer() {
    let mut buf = vec![0xDE];
    let result = audio_for_ts_into(
        &[0xAF, 0x01, 0xDE, 0xAD],
        PayloadFormat::Flv,
        48000,
        2,
        &mut buf,
    );
    assert!(result.is_some());
    // buf was cleared and repopulated with ADTS + raw AAC
    assert!(has_adts_sync(&buf));
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

#[test]
fn audio_for_rtmp_no_adts_passthrough() {
    let raw = [0xDE, 0xAD, 0xBE, 0xEF];
    let result = audio_for_rtmp(&raw);
    assert_eq!(result[0], 0xAF);
    assert_eq!(result[1], 0x01);
    assert_eq!(&result[2..], &raw);
}
