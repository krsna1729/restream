#[test]
fn audio_for_rtmp_no_adts_passthrough() {
    let raw = [0xDE, 0xAD, 0xBE, 0xEF];
    let result = audio_for_rtmp(&raw);
    assert_eq!(result[0], 0xAF);
    assert_eq!(result[1], 0x01);
    assert_eq!(&result[2..], &raw);
}

#[test]
fn audio_for_ts_refuses_frames_the_adts_length_field_cannot_hold() {
    let mut flv = vec![0xAF, 0x01];
    flv.resize(2 + MAX_ADTS_PAYLOAD + 1, 0x22);
    assert!(audio_for_ts(&flv, PayloadFormat::Flv, 48_000, 2).is_none());
    let raw = vec![0x22; MAX_ADTS_PAYLOAD + 1];
    assert!(audio_for_ts(&raw, PayloadFormat::Raw, 48_000, 2).is_none());
    let mut buf = Vec::new();
    assert!(audio_for_ts_into(&raw, PayloadFormat::Raw, 48_000, 2, &mut buf).is_none());
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


/// The checked-in H.265 clip's first keyframe and a later frame with a
/// composition offset, as enhanced RTMP tags made by the egress converter.
fn enhanced_hevc_fixture_tags() -> (bytes::Bytes, Vec<u8>, Vec<u8>, Vec<u8>) {
    use restream_core::MediaType;
    let (_, _, packets) = crate::media::test_fixtures::primary_av_packets_for_codec("h265").unwrap();
    let mut video = packets.iter().filter(|p| p.media_type == MediaType::Video);
    let keyframe = video.next().filter(|p| p.is_keyframe).unwrap();
    let header = build_hevc_enhanced_rtmp_sequence_header(&keyframe.payload).unwrap();
    let mut key_tag = Vec::new();
    assert!(hevc_video_for_enhanced_rtmp_with_composition_into(
        &keyframe.payload,
        true,
        0,
        &mut key_tag
    ));
    let next = video.next().unwrap();
    let mut next_tag = Vec::new();
    assert!(hevc_video_for_enhanced_rtmp_with_composition_into(
        &next.payload,
        false,
        -40,
        &mut next_tag
    ));
    (header, key_tag, next_tag, keyframe.payload.to_vec())
}

#[test]
fn an_enhanced_rtmp_hevc_tag_converts_to_annexb_led_by_its_parameter_sets() {
    let (header, key_tag, next_tag, keyframe) = enhanced_hevc_fixture_tags();
    let (mut nls, mut cache, mut buf) = (4, Vec::new(), Vec::new());
    assert!(video_for_ts_into(&header, PayloadFormat::Flv, &mut nls, &mut cache, &mut buf).is_none());
    let sets = annexb_parameter_sets(&keyframe).unwrap();
    assert_eq!(cache, sets, "VPS, SPS and PPS from the hvcC record");

    let annexb = video_for_ts_into(&key_tag, PayloadFormat::Flv, &mut nls, &mut cache, &mut buf)
        .unwrap()
        .to_vec();
    assert!(annexb.starts_with(&sets), "a keyframe is led by its parameter sets");
    let slices = |data: &[u8]| -> Vec<Vec<u8>> {
        split_annexb_nalus(data)
            .into_iter()
            .filter(|nalu| nalu.first().is_some_and(|b| (b >> 1) & 0x3F < 32))
            .map(<[u8]>::to_vec)
            .collect()
    };
    assert_eq!(slices(&annexb), slices(&keyframe), "the picture's slices come through whole");

    // A coded frame with a composition time (packet type 1) skips the 3 CTS
    // bytes and is not led by parameter sets.
    let next = video_for_ts_into(&next_tag, PayloadFormat::Flv, &mut nls, &mut cache, &mut buf)
        .unwrap();
    assert!(!next.starts_with(&sets));
    assert!(!slices(next).is_empty());
}

#[test]
fn a_truncated_enhanced_sequence_start_keeps_the_cache() {
    let (header, _, _, _) = enhanced_hevc_fixture_tags();
    let (mut nls, mut cache, mut buf) = (4, vec![0, 0, 0, 1, 0x40], Vec::new());
    for cut in [6, 20, 28, header.len() - 1] {
        let _ = video_for_ts_into(&header[..cut], PayloadFormat::Flv, &mut nls, &mut cache, &mut buf);
        assert_eq!(cache, [0, 0, 0, 1, 0x40], "cut at {cut}");
        assert!(restream_rtmp::flv::flv_video_config(&header[..cut]).is_none());
    }
}

proptest! {
    #[test]
    fn enhanced_tag_parsing_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..96)) {
        let mut tag = vec![0x80 | (bytes.first().copied().unwrap_or(0) & 0x7F)];
        tag.extend_from_slice(b"hvc1");
        tag.extend_from_slice(&bytes);
        let (mut nls, mut cache, mut buf) = (4, Vec::new(), Vec::new());
        let _ = video_for_ts_into(&tag, PayloadFormat::Flv, &mut nls, &mut cache, &mut buf);
        let _ = restream_rtmp::flv::flv_video_config(&tag);
        let _ = ex_video_tag(&tag);
    }
}
