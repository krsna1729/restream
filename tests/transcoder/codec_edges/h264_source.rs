use std::sync::Arc;
use std::time::Duration;

use restream::domain::stage::{StageKey, StageKind};
use restream::media::engine::MediaEngine;
use restream::media::packet::MediaType;
use restream::media::ring_buffer::Reader;

use crate::support::{
    collect_packets_with_deadline, configure_ffmpeg_test_logging, load_primary_transport_packets,
};

#[test]
fn rtmp_shaped_h264_packets_drive_source_stage() {
    configure_ffmpeg_test_logging();
    let _ = tracing_subscriber::fmt::try_init();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("rtmp-h264").await;
        let policy = restream::planner::BackendPolicy {
            internal_video_presets: true,
            internal_hevc_to_h264: true,
            ..restream::planner::BackendPolicy::default()
        };
        let manager = restream::media::stage_runtime::StageRuntimeManager::with_policy(
            engine.clone(),
            policy,
        );
        let stage_key = StageKey::new("rtmp-h264", StageKind::source());
        let (handle, is_new) = manager
            .ensure_stage(stage_key.clone(), source.clone(), None)
            .await;
        assert!(is_new);
        let output = handle.ring.clone();
        let cancel = handle.cancel.clone();

        engine
            .try_register_ingest("rtmp-h264", "stream-key", "rtmp")
            .await
            .unwrap();

        let (video, audio_tracks, mut packets) = load_primary_transport_packets("h264");
        let (video_sh, audio_sh) = restream::test_fixtures::wrap_packets_for_rtmp_ingest(
            &video,
            &audio_tracks,
            &mut packets,
        );
        packets.truncate(100);
        let expected_packets = restream::test_fixtures::count_ts_feedable_packets(
            &video,
            &audio_tracks,
            &packets,
            video_sh.as_ref(),
        );

        engine
            .update_ingest_meta(
                "rtmp-h264",
                Some(video.clone()),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("rtmp-h264", audio_tracks.clone())
            .await;
        if let Some(video_sequence_header) = video_sh {
            engine
                .cache_sequence_header("rtmp-h264", true, video_sequence_header)
                .await;
        }
        if let Some(audio_sequence_header) = audio_sh {
            engine
                .cache_sequence_header("rtmp-h264", false, audio_sequence_header)
                .await;
        }

        for packet in packets.iter().take(10) {
            source.push(packet.clone());
        }

        manager.spawn_stage(handle, source.clone(), None);

        tokio::time::sleep(Duration::from_millis(50)).await;

        for packet in packets.into_iter().skip(10) {
            source.push(packet);
        }
        source.mark_end_of_stream();

        let mut reader = Reader::new("rtmp_h264_stage".to_string(), output);
        let packets =
            collect_packets_with_deadline(&mut reader, expected_packets, Duration::from_secs(8))
                .await;

        cancel.cancel();

        assert!(
            packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "expected video packets from RTMP-shaped h264 source stage"
        );
        assert!(
            packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Audio),
            "expected audio packets from RTMP-shaped h264 source stage"
        );
    });
}
