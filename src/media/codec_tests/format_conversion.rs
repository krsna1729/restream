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
    let mut with_adts = Vec::from(build_adts_header(4, 48000, 2).unwrap());
    with_adts.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
    let result = audio_for_ts(&with_adts, PayloadFormat::Raw, 48000, 2);
    assert!(matches!(result, Some(Cow::Borrowed(_))));
}

#[test]
fn hevc_enhanced_rtmp_sequence_header_builds_from_bf0_fixture() {
    let fixture = crate::media::test_fixtures::av_marker_transport_fixture_for_bframes(
        "h265",
        false,
        crate::media::test_fixtures::AvMarkerBframeMode::Bf0,
    )
    .expect("checked-in HEVC BF0 fixture");
    let bytes = std::fs::read(fixture).expect("read HEVC BF0 fixture");
    let mut demuxer = restream_mpegts::TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in bytes.chunks(1316) {
        demuxer.feed(chunk);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);
    let first_video_prefix = packets
        .iter()
        .find(|packet| packet.media_type == restream_core::MediaType::Video)
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
            (packet.media_type == restream_core::MediaType::Video)
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
fn audio_for_rtmp_strips_adts() {
    let raw = [0xDE, 0xAD, 0xBE, 0xEF];
    let mut with_adts = Vec::from(build_adts_header(raw.len(), 48000, 2).unwrap());
    with_adts.extend_from_slice(&raw);

    let result = audio_for_rtmp(&with_adts);
    assert_eq!(result[0], 0xAF);
    assert_eq!(result[1], 0x01);
    assert_eq!(&result[2..], &raw);
}
