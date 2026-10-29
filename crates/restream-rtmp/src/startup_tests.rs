use crate::enhanced::raw_packet_starts_with_hevc_parameter_set;
use crate::*;
use bytes::Bytes;
use proptest::prelude::*;
use restream_core::metadata::AudioMeta;
use restream_sync::RingBuffer;
use rml_rtmp::chunk_io::ChunkDeserializer;
use rml_rtmp::messages::RtmpMessage;
use rml_rtmp::rml_amf0::Amf0Value;
use rml_rtmp::sessions::ClientSessionConfig;
use std::sync::Arc;
#[test]
fn classify_flv_video_packet_distinguishes_config_and_media() {
    assert_eq!(
        classify_flv_video_packet(&[0x17, 0x00, 0x00, 0x00, 0x00]),
        Some(FlvVideoPacketKind::SequenceHeader)
    );
    assert_eq!(
        classify_flv_video_packet(&[0x17, 0x01, 0x00, 0x00, 0x28]),
        Some(FlvVideoPacketKind::Keyframe)
    );
    assert_eq!(
        classify_flv_video_packet(&[0x27, 0x01, 0x00, 0x00, 0x28]),
        Some(FlvVideoPacketKind::Interframe)
    );
    assert_eq!(classify_flv_video_packet(&[0x12, 0x01]), None);
}

#[test]
fn rtmp_video_droppability_matches_rml_contract() {
    assert!(
        !rtmp_video_packet_can_be_dropped(&[0x17, 0x00, 0x00, 0x00, 0x00], true),
        "AVC sequence headers must never be marked droppable"
    );
    assert!(
        !rtmp_video_packet_can_be_dropped(&[0x17, 0x01, 0x00, 0x00, 0x28], true),
        "keyframes must not be marked droppable because later frames depend on them"
    );
    assert!(
        rtmp_video_packet_can_be_dropped(&[0x27, 0x01, 0x00, 0x00, 0x28], false),
        "interframes may be marked droppable for future slow-consumer policies"
    );
    assert!(
        !rtmp_video_packet_can_be_dropped(&[0x27, 0x01, 0x00, 0x00, 0x28], true),
        "packet metadata wins over FLV tag classification when it says keyframe"
    );
    assert!(
        !rtmp_video_packet_can_be_dropped(&[0x12, 0x01], false),
        "unclassified video payloads must fail closed until a future drop policy can prove safety"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn proptest_rtmp_video_droppability_fails_closed(
        mut payload in proptest::collection::vec(any::<u8>(), 0..32),
        is_keyframe in any::<bool>(),
    ) {
        let expected = !is_keyframe
            && payload.len() >= 2
            && matches!(payload[0] & 0x0f, 7 | 12)
            && payload[1] != 0
            && (payload[0] >> 4) != 1;

        prop_assert_eq!(rtmp_video_packet_can_be_dropped(&payload, is_keyframe), expected);

        if !payload.is_empty() {
            payload[0] = (payload[0] & 0xf0) | 0x02;
            prop_assert!(
                !rtmp_video_packet_can_be_dropped(&payload, false),
                "unknown FLV video codecs must not be marked droppable"
            );
        }
    }
}

#[test]
fn startup_video_sequence_header_prefers_ring_parameter_sets() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");
    ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68, 0xCE,
        0x38, 0x80,
    ]);
    let ingest_sequence_header =
        Bytes::from_static(&[0x17, 0x00, 0x00, 0x00, 0x00, 0x01, 0x64, 0x00, 0x1F]);

    let (selected, generation) =
        startup_video_sequence_header(&ring, Some(ingest_sequence_header.clone()), false)
            .expect("ring parameter sets should synthesize a startup header");

    assert_ne!(
        selected, ingest_sequence_header,
        "startup header should come from the output ring, not the ingest cache"
    );
    assert_eq!(selected[0], 0x17);
    assert_eq!(selected[1], 0x00);
    assert_eq!(
        Some(generation),
        ring.video_config().map(|config| config.generation),
        "the header announces the ring configuration it was built from"
    );
}

#[test]
fn raw_hevc_guard_does_not_flag_h264_non_idr_slice() {
    let h264_non_idr = [0, 0, 0, 1, 0x41, 0x9a, 0x22, 0x11];

    assert!(!raw_packet_starts_with_hevc_parameter_set(&h264_non_idr));
}

#[test]
fn enhanced_rtmp_connect_packet_advertises_hevc_fourcc_support() {
    let mut config = ClientSessionConfig::new();
    config.tc_url = Some("rtmp://example/live".to_string());

    let packet = enhanced_rtmp_connect_packet(&config, "live").unwrap();
    let mut deserializer = ChunkDeserializer::new();
    let payload = deserializer
        .get_next_message(&packet)
        .unwrap()
        .expect("connect packet should contain one RTMP message");
    let message = payload.to_rtmp_message().unwrap();

    let RtmpMessage::Amf0Command {
        command_name,
        transaction_id,
        command_object,
        ..
    } = message
    else {
        panic!("enhanced connect should be an AMF0 command");
    };
    assert_eq!(command_name, "connect");
    assert_eq!(transaction_id, 1.0);
    let Amf0Value::Object(properties) = command_object else {
        panic!("enhanced connect should use an object command payload");
    };
    assert_eq!(
        properties.get("fourCcList"),
        Some(&Amf0Value::StrictArray(vec![
            Amf0Value::Utf8String("hvc1".to_string()),
            Amf0Value::Utf8String("avc1".to_string()),
            Amf0Value::Utf8String("mp4a".to_string()),
        ]))
    );
    let Some(Amf0Value::Object(video_fourcc_info)) = properties.get("videoFourCcInfoMap") else {
        panic!("enhanced connect should advertise video FourCC capability info");
    };
    assert_eq!(
        video_fourcc_info.get("hvc1"),
        Some(&Amf0Value::Number(0x06 as f64))
    );
    assert_eq!(
        video_fourcc_info.get("avc1"),
        Some(&Amf0Value::Number(0x06 as f64))
    );
}

#[test]
fn startup_video_sequence_header_skips_ingest_header_for_empty_raw_ring() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");
    let ingest_sequence_header =
        Bytes::from_static(&[0x17, 0x00, 0x00, 0x00, 0x00, 0x01, 0x64, 0x00, 0x1F]);

    let selected = startup_video_sequence_header(&ring, Some(ingest_sequence_header), false);

    assert!(
        selected.is_none(),
        "empty raw output rings should wait for their own keyframe/config"
    );
}

#[test]
fn startup_audio_waits_for_video_on_empty_transcoded_ring() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");

    assert!(
        rtmp_output_waits_for_video(&ring),
        "transcoded RTMP output rings should wait for their own video startup"
    );
    assert!(
        should_defer_audio_until_video_ready(false, &ring),
        "audio packets must be deferred until the codec-edge ring has emitted video"
    );
    assert!(
        !should_send_startup_audio_sequence_header(false, &ring),
        "startup AAC config must not be sent before video is ready on an empty transcoded ring"
    );
}

#[test]
fn startup_audio_allows_passthrough_audio_only_ring() {
    let ring = Arc::new(RingBuffer::new(1024));

    assert!(
        !rtmp_output_waits_for_video(&ring),
        "audio-only or unknown rings should not be forced to wait for video startup"
    );
    assert!(
        !should_defer_audio_until_video_ready(false, &ring),
        "audio packets should flow immediately when the ring is not video-gated"
    );
    assert!(
        should_send_startup_audio_sequence_header(false, &ring),
        "audio-only startup should still emit AAC config immediately"
    );
}

#[test]
fn startup_audio_unblocks_once_parameter_sets_exist() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");
    ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68, 0xCE,
        0x38, 0x80,
    ]);

    assert!(
        should_send_startup_audio_sequence_header(false, &ring),
        "once the output ring has H.264 parameter sets, startup audio can follow the video config"
    );
}

#[test]
fn deferred_audio_sequence_header_prefers_cached_flv_header() {
    let cached = Bytes::from_static(&[0xaf, 0x00, 0x12, 0x10]);
    let track = AudioMeta {
        codec: "aac".into(),
        sample_rate: 48_000,
        channels: 2,
        ..Default::default()
    };

    assert_eq!(
        resolve_deferred_audio_sequence_header(Some(&cached), Some(&track)),
        Some(cached)
    );
}

#[test]
fn deferred_audio_sequence_header_synthesizes_aac_when_cache_missing() {
    let track = AudioMeta {
        codec: "aac".into(),
        sample_rate: 48_000,
        channels: 2,
        ..Default::default()
    };

    let header = resolve_deferred_audio_sequence_header(None, Some(&track))
        .expect("AAC tracks should synthesize a deferred sequence header");

    assert_eq!(header[0], 0xaf);
    assert_eq!(header[1], 0x00);
}
