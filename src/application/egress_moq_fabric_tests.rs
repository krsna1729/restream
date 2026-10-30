use super::*;

use bytes::Bytes;

use crate::application::egress::prepare_output_ring;
use restream_core::codec::build_avcc_sequence_header;
use restream_core::metadata::AudioMeta;
use restream_domain::domain::output_spec::{OutputConfig, VideoCodecKind};
use restream_domain::domain::state::DesiredOutputState;
use restream_engine::egress::EgressFeed;
use restream_engine::moq::egress::startup::startup_video;
use restream_engine::moq::egress::startup::{MoqAudioStartup, MoqVideoStartup};
use restream_moq::media::AudioConfig;
use restream_moq::media::TrackContainer;
use restream_moq::media::VideoCodec;
use restream_moq::media::VideoConfig;
use restream_sync::RingBuffer;

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

#[test]
fn moq_ca_directory_confines_url_paths() {
    let dir = ca_test_directory("boundary");
    let root = dir.join("roots");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("relay.pem"), b"allowed CA").unwrap();
    std::fs::write(dir.join("secret.pem"), b"secret content must not escape").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let load = |url: &str, root: Option<&Path>| {
        let parsed = parse_moq_url(url).unwrap();
        runtime.block_on(load_moq_ca_file(
            parsed.ca_file,
            root.map(|root| root.as_os_str().to_owned()),
        ))
    };
    assert_eq!(
        load("moqt://relay/live?ca=relay.pem", Some(&root)).unwrap(),
        Some(b"allowed CA".to_vec())
    );
    for ca in [
        "../secret.pem".to_string(),
        "%2e%2e%2fsecret.pem".to_string(),
        "sub/../relay.pem".to_string(),
        dir.join("secret.pem").display().to_string(),
        root.join("relay.pem").display().to_string(),
    ] {
        let error = load(&format!("moqt://relay/live?ca={ca}"), Some(&root)).unwrap_err();
        assert!(error.contains("relative"), "{error}");
        assert!(!error.contains("secret content"), "{error}");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.join("secret.pem"), root.join("escape.pem")).unwrap();
        std::os::unix::fs::symlink(&dir, root.join("escape-dir")).unwrap();
        for ca in ["escape.pem", "escape-dir/secret.pem"] {
            let error = load(&format!("moqt://relay/live?ca={ca}"), Some(&root)).unwrap_err();
            assert!(error.contains("outside"), "{error}");
        }
    }
    for root in [None, Some(Path::new(""))] {
        let error = load("moqt://relay/live?ca=relay.pem&insecure=1", root).unwrap_err();
        assert!(error.contains("RESTREAM_MOQ_CA_DIR"), "{error}");
        assert_eq!(load("moqt://relay/live", root).unwrap(), None);
    }
    let error = load("moqt://relay/live?ca=missing.pem", Some(&root)).unwrap_err();
    assert_eq!(
        error,
        "MoQ output ca file is unavailable in the configured CA directory"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

fn ca_test_directory(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("restream-moq-ca-{name}-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    dir
}

#[test]
fn moq_ca_directory_trusted_tls_runtime_smoke() {
    use restream_engine::moq::transport;
    use restream_engine::moq::transport::test_support::{
        LOOPBACK_CERT, LOOPBACK_SERVER_NAME, Loopback, run,
    };
    use restream_moq::tls;

    let dir = ca_test_directory("tls");
    std::fs::write(dir.join("relay.pem"), &LOOPBACK_CERT.0).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let pem = runtime
        .block_on(load_moq_ca_file(
            parse_moq_url("moqt://localhost/live?ca=relay.pem")
                .unwrap()
                .ca_file,
            Some(dir.as_os_str().to_owned()),
        ))
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    run(async move {
        let pair = Loopback::new().await;
        // A real QUIC/TLS handshake must trust only the explicitly loaded
        // private CA (no insecure override).
        let alpns = &["moq-lite-06"];
        let tls = tls::client_config(alpns, Some(&pem), false).unwrap();
        let (client, server) = futures_util::future::join(
            transport::connect(&pair.client, pair.addr, LOOPBACK_SERVER_NAME, tls),
            pair.accept(),
        )
        .await;
        let _client = client.unwrap_or_else(|error| panic!("trusted TLS client: {error}"));
        let _server = server.unwrap_or_else(|error| panic!("trusted TLS server: {error}"));
        // No CA still builds with the normal public trust store, and the
        // existing explicit insecure flag still bypasses chain parsing.
        assert!(tls::client_config(alpns, None, false).is_ok());
        assert!(tls::client_config(alpns, Some(b"not a CA"), true).is_ok());
        assert!(tls::client_config(alpns, Some(b"not a CA"), false).is_err());
    });
}

#[tokio::test]
async fn startup_reads_the_ca_only_from_the_configured_directory() {
    let dir = ca_test_directory("startup");
    std::fs::write(dir.join("relay.pem"), b"configured CA").unwrap();
    let output = test_output("pipe-ca-startup", "moqt://relay.example/live?ca=relay.pem");

    let mut engine = MediaEngine::new();
    engine.config = Arc::new(restream_engine::config::EngineConfig {
        moq_ca_dir: None,
        ..Default::default()
    });
    let engine = Arc::new(engine);
    let prepared = prepare_output_ring(&engine, &output).await;
    let error = prepare_moq_fabric_startup(&engine, &output, &prepared)
        .await
        .unwrap_err();
    assert!(error.contains("RESTREAM_MOQ_CA_DIR"), "{error}");

    let mut engine = MediaEngine::new();
    engine.config = Arc::new(restream_engine::config::EngineConfig {
        moq_ca_dir: Some(dir.display().to_string()),
        ..Default::default()
    });
    let engine = Arc::new(engine);
    let startup = prepare_moq_fabric_startup(&engine, &output, &prepared)
        .await
        .unwrap();
    assert_eq!(startup.extra_ca_pem.as_deref(), Some(&b"configured CA"[..]));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn startup_rejects_an_invalid_moq_url() {
    let engine = Arc::new(MediaEngine::new());
    let invalid = test_output("pipe-moq-ca", "moqt://relay.example/live?broadcast=");
    let prepared = prepare_output_ring(&engine, &invalid).await;
    assert!(
        prepare_moq_fabric_startup(&engine, &invalid, &prepared)
            .await
            .is_err()
    );
}
