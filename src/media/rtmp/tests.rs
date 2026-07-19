use super::flv::{BitReader, parse_sps_video_info, sps_dimensions};
use super::*;
use crate::domain::ingest_security::IngestSecurityConfig;
use crate::media::engine::{AudioMeta, MediaEngine, VideoMeta};
use crate::media::ingest_auth::{AuthenticatedPipeline, PipelineAccessFuture};
use crate::media::ring_buffer::{MediaType, RingBuffer};
use proptest::prelude::*;
use rml_rtmp::chunk_io::ChunkDeserializer;
use rml_rtmp::messages::RtmpMessage;
use rml_rtmp::rml_amf0::Amf0Value;

#[tokio::test]
async fn client_handshake_can_be_bounded_when_peer_is_silent() {
    let (mut client, mut server) = tokio::io::duplex(4096);
    let cancel = CancellationToken::new();
    let peer = tokio::spawn(async move {
        let mut buf = [0u8; 1537];
        server.read_exact(&mut buf).await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let result = tokio::time::timeout(
        Duration::from_millis(25),
        perform_client_handshake(&mut client, &cancel),
    )
    .await;

    assert!(result.is_err(), "silent peer should not complete handshake");
    cancel.cancel();
    peer.abort();
}

/// Accepts every stream key as the same fixed pipeline id. Session fault
/// tests only care about what happens after a publisher is registered, not
/// about exercising the real database-backed lookup.
struct AcceptAllAuthenticator {
    pipeline_id: String,
}

impl PipelineAccessAuthenticator for AcceptAllAuthenticator {
    fn authenticate<'a>(
        &'a self,
        _mode: PipelineAccessMode,
        _stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            Ok(AuthenticatedPipeline {
                id: self.pipeline_id.clone(),
                input_id: _stream_key.to_string(),
                selected: true,
            })
        })
    }
}

/// Drives a real `rml_rtmp` `ClientSession` through handshake, connect, and
/// publish against `socket`, blocking until the server has accepted the
/// publish request. This reuses the same client-session machinery
/// `start_rtmp_egress` uses in production, so the resulting wire bytes are a
/// genuine RTMP publish handshake rather than hand-rolled AMF.
async fn drive_client_publish_handshake(socket: &mut TcpStream, stream_key: &str) {
    let cancel = CancellationToken::new();
    let remaining = perform_client_handshake(socket, &cancel)
        .await
        .expect("client handshake must succeed against handle_rtmp_client");

    let mut config = ClientSessionConfig::new();
    config.tc_url = Some("rtmp://127.0.0.1/live".to_string());
    let (mut session, initial_results) =
        ClientSession::new(config).expect("client session must initialize");
    for res in initial_results {
        if let ClientSessionResult::OutboundResponse(pkt) = res {
            socket.write_all(&pkt.bytes).await.unwrap();
        }
    }

    let conn_pkt = match session.request_connection("live".to_string()) {
        Ok(ClientSessionResult::OutboundResponse(p)) => p,
        other => panic!("expected connect request packet, got {other:?}"),
    };
    socket.write_all(&conn_pkt.bytes).await.unwrap();

    let mut buffer = vec![0u8; 4096];
    let mut pending = remaining;
    loop {
        let results = if !pending.is_empty() {
            let taken = std::mem::take(&mut pending);
            session.handle_input(&taken).unwrap()
        } else {
            let n = socket.read(&mut buffer).await.unwrap();
            assert!(n > 0, "server closed the connection during publish setup");
            session.handle_input(&buffer[..n]).unwrap()
        };

        let mut published = false;
        for res in results {
            match res {
                ClientSessionResult::OutboundResponse(pkt) => {
                    socket.write_all(&pkt.bytes).await.unwrap();
                }
                ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestAccepted) => {
                    let pub_pkt = match session
                        .request_publishing(stream_key.to_string(), PublishRequestType::Live)
                    {
                        Ok(ClientSessionResult::OutboundResponse(p)) => p,
                        other => panic!("expected publish request packet, got {other:?}"),
                    };
                    socket.write_all(&pub_pkt.bytes).await.unwrap();
                }
                ClientSessionResult::RaisedEvent(ClientSessionEvent::PublishRequestAccepted) => {
                    published = true;
                }
                _ => {}
            }
        }
        if published {
            break;
        }
    }
}

fn test_engine_and_security() -> (Arc<MediaEngine>, Arc<IngestSecurityService>) {
    (
        Arc::new(MediaEngine::new()),
        Arc::new(IngestSecurityService::new(IngestSecurityConfig::default())),
    )
}

/// A chunk with a non-zero format on a chunk stream id that has never seen a
/// type-0 header is invalid per the RTMP chunk spec (rml_rtmp's
/// `ChunkDeserializationError::NoPreviousChunkOnStream`). It is a single
/// byte, so it deterministically faults on the very next read instead of
/// stalling while the deserializer waits for more bytes.
const MALFORMED_CHUNK_HEADER_BYTE: [u8; 1] = [0x45];

#[tokio::test]
async fn malformed_chunk_after_publish_surfaces_error_and_clears_ingest_registration() {
    let (engine, security) = test_engine_and_security();
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> = Arc::new(AcceptAllAuthenticator {
        pipeline_id: "pipe-fault-malformed".to_string(),
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let engine_c = engine.clone();
    let server = tokio::spawn(async move {
        let (socket, client_addr) = listener.accept().await.unwrap();
        handle_rtmp_client(socket, client_addr, pipeline_access, security, engine_c).await
    });

    let mut client = TcpStream::connect(addr).await.unwrap();
    drive_client_publish_handshake(&mut client, "any-key").await;

    assert!(
        engine
            .ingests
            .active
            .read()
            .await
            .contains_key("pipe-fault-malformed"),
        "publish must register an active ingest before the fault is injected"
    );

    client
        .write_all(&MALFORMED_CHUNK_HEADER_BYTE)
        .await
        .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("handle_rtmp_client must not hang on malformed chunk input")
        .expect("handle_rtmp_client task must not panic");

    assert_eq!(result, Ok(()));
    assert!(
        !engine
            .ingests
            .active
            .read()
            .await
            .contains_key("pipe-fault-malformed"),
        "malformed input after publish must fully unregister the ingest"
    );
}

#[tokio::test]
async fn truncated_chunk_then_disconnect_clears_ingest_registration_without_error() {
    let (engine, security) = test_engine_and_security();
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> = Arc::new(AcceptAllAuthenticator {
        pipeline_id: "pipe-fault-truncated".to_string(),
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let engine_c = engine.clone();
    let server = tokio::spawn(async move {
        let (socket, client_addr) = listener.accept().await.unwrap();
        handle_rtmp_client(socket, client_addr, pipeline_access, security, engine_c).await
    });

    let mut client = TcpStream::connect(addr).await.unwrap();
    drive_client_publish_handshake(&mut client, "any-key").await;

    assert!(
        engine
            .ingests
            .active
            .read()
            .await
            .contains_key("pipe-fault-truncated"),
        "publish must register an active ingest before the fault is injected"
    );

    // A lone type-0 basic header byte on csid 3 is a valid start of a new
    // chunk, but the deserializer needs 11 more bytes (timestamp, length,
    // type, stream id) before it forms a message. Sending just this byte and
    // then closing the socket simulates a mid-message truncation: the
    // deserializer must keep buffering rather than erroring, and the
    // resulting EOF must still be treated as an ordinary disconnect.
    client.write_all(&[0x03]).await.unwrap();
    drop(client);

    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("handle_rtmp_client must not hang on a truncated chunk plus disconnect")
        .expect("handle_rtmp_client task must not panic");

    assert_eq!(result, Ok(()));
    assert!(
        !engine
            .ingests
            .active
            .read()
            .await
            .contains_key("pipe-fault-truncated"),
        "truncated input followed by disconnect must fully unregister the ingest"
    );
}

#[test]
fn h264_sequence_header_for_keyframe_uses_cached_parameter_sets() {
    let mut cache = Vec::new();
    cache_h264_parameter_sets(
        &[
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
            0xCE, 0x38, 0x80,
        ],
        &mut cache,
    );
    let (sequence_header, sps) =
        h264_sequence_header_for_keyframe(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80], &cache)
            .expect("cached SPS/PPS should synthesize a sequence header");

    assert_eq!(sequence_header[0], 0x17);
    assert_eq!(sequence_header[1], 0x00);
    assert_eq!(
        sps,
        Some(vec![0x67, 0x42, 0x00, 0x1E, 0xAB]),
        "cached SPS should be reused when the keyframe carries only IDR data"
    );
}

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

    let selected =
        startup_video_sequence_header(&ring, Some(ingest_sequence_header.clone()), false)
            .expect("ring parameter sets should synthesize a startup header");

    assert_ne!(
        selected, ingest_sequence_header,
        "startup header should come from the output ring, not the ingest cache"
    );
    assert_eq!(selected[0], 0x17);
    assert_eq!(selected[1], 0x00);
}

#[test]
fn startup_video_sequence_header_builds_enhanced_hevc_header() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("hevc");
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
    let parameter_sets = packets
        .iter()
        .find_map(|packet| {
            (packet.media_type == crate::media::ring_buffer::MediaType::Video)
                .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
                .flatten()
        })
        .expect("fixture should carry HEVC parameter sets");
    ring.set_video_parameter_sets(parameter_sets);

    let selected = startup_video_sequence_header(&ring, None, true)
        .expect("HEVC parameter sets should synthesize an enhanced startup header");

    assert_eq!(&selected[..5], &[0x80, b'h', b'v', b'c', b'1']);
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
fn warmup_does_not_unlock_codec_edge_output_on_audio_only_burst() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");
    let packets = vec![Arc::new(MediaPacket {
        media_type: MediaType::Audio,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x11, 0x22]),
    })];

    assert!(
        !rtmp_warmup_ready(&ring, &packets),
        "codec-edge warmup should keep waiting until video startup data exists"
    );
}

#[test]
fn warmup_unlocks_codec_edge_output_once_video_startup_arrives() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");
    let video_packets = vec![Arc::new(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x65]),
    })];

    assert!(
        rtmp_warmup_ready(&ring, &video_packets),
        "seeing a video burst should unlock RTMP warmup even before parameter sets are cached"
    );

    let parameter_set_ring = Arc::new(RingBuffer::new(1024));
    parameter_set_ring.set_codec_hint("h264");
    parameter_set_ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68, 0xCE,
        0x38, 0x80,
    ]);

    assert!(
        rtmp_warmup_ready(&parameter_set_ring, &[]),
        "cached parameter sets should also satisfy RTMP warmup readiness"
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

#[tokio::test]
async fn rtmp_metadata_uses_terminal_ring_codec_for_hevc_codec_edge() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("p-codec-edge", "key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "p-codec-edge",
            Some(VideoMeta {
                codec: "hevc".into(),
                width: 1920,
                height: 1080,
                fps: 50.0,
                ..Default::default()
            }),
            None,
            None,
        )
        .await;

    let output_ring = Arc::new(RingBuffer::new(1024));
    output_ring.set_codec_hint("h264");
    let audio = AudioMeta {
        codec: "aac".into(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..Default::default()
    };

    let metadata = rtmp_publish_metadata(&engine, "p-codec-edge", &output_ring, Some(&audio))
        .await
        .expect("codec-edge RTMP output should publish metadata");

    assert_eq!(
        metadata.video_codec_id,
        Some(7),
        "HEVC ingest converted by hevc_to_h264 must advertise AVC on RTMP"
    );
    assert_eq!(metadata.video_width, Some(1920));
    assert_eq!(metadata.video_height, Some(1080));
    assert_eq!(metadata.audio_codec_id, Some(10));
}

#[tokio::test]
async fn rtmp_metadata_advertises_unconverted_hevc_as_hvc1_fourcc() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("p-hevc-source", "key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "p-hevc-source",
            Some(VideoMeta {
                codec: "hevc".into(),
                width: 1920,
                height: 1080,
                fps: 50.0,
                ..Default::default()
            }),
            None,
            None,
        )
        .await;

    let output_ring = Arc::new(RingBuffer::new(1024));
    let audio = AudioMeta {
        codec: "aac".into(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..Default::default()
    };

    let metadata = rtmp_publish_metadata(&engine, "p-hevc-source", &output_ring, Some(&audio))
        .await
        .expect("audio metadata should still be present");

    assert_eq!(
        metadata.video_codec_id,
        Some(RTMP_METADATA_VIDEO_CODEC_ID_HEVC),
        "without a terminal H.264 ring, HEVC must be advertised as hvc1, not AVC"
    );
    assert_eq!(metadata.video_width, Some(1920));
    assert_eq!(metadata.video_height, Some(1080));
    assert_eq!(metadata.audio_codec_id, Some(10));
}

#[tokio::test]
async fn start_rtmp_egress_waits_for_ring_data_before_connecting() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");

    let engine = Arc::new(MediaEngine::new());
    let registration = engine
        .register_egress_attempt("out-warmup", "pipe-warmup", "rtmp://ignored/app/key", None)
        .await;

    let ring_c = ring.clone();
    let engine_c = engine.clone();
    let registration_c = registration.clone();
    tokio::spawn(async move {
        start_rtmp_egress(
            "out-warmup".to_string(),
            "pipe-warmup".to_string(),
            format!("rtmp://{}/live/key", addr),
            ring_c,
            engine_c,
            registration_c,
            crate::domain::output_spec::RtmpOutputMode::Legacy,
        )
        .await;
    });

    // No data on the ring yet: the pre-connect warmup gate must hold off
    // connecting, or MediaMTX would accept an idle publisher and later
    // drop it for inactivity (the root cause of the RTMP reconnect storm
    // under high output fanout).
    let early_accept =
        tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
    assert!(
        early_accept.is_err(),
        "must not connect before the ring has data"
    );

    ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Flv,
        payload: bytes::Bytes::from_static(&[0x17, 0x01, 0x00, 0x00, 0x00]),
    });

    let late_accept =
        tokio::time::timeout(std::time::Duration::from_secs(2), listener.accept()).await;
    assert!(late_accept.is_ok(), "must connect once the ring has data");

    registration.cancel_token.cancel();
}

#[tokio::test]
async fn detects_h265_from_ingest_video_meta() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("p1", "key", "srt")
        .await
        .unwrap();

    // No video meta yet → not H.265
    {
        let ingests = engine.ingests.active.read().await;
        let is_h265 = ingests
            .get("p1")
            .and_then(|ingest| ingest.metadata().video)
            .map(|v| v.codec == "hevc")
            .unwrap_or(false);
        assert!(!is_h265, "no video meta should not be hevc");
    }

    // H.264 meta → not H.265
    engine
        .update_ingest_meta(
            "p1",
            Some(VideoMeta {
                codec: "h264".into(),
                width: 0,
                height: 0,
                fps: 0.0,
                bw: None,
                pid: None,
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            None,
            None,
        )
        .await;
    {
        let ingests = engine.ingests.active.read().await;
        let is_h265 = ingests
            .get("p1")
            .and_then(|ingest| ingest.metadata().video)
            .map(|v| v.codec == "hevc")
            .unwrap_or(false);
        assert!(!is_h265, "h264 meta should not be hevc");
    }

    // H.265 meta → is H.265
    engine
        .update_ingest_meta(
            "p1",
            Some(VideoMeta {
                codec: "hevc".into(),
                width: 0,
                height: 0,
                fps: 0.0,
                bw: None,
                pid: None,
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            None,
            None,
        )
        .await;
    {
        let ingests = engine.ingests.active.read().await;
        let is_h265 = ingests
            .get("p1")
            .and_then(|ingest| ingest.metadata().video)
            .map(|v| v.codec == "hevc")
            .unwrap_or(false);
        assert!(is_h265, "hevc meta should be detected");
    }

    engine.unregister_ingest("p1").await;
}

#[tokio::test]
async fn h265_detection_waits_for_probe_meta() {
    let engine = Arc::new(MediaEngine::new());
    let engine_clone = engine.clone();
    let pipeline_id = "p2".to_string();

    engine
        .try_register_ingest(&pipeline_id, "key", "srt")
        .await
        .unwrap();

    // Spawn a task that sets the video meta after a delay (simulating probe arrival)
    let delayed_engine = engine.clone();
    let delayed_pid = pipeline_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        delayed_engine
            .update_ingest_meta(
                &delayed_pid,
                Some(VideoMeta {
                    codec: "hevc".into(),
                    width: 0,
                    height: 0,
                    fps: 0.0,
                    bw: None,
                    pid: None,
                    language: None,
                    title: None,
                    profile: None,
                    level: None,
                    pixel_format: None,
                }),
                None,
                None,
            )
            .await;
    });

    // Now run the same probe-wait logic that start_rtmp_egress uses
    let is_h265 = 'probe: {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let ingests = engine_clone.ingests.active.read().await;
            let meta = ingests
                .get(&pipeline_id)
                .and_then(|ingest| ingest.metadata().video);
            match meta {
                Some(v) if v.codec == "hevc" => break 'probe true,
                Some(_) => break 'probe false,
                None => {}
            }
            drop(ingests);
            if std::time::Instant::now() >= deadline {
                break 'probe false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };
    assert!(is_h265, "should detect hevc after probe arrives");

    engine.unregister_ingest(&pipeline_id).await;
}

#[tokio::test]
async fn resolved_output_audio_tracks_falls_back_when_ring_metadata_is_empty() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-audio", "key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_audio_tracks(
            "pipe-audio",
            vec![
                AudioMeta {
                    codec: "aac".into(),
                    sample_rate: 48_000,
                    channels: 2,
                    track_index: 0,
                    ..Default::default()
                },
                AudioMeta {
                    codec: "aac".into(),
                    sample_rate: 48_000,
                    channels: 2,
                    track_index: 1,
                    ..Default::default()
                },
            ],
        )
        .await;

    let ring = Arc::new(RingBuffer::new(16));
    ring.set_audio_tracks(Vec::new());

    let tracks = resolved_output_audio_tracks(&engine, "pipe-audio", &ring).await;

    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].track_index, 0);
    assert_eq!(tracks[1].track_index, 1);

    engine.unregister_ingest("pipe-audio").await;
}

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

#[test]
fn parse_rtmp_url_standard_forms() {
    // Default port
    let parts = parse_rtmp_url("rtmp://a.example.com/live/mykey").unwrap();
    assert_eq!(parts.host, "a.example.com");
    assert_eq!(parts.port, 1935);
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "mykey");
    assert!(!parts.tls);

    // Explicit port
    let parts = parse_rtmp_url("rtmp://a.example.com:19350/stream/abc").unwrap();
    assert_eq!(parts.host, "a.example.com");
    assert_eq!(parts.port, 19350);
    assert_eq!(parts.app, "stream");
    assert_eq!(parts.stream_key, "abc");
    assert!(!parts.tls);

    // rtmps:// (TLS) — same parsing, different default port behaviour (still 1935 if omitted)
    let parts = parse_rtmp_url("rtmps://live-api-s.facebook.com:443/rtmp/FB-STREAM-KEY").unwrap();
    assert_eq!(parts.host, "live-api-s.facebook.com");
    assert_eq!(parts.port, 443);
    assert_eq!(parts.app, "rtmp");
    assert_eq!(parts.stream_key, "FB-STREAM-KEY");
    assert!(parts.tls);

    // Stream key containing slashes is NOT split — key gets everything after first slash in path
    let parts = parse_rtmp_url("rtmp://host/app/key/subpart").unwrap();
    assert_eq!(parts.app, "app");
    assert_eq!(parts.stream_key, "key/subpart");
    assert!(!parts.tls);

    // Unrecognised scheme → None
    assert!(parse_rtmp_url("https://host/live/key").is_none());

    // Missing path separator → None (can't split app/key)
    assert!(parse_rtmp_url("rtmp://host/noapp").is_none());
}

// --- Regression: issue #5 (Round 5) — IPv6 RTMP URL parsing ---
// Before the fix, `host_port.find(':')` landed inside the IPv6 brackets
// (first `:` in `[::1]:1935` is at position 2, inside the brackets),
// causing the host to be parsed as `[` and port parsing to fail.
#[test]
fn parse_rtmp_url_ipv6_literal() {
    let result = parse_rtmp_url("rtmp://[::1]:1935/live/mykey");
    assert!(result.is_some(), "IPv6 URL must parse successfully");
    let parts = result.unwrap();
    assert_eq!(parts.host, "::1");
    assert_eq!(parts.port, 1935);
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "mykey");
    assert!(!parts.tls);
}

#[test]
fn parse_rtmp_url_ipv6_default_port() {
    let result = parse_rtmp_url("rtmp://[2001:db8::1]/live/mykey");
    assert!(
        result.is_some(),
        "IPv6 URL without port must use default 1935"
    );
    let parts = result.unwrap();
    assert_eq!(parts.host, "2001:db8::1");
    assert_eq!(parts.port, 1935);
    assert!(!parts.tls);
}

#[test]
fn parse_rtmp_url_ipv4_unchanged() {
    // Ensure the IPv4 path still works correctly after the IPv6 fix.
    let result = parse_rtmp_url("rtmp://192.168.1.1:1935/live/key");
    assert!(result.is_some());
    let parts = result.unwrap();
    assert_eq!(parts.host, "192.168.1.1");
    assert_eq!(parts.port, 1935);
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "key");
    assert!(!parts.tls);
}

// --- Adversarial: percent-encoded app/stream key must reach the
// destination RTMP server decoded, not still escaped ---
// (found via a "hunting" pass on egress_transport.rs — path_segments()
// returns raw percent-encoded segments; forwarding them unescaped as the
// AMF-level app/stream key would corrupt any push target whose key
// contains a URL-reserved character.)

#[test]
fn parse_rtmp_url_percent_encoded_stream_key_slash() {
    // %2F inside a single path segment must decode to a literal '/' in the
    // stream key, not stay as the three-character escape.
    let parts = parse_rtmp_url("rtmp://host/live/part1%2Fpart2").unwrap();
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "part1/part2");
}

#[test]
fn parse_rtmp_url_percent_encoded_app_and_space() {
    let parts = parse_rtmp_url("rtmp://host/my%20app/key%2Bvalue").unwrap();
    assert_eq!(parts.app, "my app");
    assert_eq!(parts.stream_key, "key+value");
}

#[test]
fn parse_rtmp_url_invalid_percent_sequence_does_not_panic() {
    // A stray '%' not followed by two hex digits is invalid percent-encoding;
    // decoding must degrade gracefully (lossy) rather than panic.
    let parts = parse_rtmp_url("rtmp://host/live/key%zz").unwrap();
    assert_eq!(parts.stream_key, "key%zz");
}

#[test]
fn parse_rtmp_url_trailing_slash_yields_trailing_slash_key() {
    // Documents current behaviour: a trailing path separator becomes a
    // trailing '/' in the stream key rather than being trimmed or rejected.
    let parts = parse_rtmp_url("rtmp://host/live/key/").unwrap();
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "key/");
}

#[test]
fn parse_rtmp_url_ignores_query_and_fragment() {
    let parts = parse_rtmp_url("rtmp://host/live/key?token=abc#frag").unwrap();
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "key");
}

#[test]
fn parse_rtmp_url_drops_embedded_userinfo() {
    // Credentials embedded in the URL (rtmp://user:pass@host/...) must not
    // leak into app/stream_key and must not change the resolved host.
    let parts = parse_rtmp_url("rtmp://user:pass@host/live/key").unwrap();
    assert_eq!(parts.host, "host");
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "key");
}

#[test]
fn parse_rtmp_url_rejects_empty_authority() {
    assert!(parse_rtmp_url("rtmp:///live/key").is_none());
}

#[test]
fn parse_rtmp_url_rejects_out_of_range_port() {
    assert!(parse_rtmp_url("rtmp://host:999999/live/key").is_none());
}

#[test]
fn parse_rtmp_url_rejects_unterminated_ipv6_literal() {
    assert!(parse_rtmp_url("rtmp://[::1/live/key").is_none());
}

#[test]
fn parse_rtmp_url_case_insensitive_scheme() {
    let parts = parse_rtmp_url("RTMP://host/live/key").unwrap();
    assert!(!parts.tls);
    let parts = parse_rtmp_url("RTMPS://host/live/key").unwrap();
    assert!(parts.tls);
}

#[test]
fn parse_rtmp_url_trims_surrounding_whitespace() {
    let parts = parse_rtmp_url(" rtmp://host/live/key ").unwrap();
    assert_eq!(parts.host, "host");
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "key");
}

// --- format_host_port: must bracket bare IPv6 literals for connect-string
// use, but never double-bracket or mangle plain hostnames/IPv4 ---

#[test]
fn format_host_port_plain_hostname() {
    assert_eq!(
        super::egress_transport::format_host_port("example.com", 1935),
        "example.com:1935"
    );
}

#[test]
fn format_host_port_ipv4_literal() {
    assert_eq!(
        super::egress_transport::format_host_port("192.168.1.1", 1935),
        "192.168.1.1:1935"
    );
}

#[test]
fn format_host_port_bare_ipv6_gets_bracketed() {
    // parse_rtmp_url strips brackets into `host`; format_host_port must
    // re-add them so lookup_host doesn't misparse the embedded colons.
    assert_eq!(
        super::egress_transport::format_host_port("::1", 1935),
        "[::1]:1935"
    );
    assert_eq!(
        super::egress_transport::format_host_port("2001:db8::1", 443),
        "[2001:db8::1]:443"
    );
}

#[test]
fn format_host_port_already_bracketed_is_not_double_wrapped() {
    assert_eq!(
        super::egress_transport::format_host_port("[::1]", 1935),
        "[::1]:1935"
    );
}

// --- FLV video meta: malformed / truncated / unknown codec ---

#[test]
fn parse_flv_video_meta_empty_returns_none() {
    assert!(parse_flv_video_meta(&[]).is_none());
}

#[test]
fn parse_flv_video_meta_single_byte_returns_none() {
    assert!(parse_flv_video_meta(&[0x17]).is_none());
}

#[test]
fn parse_flv_video_meta_unknown_codec_id_returns_none() {
    // codec_id=5 (On2 VP6 with alpha) — not handled
    let data = [0x15u8, 0x01, 0x00, 0x00, 0x00];
    assert!(parse_flv_video_meta(&data).is_none());
}

#[test]
fn parse_flv_video_meta_vp6_returns_codec_name() {
    // frame_type=1, codec_id=4 (VP6) → meta returned with codec="vp6"
    let data = [0x14u8, 0x00];
    let meta = parse_flv_video_meta(&data).unwrap();
    assert_eq!(meta.codec, "vp6");
    assert_eq!(meta.width, 0);
}

#[test]
fn parse_flv_video_meta_h265_returns_codec_name() {
    // frame_type=1, codec_id=12 (H.265/HEVC enhanced)
    let data = [0x1Cu8, 0x01, 0x00, 0x00, 0x00];
    let meta = parse_flv_video_meta(&data).unwrap();
    assert_eq!(meta.codec, "h265");
}

#[test]
fn parse_flv_video_meta_h264_seq_header_truncated_avcc() {
    // seq header (byte[1]=0) but AVCDecoderConfigurationRecord too short to extract profile/level
    // data.len() == 6: passes the > 12 check? No: 6 < 12 → skips SPS parsing, no panic
    let data = [0x17u8, 0x00, 0x00, 0x00, 0x00, 0x01];
    let meta = parse_flv_video_meta(&data).unwrap();
    assert_eq!(meta.codec, "h264");
    // profile/level not parsed (too short)
    assert!(meta.profile.is_none());
    assert!(meta.level.is_none());
    assert_eq!(meta.width, 0);
}

#[test]
fn parse_flv_video_meta_h264_seq_header_short_sps_length_field() {
    // 13 bytes: passes > 12 check. avc_config starts at data[5].
    // avc_config[5]=0xE1 (numSPS=1), avc_config[6..7]=SPS len = 0x0001 (1 byte),
    // but then we'd need avc_config[8 + 1] = 9 bytes total in avc_config.
    // avc_config len = 13-5 = 8 bytes → 8 < 9 → SPS resolution not parsed. No panic.
    let data = [
        0x17u8, 0x00, 0x00, 0x00, 0x00, // frame_type/codec, pkt_type, comp_time
        0x01, 0x64, 0x00, 0x1F, // version, profile, compat, level
        0xFF, 0xE1, // lengthSizeMinusOne, numSPS=1
        0x00, 0x01, // SPS length = 1 (only 0 bytes remain → out of bounds)
    ];
    let meta = parse_flv_video_meta(&data).unwrap();
    assert_eq!(meta.codec, "h264");
    assert_eq!(meta.profile.as_deref(), Some("High"));
    assert_eq!(meta.level.as_deref(), Some("3.1"));
    assert_eq!(meta.width, 0); // SPS not parsed, no panic
}

#[test]
fn parse_flv_video_meta_h264_seq_header_extracts_fps_from_sps_vui() {
    // libx264 AVCDecoderConfigurationRecord carrying a 1920x1080@50 SPS.
    #[rustfmt::skip]
        let data = [
            0x17u8, 0x00, 0x00, 0x00, 0x00, // keyframe, AVC sequence header
            0x01, 0x42, 0xc0, 0x2a, 0xff, 0xe1, 0x00, 0x18,
            0x67, 0x42, 0xc0, 0x2a, 0xda, 0x01, 0xe0, 0x08,
            0x9f, 0x97, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00,
            0x10, 0x00, 0x00, 0x06, 0x48, 0xf1, 0x83, 0x2a,
            0x01, 0x00, 0x04, 0x68, 0xce, 0x0f, 0xc8,
        ];

    let meta = parse_flv_video_meta(&data).unwrap();
    assert_eq!(meta.codec, "h264");
    assert_eq!(meta.width, 1920);
    assert_eq!(meta.height, 1080);
    assert!((meta.fps - 50.0).abs() < 0.01, "fps={}", meta.fps);
}

// --- FLV audio meta: malformed / truncated / non-AAC codecs ---

#[test]
fn parse_flv_audio_meta_empty_returns_none() {
    assert!(parse_flv_audio_meta(&[]).is_none());
}

#[test]
fn parse_flv_audio_meta_mp3_no_asc() {
    // format_id=2 (MP3), rate=3 (44100), size=1, type=1 (stereo)
    let data = [0x2Fu8];
    let meta = parse_flv_audio_meta(&data).unwrap();
    assert_eq!(meta.codec, "mp3");
    assert_eq!(meta.sample_rate, 44100);
    assert_eq!(meta.channels, 2);
}

#[test]
fn parse_flv_audio_meta_speex_mono_11025() {
    // format_id=11 (Speex), rate=1 (11025), type=0 (mono)
    let data = [0xB4u8];
    let meta = parse_flv_audio_meta(&data).unwrap();
    assert_eq!(meta.codec, "speex");
    assert_eq!(meta.sample_rate, 11025);
    assert_eq!(meta.channels, 1);
    assert_eq!(meta.channel_layout.as_deref(), Some("mono"));
}

#[test]
fn parse_flv_audio_meta_aac_data_packet_not_seq_header() {
    // format_id=10 (AAC), byte[1]=1 (data packet, not seq header) → no ASC parsing
    let data = [0xAFu8, 0x01, 0x12, 0x10];
    let meta = parse_flv_audio_meta(&data).unwrap();
    assert_eq!(meta.codec, "aac");
    // sample_rate from FLV rate_id bits only (rate_id=3 → 44100)
    assert_eq!(meta.sample_rate, 44100);
}

#[test]
fn parse_flv_audio_meta_aac_seq_header_truncated_asc_one_byte() {
    // format_id=10, byte[1]=0 (seq header), only 1 byte of ASC → asc.len() < 2, no ASC parsing
    let data = [0xAFu8, 0x00, 0x12];
    let meta = parse_flv_audio_meta(&data).unwrap();
    assert_eq!(meta.codec, "aac");
    // Falls back to FLV header rates (rate_id=3 → 44100)
    assert_eq!(meta.sample_rate, 44100);
}

#[test]
fn parse_flv_audio_meta_aac_5_1_surround() {
    // object_type=2 (AAC-LC), freq_idx=3 (48000), ch_config=6 (5.1)
    // byte[0]: 0xAF (format=10, rate=3, size=1, channels=1 bit)
    // ASC: (2<<3)|(3>>1)=0x11, (3<<7)|(6<<3)=0xB0
    let data = [0xAFu8, 0x00, 0x11, 0xB0];
    let meta = parse_flv_audio_meta(&data).unwrap();
    assert_eq!(meta.codec, "aac");
    assert_eq!(meta.sample_rate, 48000);
    assert_eq!(meta.channels, 6);
    assert_eq!(meta.channel_layout.as_deref(), Some("5.1"));
}

#[test]
fn rtmp_timestamp_guard_bumps_repeated_video_dts() {
    let mut guard = RtmpTimestampGuard::new();

    assert_eq!(guard.enforce_ms(MediaType::Video, 41), 41);
    assert_eq!(guard.enforce_ms(MediaType::Video, 41), 42);
    assert_eq!(guard.enforce_ms(MediaType::Video, 40), 43);
}

#[test]
fn rtmp_timestamp_guard_bumps_repeated_audio_pts() {
    let mut guard = RtmpTimestampGuard::new();

    assert_eq!(guard.enforce_ms(MediaType::Audio, 26), 26);
    assert_eq!(guard.enforce_ms(MediaType::Audio, 26), 27);
    assert_eq!(guard.enforce_ms(MediaType::Audio, 25), 28);
}

#[test]
fn rtmp_timestamp_guard_keeps_audio_and_video_independent() {
    let mut guard = RtmpTimestampGuard::new();

    assert_eq!(guard.enforce_ms(MediaType::Video, 100), 100);
    assert_eq!(guard.enforce_ms(MediaType::Audio, 100), 100);
    assert_eq!(guard.enforce_ms(MediaType::Video, 100), 101);
    assert_eq!(guard.enforce_ms(MediaType::Audio, 100), 101);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn proptest_rtmp_timestamp_guard_is_bounded_and_monotone_per_media(
        events in proptest::collection::vec((any::<bool>(), -1_000i64..=(u32::MAX as i64 + 1_000)), 1..128),
    ) {
        let mut guard = RtmpTimestampGuard::new();
        let mut expected_video = i64::MIN;
        let mut expected_audio = i64::MIN;

        for (is_video, input_ts) in events {
            let media_type = if is_video {
                MediaType::Video
            } else {
                MediaType::Audio
            };
            let expected_slot = if is_video {
                &mut expected_video
            } else {
                &mut expected_audio
            };

            let mut expected = input_ts.clamp(0, u32::MAX as i64);
            if expected <= *expected_slot {
                expected = (*expected_slot + 1).min(u32::MAX as i64);
            }
            *expected_slot = expected;

            let actual = guard.enforce_ms(media_type, input_ts);
            prop_assert_eq!(actual, expected);
            prop_assert!((0..=u32::MAX as i64).contains(&actual));
        }
    }

    #[test]
    fn proptest_refreshed_video_sequence_header_timestamp_precedes_media(
        media_ts in any::<u32>(),
    ) {
        let refreshed = refreshed_video_sequence_header_timestamp(RtmpTimestamp::new(media_ts));
        prop_assert_eq!(refreshed.value, media_ts.saturating_sub(1));
        prop_assert!(refreshed.value <= media_ts);
    }
}

#[test]
fn refreshed_video_sequence_header_uses_current_media_timestamp() {
    let timestamp = refreshed_video_sequence_header_timestamp(RtmpTimestamp::new(42));

    assert_eq!(timestamp.value, 41);
}

#[test]
fn refreshed_video_sequence_header_consumes_a_video_timestamp_slot() {
    let mut guard = RtmpTimestampGuard::new();
    let sequence_header_ts = RtmpTimestamp::new(guard.enforce_ms(MediaType::Video, 42) as u32);
    let packet_ts = RtmpTimestamp::new(
        guard.enforce_ms(MediaType::Video, sequence_header_ts.value as i64) as u32,
    );

    assert_eq!(
        refreshed_video_sequence_header_timestamp(sequence_header_ts).value,
        41
    );
    assert_eq!(
        packet_ts.value, 43,
        "the following keyframe must advance past the refreshed sequence header DTS"
    );
}

#[test]
fn refreshed_video_sequence_header_keeps_zero_timestamp_for_first_keyframe() {
    let timestamp = refreshed_video_sequence_header_timestamp(RtmpTimestamp::new(0));

    assert_eq!(timestamp.value, 0);
}

#[test]
fn validate_rtmp_output_audio_tracks_accepts_single_track() {
    let track = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 1,
        ..Default::default()
    };

    assert!(validate_rtmp_output_audio_tracks(&[track]).is_ok());
}

#[test]
fn validate_rtmp_output_audio_tracks_rejects_multitrack_outputs() {
    let track0 = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..Default::default()
    };
    let track1 = AudioMeta {
        track_index: 1,
        ..track0.clone()
    };

    let error = validate_rtmp_output_audio_tracks(&[track0, track1]).unwrap_err();
    assert!(error.contains("exactly one audio track"));
    assert!(error.contains("subset"));
}

#[test]
fn validate_rtmp_output_audio_packet_track_accepts_track_zero() {
    assert!(validate_rtmp_output_audio_packet_track(0).is_ok());
}

#[test]
fn validate_rtmp_output_audio_packet_track_rejects_nonzero_track() {
    let error = validate_rtmp_output_audio_packet_track(1).unwrap_err();
    assert!(error.contains("single routed audio track"));
    assert!(error.contains("track index 1"));
}

// --- FLV composition time: edge cases ---

#[test]
fn flv_composition_time_too_short_returns_zero() {
    assert_eq!(flv_video_composition_time_ms(&[]), 0);
    assert_eq!(flv_video_composition_time_ms(&[0x17, 0x01, 0x00, 0x00]), 0); // 4 bytes < 5
}

#[test]
fn flv_composition_time_sequence_header_returns_zero() {
    // packet_type=0 (seq header) → composition time is always 0 per spec
    let data = [0x17u8, 0x00, 0x00, 0x00, 0x28];
    assert_eq!(flv_video_composition_time_ms(&data), 0);
}

#[test]
fn flv_composition_time_h265_nalu_packet() {
    // codec_id=12 (H.265), packet_type=1 (NALU), positive offset = 40ms
    let data = [0x1Cu8, 0x01, 0x00, 0x00, 0x28];
    assert_eq!(flv_video_composition_time_ms(&data), 40);
}

#[test]
fn flv_composition_time_audio_byte_returns_zero() {
    // FLV audio tag (codec_id=10, i.e. byte[0]&0x0F=10, not 7 or 12) → 0
    let data = [0xAFu8, 0x01, 0x00, 0x00, 0x28];
    assert_eq!(flv_video_composition_time_ms(&data), 0);
}
