use std::sync::Arc;

use bytes::Bytes;
use rml_rtmp::sessions::StreamMetadata;

use crate::application::egress::PreparedOutput;
use crate::application::models::Output;
use restream_core::metadata::AudioMeta;
use restream_domain::domain::output_spec::OutputUrlScheme;
use restream_engine::egress::journal::{FeedEpoch, RingFeed};
use restream_engine::egress::policy::LeafPolicy;
use restream_engine::egress::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use restream_engine::engine::MediaEngine;
use restream_engine::rtmp::output_ring_video_codec_kind;
use restream_engine::rtmp::resolved_output_audio_tracks;
use restream_engine::rtmp::rtmp_publish_metadata;
use restream_rtmp::should_defer_audio_until_video_ready;
use restream_rtmp::should_send_startup_audio_sequence_header;
use restream_rtmp::startup_video_sequence_header;
use restream_rtmp::validate_rtmp_output_audio_tracks;

pub struct PreparedRtmpFabricFeed {
    pub feed_id: FeedId,
    pub feed: Arc<RingFeed>,
}

#[derive(Debug, Clone)]
pub struct RtmpFabricStartup {
    pub enhanced_rtmp: bool,
    pub enhanced_hevc_video: bool,
    pub output_audio_track: Option<AudioMeta>,
    pub publish_metadata: Option<StreamMetadata>,
    pub startup_video_sequence_header: Option<Bytes>,
    /// Ring configuration generation `startup_video_sequence_header` was
    /// built from; 0 when it is the ingest header or absent.
    pub startup_video_generation: u32,
    pub startup_audio_sequence_header: Option<Bytes>,
    pub deferred_audio_sequence_header: Option<Bytes>,
    pub defer_audio_until_video_ready: bool,
}

impl From<RtmpFabricStartup> for restream_engine::egress::backends::rtmp::RtmpPublishStartup {
    fn from(startup: RtmpFabricStartup) -> Self {
        Self {
            enhanced_rtmp: startup.enhanced_rtmp,
            enhanced_hevc_video: startup.enhanced_hevc_video,
            output_audio_track: startup.output_audio_track,
            publish_metadata: startup.publish_metadata,
            startup_video_sequence_header: startup.startup_video_sequence_header,
            startup_video_generation: startup.startup_video_generation,
            startup_audio_sequence_header: startup.startup_audio_sequence_header,
            deferred_audio_sequence_header: startup.deferred_audio_sequence_header,
            defer_audio_until_video_ready: startup.defer_audio_until_video_ready,
        }
    }
}

pub fn prepare_rtmp_fabric_feed(prepared: &PreparedOutput) -> PreparedRtmpFabricFeed {
    let feed_id = FeedId::new(format!("rtmp:{}", prepared.media_stage_key));

    PreparedRtmpFabricFeed {
        feed_id,
        feed: Arc::new(RingFeed::new(
            prepared.ring.clone(),
            Arc::new(FeedEpoch::new()),
        )),
    }
}

pub async fn prepare_rtmp_fabric_startup(
    engine: &MediaEngine,
    output: &Output,
    prepared: &PreparedOutput,
) -> Result<RtmpFabricStartup, String> {
    let output_audio_tracks =
        resolved_output_audio_tracks(engine, &output.pipeline_id, &prepared.ring).await;
    validate_rtmp_output_audio_tracks(&output_audio_tracks)?;
    let output_audio_track = output_audio_tracks.into_iter().next();
    let enhanced_rtmp = output.config.rtmp_mode().is_enhanced();
    let enhanced_hevc_video = enhanced_rtmp
        && output_ring_video_codec_kind(engine, &output.pipeline_id, &prepared.ring)
            .await
            .is_hevc();
    let (ingest_video_sequence_header, mut audio_sequence_header) =
        engine.get_sequence_headers(&output.pipeline_id).await;
    if audio_sequence_header.is_none()
        && let Some(track) = output_audio_track.as_ref()
    {
        audio_sequence_header = track.codec.eq_ignore_ascii_case("aac").then(|| {
            restream_core::codec::build_aac_sequence_header(track.sample_rate, track.channels)
        });
    }
    let (startup_video_sequence_header, startup_video_generation) = startup_video_sequence_header(
        &prepared.ring,
        ingest_video_sequence_header,
        enhanced_hevc_video,
    )
    .unzip();
    let send_startup_audio = should_send_startup_audio_sequence_header(false, &prepared.ring);
    let startup_audio_sequence_header = send_startup_audio
        .then(|| audio_sequence_header.clone())
        .flatten();
    let deferred_audio_sequence_header = (!send_startup_audio)
        .then_some(audio_sequence_header)
        .flatten();

    Ok(RtmpFabricStartup {
        enhanced_rtmp,
        enhanced_hevc_video,
        publish_metadata: rtmp_publish_metadata(
            engine,
            &output.pipeline_id,
            &prepared.ring,
            output_audio_track.as_ref(),
        )
        .await,
        output_audio_track,
        startup_video_sequence_header,
        startup_video_generation: startup_video_generation.unwrap_or(0),
        startup_audio_sequence_header,
        deferred_audio_sequence_header,
        defer_audio_until_video_ready: should_defer_audio_until_video_ready(false, &prepared.ring),
    })
}

pub fn rtmp_fabric_output_spec(output: &Output, generation: u64, feed_id: FeedId) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(output.id.clone()),
        generation,
        feed: feed_id,
        protocol: ProtocolSpec::Rtmp {
            url: output.url.clone(),
            tls: matches!(
                OutputUrlScheme::from_url(&output.url),
                OutputUrlScheme::Rtmps
            ),
        },
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::application::egress::prepare_output_ring;
    use restream_core::metadata::AudioMeta;
    use restream_domain::domain::output_spec::{OutputConfig, RtmpOutputMode};
    use restream_domain::domain::state::DesiredOutputState;
    use restream_engine::egress::EgressFeed;
    use restream_engine::engine::MediaEngine;

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

    #[tokio::test]
    async fn prepare_rtmp_fabric_feed_wraps_prepared_terminal_ring() {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("pipe-rtmp-fabric-feed").await;
        let output = test_output("pipe-rtmp-fabric-feed", "rtmp://example/live/key");
        let prepared = prepare_output_ring(&engine, &output).await;

        let feed = prepare_rtmp_fabric_feed(&prepared);

        assert_eq!(feed.feed_id.as_str(), "rtmp:pipe-rtmp-fabric-feed:source");
        assert!(Arc::ptr_eq(&source, &prepared.ring));
        assert_eq!(feed.feed.head_sequence(), 0);
    }

    #[test]
    fn rtmp_fabric_output_spec_maps_plain_and_tls_schemes() {
        let plain = test_output("pipe-rtmp-fabric-spec", "rtmp://localhost/live/key");
        let tls = test_output("pipe-rtmps-fabric-spec", "rtmps://localhost/live/key");

        let plain_spec = rtmp_fabric_output_spec(&plain, 11, FeedId::new("feed-rtmp"));
        let tls_spec = rtmp_fabric_output_spec(&tls, 12, FeedId::new("feed-rtmps"));

        assert_eq!(plain_spec.id.as_str(), "pipe-rtmp-fabric-spec-out");
        assert_eq!(plain_spec.generation, 11);
        assert_eq!(plain_spec.feed.as_str(), "feed-rtmp");
        match plain_spec.protocol {
            ProtocolSpec::Rtmp { url, tls } => {
                assert_eq!(url, "rtmp://localhost/live/key");
                assert!(!tls);
            }
            ProtocolSpec::Srt { .. }
            | ProtocolSpec::Sink
            | ProtocolSpec::HlsPut { .. }
            | ProtocolSpec::Pipeline { .. }
            | ProtocolSpec::Moq { .. } => {
                panic!("plain RTMP fabric spec must carry the RTMP protocol")
            }
        }

        assert_eq!(tls_spec.id.as_str(), "pipe-rtmps-fabric-spec-out");
        assert_eq!(tls_spec.generation, 12);
        assert_eq!(tls_spec.feed.as_str(), "feed-rtmps");
        match tls_spec.protocol {
            ProtocolSpec::Rtmp { url, tls } => {
                assert_eq!(url, "rtmps://localhost/live/key");
                assert!(tls);
            }
            ProtocolSpec::Srt { .. }
            | ProtocolSpec::Sink
            | ProtocolSpec::HlsPut { .. }
            | ProtocolSpec::Pipeline { .. }
            | ProtocolSpec::Moq { .. } => {
                panic!("RTMPS fabric spec must carry the RTMP protocol with TLS")
            }
        }
    }

    #[tokio::test]
    async fn rtmp_fabric_startup_keeps_an_empty_source_header_free() {
        let engine = Arc::new(MediaEngine::new());
        let output = test_output("pipe-rtmp-startup-empty", "rtmp://example/live/key");
        let prepared = prepare_output_ring(&engine, &output).await;

        let startup = prepare_rtmp_fabric_startup(&engine, &output, &prepared)
            .await
            .expect("empty RTMP startup must remain valid");

        assert!(!startup.enhanced_rtmp);
        assert!(!startup.enhanced_hevc_video);
        assert_eq!(startup.startup_video_generation, 0);
        assert!(startup.publish_metadata.is_none());
        assert!(startup.startup_video_sequence_header.is_none());
        assert!(startup.startup_audio_sequence_header.is_none());
        assert!(startup.deferred_audio_sequence_header.is_none());
        assert!(!startup.defer_audio_until_video_ready);
    }

    #[tokio::test]
    async fn rtmp_fabric_startup_captures_prepared_h264_and_aac_state() {
        let engine = Arc::new(MediaEngine::new());
        let output = test_output("pipe-rtmp-startup-ready", "rtmp://example/live/key");
        let prepared = prepare_output_ring(&engine, &output).await;
        let parameter_sets = vec![
            0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce,
            0x06, 0xe2,
        ];
        prepared.ring.set_codec_hint("h264");
        prepared
            .ring
            .set_video_parameter_sets(parameter_sets.clone());
        prepared.ring.set_audio_tracks(vec![AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48_000,
            channels: 2,
            track_index: 0,
            ..AudioMeta::default()
        }]);

        let startup = prepare_rtmp_fabric_startup(&engine, &output, &prepared)
            .await
            .expect("prepared RTMP startup must remain valid");

        assert_eq!(
            Some(startup.startup_video_generation),
            prepared.ring.video_config().map(|config| config.generation)
        );
        assert_eq!(
            startup.output_audio_track.map(|track| track.track_index),
            Some(0)
        );
        assert!(startup.startup_video_sequence_header.is_some());
        assert_ne!(startup.startup_video_generation, 0);
        assert!(startup.startup_audio_sequence_header.is_some());
        assert!(startup.deferred_audio_sequence_header.is_none());
        assert!(startup.defer_audio_until_video_ready);
    }

    /// Enhanced mode advertises E-RTMP on connect for any codec; only HEVC
    /// video switches to FourCC packets.
    #[tokio::test]
    async fn enhanced_rtmp_startup_advertises_e_rtmp_for_h264_video() {
        let engine = Arc::new(MediaEngine::new());
        let mut output = test_output("pipe-rtmp-startup-enhanced", "rtmp://example/live/key");
        output.config = OutputConfig::source().with_rtmp_mode(RtmpOutputMode::Enhanced);
        let prepared = prepare_output_ring(&engine, &output).await;
        prepared.ring.set_codec_hint("h264");

        let startup = prepare_rtmp_fabric_startup(&engine, &output, &prepared)
            .await
            .expect("enhanced RTMP startup must remain valid");

        assert!(startup.enhanced_rtmp);
        assert!(!startup.enhanced_hevc_video);
    }
}
