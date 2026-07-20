#[test]
fn avcc_annexb_round_trip() {
    // SPS (type 7) + PPS (type 8) + IDR (type 5) as Annex B
    let annexb = [
        0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0xAB, // SPS
        0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80, // PPS
        0, 0, 0, 1, 0x65, 0x88, 0x80, 0x40, // IDR slice
    ];

    // annexb_to_avcc should filter SPS/PPS/AUD and keep only IDR
    let avcc = annexb_to_avcc(&annexb);
    assert!(!avcc.is_empty());
    // First 4 bytes = length of the IDR NALU
    let nalu_len = u32::from_be_bytes([avcc[0], avcc[1], avcc[2], avcc[3]]) as usize;
    assert_eq!(nalu_len, 4); // IDR data: 0x65 0x88 0x80 0x40
    assert_eq!(avcc[4] & 0x1F, 5); // IDR NAL type

    // Convert back
    let back = avcc_to_annexb(&avcc, 4);
    assert_eq!(&back[..4], &[0, 0, 0, 1]); // start code
    assert_eq!(back[4] & 0x1F, 5); // IDR
}

#[test]
fn parse_avcc_config_extracts_sps_pps() {
    // Minimal AVCC config: version=1, profile=66, compat=0, level=30, len_size=4
    let mut config = vec![
        1, 66, 0, 30, 0xFF, // lengthSizeMinusOne = 3 → 4 bytes
    ];
    // 1 SPS
    let sps = [0x67, 0x42, 0x00, 0x1E];
    config.push(0xE1); // num_sps = 1
    config.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    config.extend_from_slice(&sps);
    // 1 PPS
    let pps = [0x68, 0xCE, 0x38, 0x80];
    config.push(1); // num_pps = 1
    config.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    config.extend_from_slice(&pps);

    let (nls, annexb) = parse_avcc_config(&config);
    assert_eq!(nls, 4);
    // Should contain start_code + SPS + start_code + PPS
    assert!(annexb.len() > 8);
    assert_eq!(&annexb[..4], &[0, 0, 0, 1]);
    assert_eq!(annexb[4], 0x67); // SPS NAL type
}

#[test]
fn adts_round_trip() {
    let raw_aac = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02];
    let with_adts = prepend_adts(&raw_aac, 48000, 2);
    assert_eq!(with_adts.len(), 7 + raw_aac.len());
    assert!(has_adts_sync(&with_adts));
    let stripped = strip_adts(&with_adts);
    assert_eq!(stripped, &raw_aac[..]);
}

#[test]
fn adts_frame_count_counts_complete_frames() {
    let mut payload = Vec::new();
    let frame_a = build_adts_header(2, 48000, 2);
    payload.extend_from_slice(&frame_a);
    payload.extend_from_slice(&[0x11, 0x22]);
    let frame_b = build_adts_header(3, 48000, 2);
    payload.extend_from_slice(&frame_b);
    payload.extend_from_slice(&[0x33, 0x44, 0x55]);

    assert_eq!(adts_frame_count(&payload), 2);
    payload.pop();
    assert_eq!(
        adts_frame_count(&payload),
        1,
        "truncated trailing frames must not be counted"
    );
}

proptest! {
    #[test]
    fn adts_frame_count_matches_generated_complete_frames(
        frame_sizes in proptest::collection::vec(1usize..64, 0..12),
        truncate_tail in 0usize..8,
    ) {
        let mut payload = Vec::new();
        for (idx, frame_size) in frame_sizes.iter().copied().enumerate() {
            let frame = build_adts_header(frame_size, 48000, 2);
            payload.extend_from_slice(&frame);
            payload.extend(std::iter::repeat_n(idx as u8, frame_size));
        }
        let mut expected = frame_sizes.len();
        if truncate_tail > 0 && !payload.is_empty() {
            let remove = truncate_tail.min(payload.len());
            payload.truncate(payload.len() - remove);
            expected = expected.saturating_sub(1);
        }
        prop_assert_eq!(adts_frame_count(&payload), expected);
    }
}

#[test]
fn video_for_ts_flv_passthrough_raw() {
    let annexb_payload = vec![0, 0, 0, 1, 0x65, 0x88];
    let mut nls = 4;
    let mut cache = Vec::new();
    let result = video_for_ts(&annexb_payload, PayloadFormat::Raw, &mut nls, &mut cache);
    assert!(result.is_some());
    // Raw should be zero-copy
    assert!(matches!(result, Some(Cow::Borrowed(_))));
    assert_eq!(&*result.unwrap(), &annexb_payload[..]);
}

#[test]
fn audio_for_ts_adds_adts_for_raw_without() {
    let raw_aac = vec![0xDE, 0xAD];
    let result = audio_for_ts(&raw_aac, PayloadFormat::Raw, 48000, 2);
    assert!(result.is_some());
    let data = result.unwrap();
    assert!(has_adts_sync(&data));
    assert_eq!(&data[7..], &raw_aac[..]);
}

#[test]
fn audio_for_ts_passes_through_existing_adts() {
    let mut with_adts = Vec::from(build_adts_header(4, 48000, 2));
    with_adts.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
    let result = audio_for_ts(&with_adts, PayloadFormat::Raw, 48000, 2);
    assert!(matches!(result, Some(Cow::Borrowed(_))));
}

#[test]
fn build_avcc_seq_header_from_annexb() {
    let annexb = [
        0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1E, 0xAB, // SPS
        0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80, // PPS
        0, 0, 0, 1, 0x65, 0x88, 0x80, 0x40, // IDR
    ];
    let seq_hdr = build_avcc_sequence_header(&annexb).unwrap();
    // FLV tag: keyframe + seq header
    assert_eq!(seq_hdr[0], 0x17);
    assert_eq!(seq_hdr[1], 0x00);
    // AVCC config version
    assert_eq!(seq_hdr[5], 1);
}

#[test]
fn video_for_rtmp_converts_annexb() {
    let annexb = [0, 0, 0, 1, 0x65, 0x88, 0x80, 0x40];
    let result = video_for_rtmp(&annexb, true).unwrap();
    assert_eq!(result[0], 0x17); // keyframe tag
    assert_eq!(result[1], 1); // data packet
    // AVCC data starts at offset 5
    let nalu_len = u32::from_be_bytes([result[5], result[6], result[7], result[8]]) as usize;
    assert_eq!(nalu_len, 4);
}

#[test]
fn video_for_rtmp_preserves_positive_composition_time() {
    let annexb = [0, 0, 0, 1, 0x65, 0x88, 0x80, 0x40];
    let mut out = Vec::new();

    assert!(video_for_rtmp_with_composition_into(
        &annexb, true, 40, &mut out
    ));

    assert_eq!(&out[..5], &[0x17, 0x01, 0x00, 0x00, 0x28]);
}

#[test]
fn video_for_rtmp_preserves_negative_composition_time() {
    let annexb = [0, 0, 0, 1, 0x41, 0x88, 0x80, 0x40];
    let mut out = Vec::new();

    assert!(video_for_rtmp_with_composition_into(
        &annexb, false, -40, &mut out
    ));

    assert_eq!(&out[..5], &[0x27, 0x01, 0xff, 0xff, 0xd8]);
}

#[test]
fn hevc_video_for_enhanced_rtmp_uses_coded_frames_x_for_zero_composition() {
    let annexb = [
        0, 0, 0, 1, 0x40, 0x01, 0xAA, 0, 0, 0, 1, 0x42, 0x01, 0x01, 0x01, 0x60, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0x78, 0, 0, 0, 1, 0x44, 0x01, 0xCC, 0, 0, 0, 1, 0x26, 0x01, 0xDE, 0xAD,
    ];
    let mut out = Vec::new();

    assert!(hevc_video_for_enhanced_rtmp_with_composition_into(
        &annexb, true, 0, &mut out
    ));

    assert_eq!(&out[..5], &[0x93, b'h', b'v', b'c', b'1']);
    let nalu_len = u32::from_be_bytes([out[5], out[6], out[7], out[8]]);
    assert_eq!(nalu_len, 4);
    assert_eq!(&out[9..], &[0x26, 0x01, 0xDE, 0xAD]);
}

#[test]
fn hevc_video_for_enhanced_rtmp_writes_composition_for_nonzero_offset() {
    let annexb = [0, 0, 0, 1, 0x26, 0x01, 0xDE, 0xAD];
    let mut out = Vec::new();

    assert!(hevc_video_for_enhanced_rtmp_with_composition_into(
        &annexb, true, 40, &mut out
    ));

    assert_eq!(&out[..8], &[0x91, b'h', b'v', b'c', b'1', 0, 0, 40]);
    let nalu_len = u32::from_be_bytes([out[8], out[9], out[10], out[11]]);
    assert_eq!(nalu_len, 4);
    assert_eq!(&out[12..], &[0x26, 0x01, 0xDE, 0xAD]);
}

#[test]
fn hevc_enhanced_rtmp_sequence_header_uses_hvc1_fourcc() {
    let sps = minimal_hevc_sps_nalu(1, 2);
    let mut annexb = vec![0, 0, 0, 1, 0x40, 0x01, 0xAA];
    annexb.extend_from_slice(&[0, 0, 0, 1]);
    annexb.extend_from_slice(&sps);
    annexb.extend_from_slice(&[0, 0, 0, 1, 0x44, 0x01, 0xCC]);

    let seq_hdr = build_hevc_enhanced_rtmp_sequence_header(&annexb).unwrap();

    assert_eq!(&seq_hdr[..5], &[0x80, b'h', b'v', b'c', b'1']);
    assert_eq!(seq_hdr[5], 1);
    assert_eq!(seq_hdr[6] & 0x1f, 2);
    assert_eq!(seq_hdr[17], 0x7b);
    assert_eq!(seq_hdr[21], 0xfd);
    assert_eq!(seq_hdr[22], 0xfa);
    assert_eq!(seq_hdr[23], 0xfa);
}

#[test]
fn hevc_enhanced_rtmp_sequence_header_builds_from_bf0_fixture() {
    let fixture = crate::test_fixtures::av_marker_transport_fixture_for_bframes(
        "h265",
        false,
        crate::test_fixtures::AvMarkerBframeMode::Bf0,
    )
    .expect("checked-in HEVC BF0 fixture");
    let bytes = std::fs::read(fixture).expect("read HEVC BF0 fixture");
    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in bytes.chunks(1316) {
        demuxer.feed(chunk);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);
    let first_video_prefix = packets
        .iter()
        .find(|packet| packet.media_type == crate::media::packet::MediaType::Video)
        .map(|packet| {
            packet
                .payload
                .iter()
                .take(16)
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| "none".to_string());
    let parameter_sets = packets
        .iter()
        .find_map(|packet| {
            (packet.media_type == crate::media::packet::MediaType::Video)
                .then(|| annexb_parameter_sets(&packet.payload))
                .flatten()
        })
        .unwrap_or_else(|| {
            panic!(
                "fixture should carry HEVC parameter sets; packets={} first_video={}",
                packets.len(),
                first_video_prefix
            )
        });

    let seq_hdr = build_hevc_enhanced_rtmp_sequence_header(&parameter_sets)
        .expect("fixture HEVC parameter sets should build Enhanced RTMP hvcC");

    assert_eq!(&seq_hdr[..5], &[0x80, b'h', b'v', b'c', b'1']);
    assert_eq!(seq_hdr[21], 0xfd);
}

#[test]
fn video_for_rtmp_rejects_non_vcl_annexb_payload() {
    let sei_only = [0, 0, 0, 1, 0x06, 0x05, 0xff, 0xff];
    let mut out = Vec::new();

    assert!(!video_for_rtmp_with_composition_into(
        &sei_only, true, 0, &mut out
    ));
    assert_eq!(&out[..5], &[0x17, 0x01, 0x00, 0x00, 0x00]);
}

#[test]
fn build_aac_seq_header_synthesizes_correct_config() {
    // AAC-LC (audioObjectType=2), 48000Hz (freq_idx=3), stereo (ch=2)
    // asc_byte0 = (2 << 3) | (3 >> 1) = 16 | 1 = 0x11
    // asc_byte1 = ((3 & 1) << 7) | (2 << 3) = 128 | 16 = 0x90
    let hdr = build_aac_sequence_header(48000, 2);
    assert_eq!(hdr.len(), 4);
    assert_eq!(hdr[0], 0xAF); // AAC, 44kHz, 16-bit, stereo
    assert_eq!(hdr[1], 0x00); // packet_type = 0 (sequence header)
    assert_eq!(hdr[2], 0x11);
    assert_eq!(hdr[3], 0x90);

    // AAC-LC, 44100Hz (freq_idx=4), mono (ch=1)
    // asc_byte0 = (2 << 3) | (4 >> 1) = 16 | 2 = 0x12
    // asc_byte1 = ((4 & 1) << 7) | (1 << 3) = 0 | 8 = 0x08
    let hdr2 = build_aac_sequence_header(44100, 1);
    assert_eq!(hdr2.len(), 4);
    assert_eq!(hdr2[0], 0xAF);
    assert_eq!(hdr2[1], 0x00);
    assert_eq!(hdr2[2], 0x12);
    assert_eq!(hdr2[3], 0x08);
}

#[test]
fn audio_for_rtmp_strips_adts() {
    let raw = [0xDE, 0xAD, 0xBE, 0xEF];
    let mut with_adts = Vec::from(build_adts_header(raw.len(), 48000, 2));
    with_adts.extend_from_slice(&raw);

    let result = audio_for_rtmp(&with_adts);
    assert_eq!(result[0], 0xAF);
    assert_eq!(result[1], 0x01);
    assert_eq!(&result[2..], &raw);
}
