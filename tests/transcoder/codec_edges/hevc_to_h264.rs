use std::sync::Arc;
use std::time::Duration;

use restream::media::engine::MediaEngine;
use restream_core::{MediaType, PayloadFormat};
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_sync::Reader;

use crate::support::{collect_packets_with_deadline, load_primary_transport_packets};

#[test]
fn rtmp_shaped_hevc_packets_drive_h264_edge_stage() {
    let _ = tracing_subscriber::fmt::try_init();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("rtmp-hevc").await;
        let manager = restream::media::stage_runtime::StageRuntimeManager::new(engine.clone());
        let stage_key = StageKey::new(
            "rtmp-hevc",
            StageKind::codec_edge("hevc_to_h264", StageKind::source()),
        );
        let (handle, _lease204, is_new) = manager
            .ensure_stage(stage_key.clone(), source.clone(), None)
            .await;
        assert!(is_new);
        let output = handle.ring.clone();
        let cancel = handle.cancel.clone();

        engine
            .try_register_ingest("rtmp-hevc", "stream-key", "rtmp")
            .await
            .unwrap();

        let (video, audio_tracks, mut packets) = load_primary_transport_packets("h265");
        let (_video_sequence_header, audio_sequence_header) =
            restream::test_fixtures::wrap_packets_for_rtmp_ingest(
                &video,
                &audio_tracks,
                &mut packets,
            );
        packets.truncate(100);

        engine
            .update_ingest_meta(
                "rtmp-hevc",
                Some(video.clone()),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("rtmp-hevc", audio_tracks.clone())
            .await;
        if let Some(audio_sequence_header) = audio_sequence_header {
            engine
                .cache_sequence_header("rtmp-hevc", false, audio_sequence_header)
                .await;
        }

        for packet in packets.iter().take(10) {
            source.push(packet.clone());
        }

        manager.spawn_codec_edge_stage(handle, source.clone());

        tokio::time::sleep(Duration::from_millis(50)).await;

        for packet in packets.into_iter().skip(10) {
            source.push(packet);
        }

        let mut reader = Reader::new("rtmp_hevc_h264_edge".to_string(), output.clone());
        let packets = collect_packets_with_deadline(&mut reader, 40, Duration::from_secs(5)).await;

        cancel.cancel();

        assert_eq!(output.codec_hint_str(), "h264");
        assert!(
            packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "expected video packets from RTMP-shaped hevc h264 edge stage"
        );
        assert!(
            packets
                .iter()
                .filter(|packet| packet.media_type == MediaType::Video)
                .all(|packet| packet.format == PayloadFormat::Raw),
            "expected raw H.264 packets out of the hevc_to_h264 edge stage"
        );
    });
}
