use super::*;

use bytes::Bytes;

use crate::application::egress::prepare_output_ring;
use crate::domain::output_spec::{OutputConfig, VideoCodecKind};
use crate::domain::state::DesiredOutputState;
use crate::media::codec::build_avcc_sequence_header;
use crate::media::egress::EgressFeed;
use crate::media::metadata::AudioMeta;
use crate::media::moq::egress::startup::startup_video;
use crate::media::moq::egress::startup::{MoqAudioStartup, MoqVideoStartup};
use crate::media::moq::media::{AudioConfig, TrackContainer, VideoCodec, VideoConfig};
use crate::media::ring_buffer::RingBuffer;

fn test_output(pipeline_id: &str, url: &str) -> Output {
    Output {
        id: format!("{pipeline_id}-out"),
        pipeline_id: pipeline_id.to_string(),
        name: "Output".to_string(),
        url: url.to_string(),
        monitoring_url: None,
        desired_state: DesiredOutputState::Running,
        retries_exhausted: false,
        config: OutputConfig::source(),
    }
}

/// Baseline 3.0 SPS (320x240) and a PPS, Annex-B.
const PARAMETER_SETS: &[u8] = &[
    0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2,
];

#[tokio::test]
async fn moq_feed_and_spec_carry_the_prepared_ring_and_the_output_url() {
    let engine = Arc::new(MediaEngine::new());
    let output = test_output("pipe-moq-feed", "moqt://relay.example/live");
    let prepared = prepare_output_ring(&engine, &output).await;

    let feed = prepare_moq_fabric_feed(&prepared);
    let spec = moq_fabric_output_spec(&output, 7, feed.feed_id.clone());

    assert_eq!(feed.feed_id.as_str(), "moq:pipe-moq-feed:source");
    assert_eq!(feed.feed.head_sequence(), 0);
    assert_eq!(spec.id.as_str(), "pipe-moq-feed-out");
    assert_eq!(spec.generation, 7);
    assert_eq!(spec.feed, feed.feed_id);
    match spec.protocol {
        ProtocolSpec::Moq { url } => assert_eq!(url, "moqt://relay.example/live"),
        ProtocolSpec::Rtmp { .. }
        | ProtocolSpec::Srt { .. }
        | ProtocolSpec::Sink
        | ProtocolSpec::HlsPut { .. }
        | ProtocolSpec::Pipeline { .. } => panic!("a moqt output must carry the MoQ protocol"),
    }
}

#[tokio::test]
async fn startup_without_media_metadata_publishes_no_tracks_under_the_pipeline_id() {
    let engine = Arc::new(MediaEngine::new());
    let output = test_output("pipe-moq-empty", "moqt://relay.example/live");
    let prepared = prepare_output_ring(&engine, &output).await;

    let startup = prepare_moq_fabric_startup(&engine, &output, &prepared)
        .await
        .unwrap();

    assert_eq!(
        startup,
        MoqPublishStartup {
            broadcast: "pipe-moq-empty".to_string(),
            ..MoqPublishStartup::default()
        }
    );
}

#[tokio::test]
async fn startup_captures_ring_h264_config_aac_track_and_broadcast_name() {
    let engine = Arc::new(MediaEngine::new());
    let output = test_output(
        "pipe-moq-ready",
        "moqt://relay.example/anon?broadcast=show%2Fmain",
    );
    let prepared = prepare_output_ring(&engine, &output).await;
    prepared.ring.set_codec_hint("h264");
    prepared
        .ring
        .set_video_parameter_sets(PARAMETER_SETS.to_vec());
    prepared.ring.set_audio_tracks(vec![
        AudioMeta {
            codec: "opus".to_string(),
            sample_rate: 48_000,
            channels: 2,
            track_index: 0,
            ..AudioMeta::default()
        },
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48_000,
            channels: 2,
            track_index: 1,
            ..AudioMeta::default()
        },
    ]);

    let startup = prepare_moq_fabric_startup(&engine, &output, &prepared)
        .await
        .unwrap();

    assert_eq!(startup.broadcast, "show/main");
    let video = startup.video.expect("an H.264 ring publishes video");
    assert_eq!(video.codec, VideoCodec::H264);
    let config = video
        .config
        .expect("the ring's parameter sets configure it");
    assert_eq!(
        config,
        VideoConfig::from_parameter_sets(VideoCodec::H264, PARAMETER_SETS).unwrap()
    );
    assert_eq!(
        config
            .parameter_sets()
            .map(|(sets, size)| (sets.to_vec(), size)),
        Some((PARAMETER_SETS.to_vec(), 4))
    );
    assert_eq!(
        startup.audio,
        Some(MoqAudioStartup {
            track_index: 1,
            config: AudioConfig {
                sample_rate: 48_000,
                channels: 2,
                // AAC-LC, 48 kHz (index 3), two channels.
                asc: Some(Bytes::from_static(&[0x11, 0x90])),
                container: TrackContainer::Legacy,
            },
        })
    );
    assert_eq!(startup.extra_ca_pem, None);
}

#[test]
fn a_fresh_transformed_ring_waits_for_its_own_parameter_sets() {
    let ring = RingBuffer::new(16);
    ring.set_codec_hint("h264");
    let ingest_header = build_avcc_sequence_header(PARAMETER_SETS).unwrap();

    assert_eq!(
        startup_video(
            VideoCodecKind::H264,
            None,
            &ring,
            Some(ingest_header.clone())
        ),
        Some(MoqVideoStartup {
            codec: VideoCodec::H264,
            config: None,
        })
    );

    // A ring with no codec hint is the source itself: the ingest header
    // describes it.
    let source = RingBuffer::new(16);
    let video = startup_video(VideoCodecKind::H264, None, &source, Some(ingest_header)).unwrap();
    assert_eq!(
        video.config,
        VideoConfig::from_parameter_sets(VideoCodec::H264, PARAMETER_SETS)
    );
    assert!(startup_video(VideoCodecKind::Unknown, None, &source, None).is_none());
}

#[tokio::test]
async fn startup_reads_the_ca_file_and_rejects_a_missing_one() {
    let engine = Arc::new(MediaEngine::new());
    let path = std::env::temp_dir().join(format!("moq-ca-{}.pem", std::process::id()));
    std::fs::write(&path, b"-----BEGIN CERTIFICATE-----\n").unwrap();
    let output = test_output(
        "pipe-moq-ca",
        &format!("moqt://relay.example/live?ca={}", path.display()),
    );
    let prepared = prepare_output_ring(&engine, &output).await;

    let startup = prepare_moq_fabric_startup(&engine, &output, &prepared)
        .await
        .unwrap();
    assert_eq!(
        startup.extra_ca_pem.as_deref(),
        Some(&b"-----BEGIN CERTIFICATE-----\n"[..])
    );

    std::fs::remove_file(&path).unwrap();
    let error = prepare_moq_fabric_startup(&engine, &output, &prepared)
        .await
        .unwrap_err();
    assert!(error.contains("ca file"), "{error}");

    let invalid = test_output("pipe-moq-ca", "moqt://relay.example/live?broadcast=");
    assert!(
        prepare_moq_fabric_startup(&engine, &invalid, &prepared)
            .await
            .is_err()
    );
}
