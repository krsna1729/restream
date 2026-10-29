//! The ingest registration's cancel safety: a caller whose future is dropped
//! inside it must leave no half-written state behind.

use std::future::Future;
use std::task::{Context, Waker};

use super::*;

/// A registration waits on a `RwLock` write, and the input-status JSON holds
/// that lock as a reader, so a caller can be dropped at that await — a pull's
/// cancel lands exactly there. It must leave neither a token nothing will
/// cancel (the input would read as connected until the next registration) nor
/// a session with no owner.
#[tokio::test]
async fn a_registration_dropped_at_a_write_guard_leaves_nothing_behind() {
    let engine = MediaEngine::new();
    let held = engine.ingests.sessions.read().await;
    let mut attempt =
        Box::pin(engine.try_register_pipeline_input_attempt("pipe", "input", "key", "hls", true));
    assert!(
        attempt
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the registration must wait for the held reader"
    );
    drop(attempt);
    drop(held);

    assert!(
        !engine.input_has_session("pipe", "input").await,
        "a dropped registration left a session behind"
    );
    assert!(
        engine
            .try_register_pipeline_input_attempt("pipe", "input", "key", "hls", true)
            .await
            .is_some(),
        "a dropped registration left a token that blocks the input"
    );
}

/// A registration that replaces the active input demotes it before it takes its
/// write guards. A caller dropped after the demote must leave that input
/// forwarding as it was, not registered and silent.
#[tokio::test]
async fn a_registration_dropped_after_the_demote_restores_the_previous_input() {
    let engine = MediaEngine::new();
    engine
        .try_register_pipeline_input_attempt("pipe", "first", "key-1", "hls", true)
        .await
        .expect("the first input registers");
    let previous = engine
        .ingests
        .active
        .read()
        .await
        .get("pipe")
        .cloned()
        .expect("the first input is active");
    let before = previous.gate.state();
    engine
        .ingests
        .selected_inputs
        .write()
        .await
        .insert("pipe".to_string(), "second".to_string());

    // Park the replacement on `timelines`, which it takes after the demote.
    let held = engine.ingests.timelines.read().await;
    let mut attempt = Box::pin(
        engine.try_register_pipeline_input_attempt("pipe", "second", "key-2", "hls", true),
    );
    assert!(
        attempt
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the replacement must wait for the held reader"
    );
    assert_ne!(
        previous.gate.state(),
        before,
        "the replacement demoted the previous input before parking"
    );
    drop(attempt);
    drop(held);

    assert_eq!(
        previous.gate.state(),
        before,
        "a dropped replacement left the previous input demoted"
    );
}

#[tokio::test]
async fn q098_authoritative_tracks_follow_selection_and_reject_replaced_attempts() {
    use restream_core::metadata::{AudioMeta, VideoMeta};

    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("pipe").await;
    let first = engine
        .try_register_pipeline_input_attempt("pipe", "first", "key1", "srt", true)
        .await
        .unwrap();
    let backup = engine
        .try_register_pipeline_input_attempt("pipe", "backup", "key2", "srt", false)
        .await
        .unwrap();
    let video = VideoMeta {
        codec: "h264".to_string(),
        ..VideoMeta::default()
    };
    let audio = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        ..AudioMeta::default()
    };
    engine
        .replace_ingest_session_tracks(&first, Some(video), vec![audio.clone()], 1, None)
        .await;
    ring.set_video_parameter_sets(vec![1, 2, 3]);
    engine
        .replace_ingest_session_tracks(&backup, None, vec![audio], 0, None)
        .await;
    assert_eq!(
        ring.codec_hint_str(),
        "h264",
        "standby cannot clear selection"
    );
    assert!(ring.video_config().is_some());

    assert!(engine.select_pipeline_input("pipe", "backup").await);
    assert!(
        ring.codec_hint_str().is_empty(),
        "promotion installs known absence"
    );
    assert!(ring.video_config().is_none());
    assert!(ring.audio_tracks().is_some());
    backup.cancel_token.cancel();
    let replacement = engine
        .try_register_pipeline_input_attempt("pipe", "backup", "key3", "srt", true)
        .await
        .unwrap();
    engine
        .replace_ingest_session_tracks(
            &replacement,
            Some(VideoMeta {
                codec: "h264".to_string(),
                ..VideoMeta::default()
            }),
            Vec::new(),
            1,
            None,
        )
        .await;
    // A late old-session removal must not erase the replacement's video.
    engine
        .replace_ingest_session_tracks(&backup, None, Vec::new(), 0, None)
        .await;
    assert_eq!(ring.codec_hint_str(), "h264");
    let metadata = engine
        .with_ingest_session(&replacement, |ingest| ingest.metadata())
        .await
        .unwrap();
    assert!(metadata.video.is_some());
    assert!(metadata.audio.is_none());
    assert!(ring.audio_tracks().is_none());
}

#[tokio::test]
async fn q098_authoritative_snapshot_clears_an_obsolete_sequence_header() {
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("pipe").await;
    let registration = engine
        .try_register_pipeline_input_attempt("pipe", "input", "key", "srt", true)
        .await
        .unwrap();
    let video = |codec: &str| restream_core::metadata::VideoMeta {
        codec: codec.to_string(),
        ..Default::default()
    };
    engine
        .replace_ingest_session_tracks(
            &registration,
            Some(video("h264")),
            Vec::new(),
            1,
            Some(bytes::Bytes::from_static(b"avc")),
        )
        .await;
    assert!(
        engine
            .get_ingest_session_sequence_headers(&registration)
            .await
            .0
            .is_some()
    );
    engine
        .replace_ingest_session_tracks(&registration, Some(video("hevc")), Vec::new(), 1, None)
        .await;
    assert!(
        engine
            .get_ingest_session_sequence_headers(&registration)
            .await
            .0
            .is_none()
    );
}
