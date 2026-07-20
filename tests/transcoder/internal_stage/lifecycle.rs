use std::sync::Arc;
use std::time::Duration;

use restream::domain::stage::{StageKey, StageKind};
use restream::media::engine::MediaEngine;
use restream::media::metadata::AudioMeta;
use restream::media::packet::MediaType;
use restream::media::ring_buffer::Reader;

use crate::support::{
    collect_packets_with_deadline, configure_ffmpeg_test_logging, load_primary_transport_packets,
};

#[tokio::test]
async fn replacement_video_stage_preserves_codec_hint_and_audio_tracks() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine
        .get_or_create_pipeline("internal-replacement-meta")
        .await;
    source.set_codec_hint("hevc");
    source.set_audio_tracks(vec![
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: 0,
            pid: Some(0x101),
            language: Some("eng".to_string()),
            title: None,
            profile: None,
        },
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: 1,
            pid: Some(0x102),
            language: Some("spa".to_string()),
            title: None,
            profile: None,
        },
    ]);

    let stage_kind = StageKind::video_preset("720p");
    let first = engine
        .get_or_create_transcoder(
            "internal-replacement-meta",
            stage_kind.clone(),
            source.clone(),
            Some("hevc"),
        )
        .await;

    assert_eq!(
        first.codec_hint_str(),
        "hevc",
        "initial replacement candidate should inherit hevc codec hint"
    );
    let first_tracks = first
        .audio_tracks()
        .expect("initial stage should expose audio tracks")
        .to_vec();
    assert_eq!(first_tracks.len(), 2);
    assert_eq!(first_tracks[0].pid, Some(0x101));
    assert_eq!(first_tracks[1].pid, Some(0x102));

    // Simulate registry cancellation/replacement.
    engine
        .cleanup_pipeline_stages("internal-replacement-meta")
        .await;

    let replacement = engine
        .get_or_create_transcoder(
            "internal-replacement-meta",
            stage_kind,
            source,
            Some("hevc"),
        )
        .await;

    assert!(
        !Arc::ptr_eq(&first, &replacement),
        "replacement stage must allocate a new ring buffer after cancellation"
    );
    assert_eq!(
        replacement.codec_hint_str(),
        "hevc",
        "replacement stage must preserve codec hint metadata"
    );

    let replacement_tracks = replacement
        .audio_tracks()
        .expect("replacement stage should expose audio tracks")
        .to_vec();
    assert_eq!(replacement_tracks.len(), 2);
    assert_eq!(replacement_tracks[0].track_index, 0);
    assert_eq!(replacement_tracks[1].track_index, 1);
    assert_eq!(replacement_tracks[0].pid, Some(0x101));
    assert_eq!(replacement_tracks[1].pid, Some(0x102));

    // Stop the replacement stage task before test teardown.
    engine
        .cleanup_pipeline_stages("internal-replacement-meta")
        .await;
}

#[test]
fn prebuffered_h264_packets_drive_internal_scaled_stage() {
    configure_ffmpeg_test_logging();
    let _ = tracing_subscriber::fmt::try_init();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(async {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("prebuffered-h264").await;
        let policy = restream::planner::BackendPolicy {
            internal_video_presets: true,
            ..restream::planner::BackendPolicy::default()
        };
        let manager = restream::media::stage_runtime::StageRuntimeManager::with_policy(
            engine.clone(),
            policy,
        );
        let stage_key = StageKey::new("prebuffered-h264", StageKind::video_preset("720p"));
        let (handle, is_new) = manager
            .ensure_stage(stage_key.clone(), source.clone(), None)
            .await;
        assert!(is_new);
        let output = handle.ring.clone();
        let cancel = handle.cancel.clone();

        engine
            .try_register_ingest("prebuffered-h264", "stream-key", "srt")
            .await
            .unwrap();

        let (video, audio_tracks, mut packets) = load_primary_transport_packets("h264");
        engine
            .update_ingest_meta(
                "prebuffered-h264",
                Some(video),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("prebuffered-h264", audio_tracks.clone())
            .await;
        source.set_codec_hint("h264");
        source.set_audio_tracks(audio_tracks);
        if let Some(parameter_sets) = packets.iter().find_map(|packet| {
            if packet.media_type != MediaType::Video {
                return None;
            }
            let header = restream::media::codec::build_avcc_sequence_header(&packet.payload)?;
            let (_, parameter_sets) = restream::media::codec::parse_avcc_config(&header[5..]);
            (!parameter_sets.is_empty()).then_some(parameter_sets)
        }) {
            source.set_video_parameter_sets(parameter_sets);
        }
        assert!(
            source.video_parameter_sets().is_some(),
            "fixture should seed source ring H.264 parameter sets for a late-start scaled stage"
        );
        source.push_batch(packets.drain(..));

        let mut reader = Reader::new("prebuffered_h264_internal_720p".to_string(), output.clone());
        manager.spawn_stage(handle, source.clone(), None);

        let packets = collect_packets_with_deadline(&mut reader, 20, Duration::from_secs(6)).await;

        cancel.cancel();

        let snapshot = manager.snapshot(&stage_key).await;
        assert!(
            packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "expected prebuffered H.264 packets to drive the internal scaled stage; snapshot={snapshot:?}"
        );
        assert!(
            matches!(
                snapshot.map(|snapshot| snapshot.phase),
                Some(restream::media::stage_lifecycle::StagePhase::FirstOutput)
                    | Some(restream::media::stage_lifecycle::StagePhase::Producing)
            ),
            "internal scaled stage should leave backendSpawned after emitting output"
        );
    });
}
