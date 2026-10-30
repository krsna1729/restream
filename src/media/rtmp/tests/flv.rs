
#[test]
fn an_enhanced_rtmp_hevc_publish_is_probed_classified_and_timed() {
    use restream_core::codec;
    let (probe, _, packets) = crate::media::test_fixtures::primary_av_packets_for_codec("h265").unwrap();
    let keyframe = packets
        .iter()
        .find(|p| p.media_type == MediaType::Video && p.is_keyframe)
        .unwrap();
    let header = codec::build_hevc_enhanced_rtmp_sequence_header(&keyframe.payload).unwrap();

    let meta = parse_flv_video_meta(&header).unwrap();
    assert_eq!(meta.codec, "h265");
    assert_eq!((meta.width, meta.height), (probe.width, probe.height));
    assert_eq!(
        classify_flv_video_packet(&header),
        Some(FlvVideoPacketKind::SequenceHeader)
    );
    assert_eq!(
        flv_config_annexb_parameter_sets(&header),
        codec::annexb_parameter_sets(&keyframe.payload)
    );

    let mut tag = Vec::new();
    codec::hevc_video_for_enhanced_rtmp_with_composition_into(&keyframe.payload, true, 80, &mut tag);
    assert_eq!(
        classify_flv_video_packet(&tag),
        Some(FlvVideoPacketKind::Keyframe)
    );
    assert_eq!(flv_video_composition_time_ms(&tag), 80);
    codec::hevc_video_for_enhanced_rtmp_with_composition_into(&keyframe.payload, false, 0, &mut tag);
    assert_eq!(
        classify_flv_video_packet(&tag),
        Some(FlvVideoPacketKind::Interframe)
    );
    assert_eq!(flv_video_composition_time_ms(&tag), 0);
    // Sequence end carries no picture.
    assert_eq!(classify_flv_video_packet(b"\x92hvc1"), None);
}
