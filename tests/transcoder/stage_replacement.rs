use std::sync::Arc;

use restream_core::metadata::AudioMeta;
use restream_domain::domain::stage::StageKind;
use restream_engine::engine::MediaEngine;

#[tokio::test]
async fn replacement_video_stage_preserves_codec_hint_and_audio_tracks() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("replacement-meta").await;
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
    let (first, _lease201) = engine
        .get_or_create_transcoder(
            "replacement-meta",
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
    engine.cleanup_pipeline_stages("replacement-meta").await;

    let (replacement, _lease202) = engine
        .get_or_create_transcoder("replacement-meta", stage_kind, source, Some("hevc"))
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
    engine.cleanup_pipeline_stages("replacement-meta").await;
}
