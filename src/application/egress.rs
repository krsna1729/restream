//! Application-layer output preparation that turns persisted output settings
//! into the runtime ring and transcoder wiring owned by the media engine.

use crate::application::models::Output;
use crate::domain::output_spec::{EgressProtocol, OutputUrlScheme, VideoCodecKind};
use crate::domain::stage::{StageKey, StageKind};
use crate::media::egress::journal::{FeedEpoch, TsFeed};
use crate::media::egress::policy::LeafPolicy;
use crate::media::egress::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::engine::MediaEngine;
use crate::media::ring_buffer::RingBuffer;
use crate::planner::PlannedOutput;
use std::sync::Arc;

/// Prepared runtime attachment point for an output.
pub struct PreparedOutput {
    /// Ring the protocol-specific sender reads from.
    pub ring: Arc<RingBuffer>,
    /// Media stage that feeds the protocol adapter.
    pub media_stage_key: StageKey,
    /// Terminal graph stage reported in dependency status.
    pub terminal_stage_key: StageKey,
}

pub struct PreparedSrtFabricFeed {
    pub feed_id: FeedId,
    pub feed: Arc<TsFeed>,
    pub muxer_stage_key: String,
}

/// Prepare the output ring and graph terminal stage for dependency tracking.
pub async fn prepare_output_ring(engine: &Arc<MediaEngine>, output: &Output) -> PreparedOutput {
    let source_buf = engine.get_or_create_pipeline(&output.pipeline_id).await;
    let ingest_video_codec = engine
        .ingest_video_codec(&output.pipeline_id)
        .await
        .or_else(|| {
            let hint = source_buf.codec_hint_str();
            (!hint.is_empty()).then_some(hint.to_string())
        });
    let ingest_is_hevc = ingest_video_codec
        .as_deref()
        .map(VideoCodecKind::from_codec_name)
        .is_some_and(VideoCodecKind::is_hevc);
    let output_protocol = EgressProtocol::from_url(&output.url);
    let ingest_codec_override = (output_protocol.is_rtmp() && ingest_is_hevc).then_some("hevc");

    let url_scheme = OutputUrlScheme::from_url(&output.url);
    let backend_policy = engine.backend_policy();
    let planned_output = PlannedOutput::new(
        output.id.as_str(),
        output.config.clone(),
        output.url.as_str(),
    );
    let plan = if url_scheme.is_hls_family() {
        crate::planner::plan_hls_output_graph(
            &output.pipeline_id,
            ingest_video_codec.as_deref(),
            &planned_output,
            &backend_policy,
        )
    } else {
        crate::planner::plan_pipeline_graph(
            &output.pipeline_id,
            ingest_video_codec.as_deref(),
            std::slice::from_ref(&planned_output),
            false,
            &backend_policy,
        )
    };

    let mut current_bufs = std::collections::HashMap::new();
    current_bufs.insert(
        StageKey::new(output.pipeline_id.as_str(), StageKind::Source),
        source_buf.clone(),
    );

    for stage in &plan.stages {
        if stage.kind == StageKind::Source {
            continue;
        }
        if matches!(stage.kind, StageKind::Hls | StageKind::HlsSegmenter { .. }) {
            continue;
        }

        let input_key = stage.input.as_ref().unwrap();
        let input_buf = current_bufs
            .get(input_key)
            .cloned()
            .unwrap_or_else(|| source_buf.clone());

        let stage_buf = match &stage.kind {
            StageKind::VideoPreset { .. } => {
                engine
                    .get_or_create_transcoder(
                        &output.pipeline_id,
                        stage.kind.clone(),
                        input_buf,
                        ingest_codec_override,
                    )
                    .await
            }
            StageKind::CodecEdge { operation, .. } if operation == "hevc_to_h264" => {
                engine
                    .get_or_create_h264_transcoder(
                        &output.pipeline_id,
                        input_key.kind.clone(),
                        input_buf,
                    )
                    .await
            }
            _ => {
                engine
                    .get_or_create_transcoder(
                        &output.pipeline_id,
                        stage.kind.clone(),
                        input_buf,
                        None,
                    )
                    .await
            }
        };

        current_bufs.insert(stage.key.clone(), stage_buf);
    }

    let terminal_stage_key = plan.terminal_stage.clone();
    let media_stage_key = match &terminal_stage_key.kind {
        StageKind::Hls | StageKind::HlsSegmenter { .. } => plan
            .stages
            .iter()
            .find(|stage| stage.key == terminal_stage_key)
            .and_then(|stage| stage.input.clone())
            .unwrap_or_else(|| StageKey::new(output.pipeline_id.as_str(), StageKind::Source)),
        _ => terminal_stage_key.clone(),
    };
    let terminal_buf = current_bufs
        .get(&media_stage_key)
        .cloned()
        .unwrap_or(source_buf);

    PreparedOutput {
        ring: terminal_buf,
        media_stage_key,
        terminal_stage_key,
    }
}

pub async fn prepare_srt_fabric_feed(
    engine: &Arc<MediaEngine>,
    output: &Output,
    prepared: &PreparedOutput,
    attempt_id: u64,
) -> PreparedSrtFabricFeed {
    let encoding = prepared.media_stage_key.kind.to_string();
    let muxer_stage_key = engine
        .assign_srt_egress_muxer_stage(&output.pipeline_id, &encoding, &output.id, attempt_id)
        .await;
    let shared_muxer = engine
        .get_or_create_ts_muxer_stage(&output.pipeline_id, &muxer_stage_key, prepared.ring.clone())
        .await;
    let feed_id = FeedId::new(format!("srt:{}:{muxer_stage_key}", output.pipeline_id));

    PreparedSrtFabricFeed {
        feed_id,
        feed: Arc::new(TsFeed::new(&shared_muxer, Arc::new(FeedEpoch::new()))),
        muxer_stage_key,
    }
}

pub fn srt_fabric_output_spec(output: &Output, generation: u64, feed_id: FeedId) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(output.id.clone()),
        generation,
        feed: feed_id,
        protocol: ProtocolSpec::Srt {
            url: output.url.clone(),
        },
        policy: LeafPolicy::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::audio_routing::AudioRouting;
    use crate::domain::output_spec::{OutputConfig, RtmpOutputMode};
    use crate::domain::stage::StageKind;
    use crate::domain::state::DesiredOutputState;
    use crate::media::egress::EgressFeed;
    use crate::media::metadata::VideoMeta;

    fn test_output(pipeline_id: &str, config: OutputConfig, url: &str) -> Output {
        Output {
            id: format!("{pipeline_id}-out"),
            pipeline_id: pipeline_id.to_string(),
            name: "Output".to_string(),
            url: url.to_string(),
            monitoring_url: None,
            desired_state: DesiredOutputState::Running,
            config,
        }
    }

    fn test_output_with_rtmp_mode(
        pipeline_id: &str,
        config: OutputConfig,
        url: &str,
        rtmp_mode: RtmpOutputMode,
    ) -> Output {
        test_output(pipeline_id, config.with_rtmp_mode(rtmp_mode), url)
    }

    #[tokio::test]
    async fn prepare_output_ring_reuses_source_ring_for_passthrough_output() {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("pipe-source").await;
        let output = test_output("pipe-source", OutputConfig::source(), "srt://example:9000");

        let prepared = prepare_output_ring(&engine, &output).await;

        assert!(Arc::ptr_eq(&source, &prepared.ring));
        assert_eq!(
            prepared.media_stage_key,
            StageKey::new("pipe-source", StageKind::source())
        );
        assert_eq!(
            prepared.terminal_stage_key,
            StageKey::new("pipe-source", StageKind::source())
        );
    }

    #[tokio::test]
    async fn prepare_srt_fabric_feed_uses_shared_muxer_assignment_identity() {
        let engine = Arc::new(MediaEngine::new_with_config(Arc::new(crate::AppConfig {
            srt_egress_muxer_max_outputs_per_shard: 2,
            srt_egress_muxer_max_shards: 8,
            ..Default::default()
        })));
        let mut first = test_output(
            "pipe-srt-fabric-feed",
            OutputConfig::source(),
            "srt://example:9000",
        );
        first.id = "out-1".to_string();
        let mut second = test_output(
            "pipe-srt-fabric-feed",
            OutputConfig::source(),
            "srt://example:9001",
        );
        second.id = "out-2".to_string();
        let mut third = test_output(
            "pipe-srt-fabric-feed",
            OutputConfig::source(),
            "srt://example:9002",
        );
        third.id = "out-3".to_string();

        let first_prepared = prepare_output_ring(&engine, &first).await;
        let second_prepared = prepare_output_ring(&engine, &second).await;
        let third_prepared = prepare_output_ring(&engine, &third).await;

        let first_feed = prepare_srt_fabric_feed(&engine, &first, &first_prepared, 1).await;
        let second_feed = prepare_srt_fabric_feed(&engine, &second, &second_prepared, 1).await;
        let third_feed = prepare_srt_fabric_feed(&engine, &third, &third_prepared, 1).await;

        assert_eq!(first_feed.muxer_stage_key, "source:srt-mux-shard:0");
        assert_eq!(second_feed.muxer_stage_key, "source:srt-mux-shard:0");
        assert_eq!(third_feed.muxer_stage_key, "source:srt-mux-shard:1");
        assert_eq!(
            first_feed.feed_id.as_str(),
            "srt:pipe-srt-fabric-feed:source:srt-mux-shard:0"
        );
        assert_eq!(first_feed.feed_id, second_feed.feed_id);
        assert_ne!(first_feed.feed_id, third_feed.feed_id);
        assert_eq!(first_feed.feed.head_sequence(), 0);
    }

    #[test]
    fn srt_fabric_output_spec_uses_output_identity_and_prepared_feed() {
        let output = test_output(
            "pipe-srt-fabric-spec",
            OutputConfig::source(),
            "srt://localhost:9000?mode=caller",
        );
        let spec = srt_fabric_output_spec(&output, 7, FeedId::new("feed-srt-source"));

        assert_eq!(spec.id.as_str(), "pipe-srt-fabric-spec-out");
        assert_eq!(spec.generation, 7);
        assert_eq!(spec.feed.as_str(), "feed-srt-source");
        match spec.protocol {
            crate::media::egress::ProtocolSpec::Srt { url } => {
                assert_eq!(url, "srt://localhost:9000?mode=caller");
            }
            crate::media::egress::ProtocolSpec::Rtmp { .. }
            | crate::media::egress::ProtocolSpec::Sink => {
                panic!("SRT fabric spec must carry the SRT protocol")
            }
        }
    }

    #[tokio::test]
    async fn prepare_output_ring_reuses_hevc_source_for_enhanced_rtmp() {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("pipe-enhanced-rtmp").await;
        source.set_codec_hint("hevc");
        let output = test_output_with_rtmp_mode(
            "pipe-enhanced-rtmp",
            OutputConfig::source(),
            "rtmp://example/live",
            RtmpOutputMode::Enhanced,
        );

        let prepared = prepare_output_ring(&engine, &output).await;

        assert!(Arc::ptr_eq(&source, &prepared.ring));
        assert_eq!(
            prepared.media_stage_key,
            StageKey::new("pipe-enhanced-rtmp", StageKind::source())
        );
    }

    #[tokio::test]
    async fn prepare_output_ring_routes_hevc_rtmp_through_shared_h264_stage() {
        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-hevc", "stream-key", "rtmp")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                "pipe-hevc",
                Some(VideoMeta {
                    codec: "hevc".to_string(),
                    ..Default::default()
                }),
                None,
                None,
            )
            .await;
        let source = engine.get_or_create_pipeline("pipe-hevc").await;
        let expected = engine
            .get_or_create_h264_transcoder("pipe-hevc", StageKind::source(), source)
            .await;
        let output = test_output(
            "pipe-hevc",
            OutputConfig::source(),
            "rtmp://example/live/test",
        );

        let prepared = prepare_output_ring(&engine, &output).await;

        assert!(Arc::ptr_eq(&expected, &prepared.ring));
        assert_eq!(prepared.ring.codec_hint_str(), "h264");
        assert_eq!(
            prepared.media_stage_key,
            StageKey::new(
                "pipe-hevc",
                StageKind::codec_edge("hevc_to_h264", StageKind::source())
            )
        );
        assert_eq!(
            prepared.terminal_stage_key,
            StageKey::new(
                "pipe-hevc",
                StageKind::codec_edge("hevc_to_h264", StageKind::source())
            )
        );
    }

    #[tokio::test]
    async fn prepare_output_ring_shares_legacy_rtmp_h264_preset_before_audio_selection() {
        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-hevc-audio", "stream-key", "srt")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                "pipe-hevc-audio",
                Some(VideoMeta {
                    codec: "hevc".to_string(),
                    ..Default::default()
                }),
                None,
                None,
            )
            .await;

        let output_a = test_output(
            "pipe-hevc-audio",
            OutputConfig::preset("720p").with_audio(AudioRouting::SelectTracks { tracks: vec![0] }),
            "rtmp://example/live/a",
        );
        let output_b = test_output(
            "pipe-hevc-audio",
            OutputConfig::preset("720p").with_audio(AudioRouting::SelectTracks { tracks: vec![1] }),
            "rtmp://example/live/b",
        );

        let ring_a = prepare_output_ring(&engine, &output_a).await.ring;
        let ring_b = prepare_output_ring(&engine, &output_b).await.ring;
        let stages = engine.active_transcoder_stages("pipe-hevc-audio").await;

        assert!(
            !Arc::ptr_eq(&ring_a, &ring_b),
            "different selected audio tracks must remain distinct terminal rings"
        );
        assert_eq!(
            stages
                .iter()
                .filter(|(kind, active)| { *active && matches!(kind, StageKind::CodecEdge { .. }) })
                .count(),
            0,
            "legacy RTMP preset auto resolves to an H.264 preset stage without a bridge"
        );
        assert!(stages.iter().any(|(kind, active)| {
            *active && *kind == StageKind::video_preset_with_codec("720p", "h264")
        }));
        assert_eq!(
            stages
                .iter()
                .filter(|(kind, active)| {
                    *active && matches!(kind, StageKind::AudioRoute { .. })
                })
                .count(),
            2,
            "audio selection should happen after the shared H.264 video preset"
        );
    }

    #[tokio::test]
    async fn h265_srt_and_legacy_rtmp_presets_use_separate_codec_stages() {
        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-hevc-mixed", "stream-key", "file")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                "pipe-hevc-mixed",
                Some(VideoMeta {
                    codec: "hevc".to_string(),
                    ..Default::default()
                }),
                None,
                None,
            )
            .await;

        let srt = test_output(
            "pipe-hevc-mixed",
            OutputConfig::preset("720p"),
            "srt://example:9000",
        );
        let rtmp = test_output(
            "pipe-hevc-mixed",
            OutputConfig::preset("720p").with_audio(AudioRouting::SelectTracks { tracks: vec![0] }),
            "rtmp://example/live/a",
        );

        let srt_ring = prepare_output_ring(&engine, &srt).await.ring;
        let rtmp_ring = prepare_output_ring(&engine, &rtmp).await.ring;
        let stages = engine.active_transcoder_stages("pipe-hevc-mixed").await;

        assert!(!Arc::ptr_eq(&srt_ring, &rtmp_ring));
        assert!(
            !stages
                .iter()
                .any(|(kind, active)| { *active && *kind == StageKind::video_preset("720p") }),
            "HEVC input must not create a second plain scaled-video stage"
        );
        assert!(stages.iter().any(|(kind, active)| {
            *active && *kind == StageKind::video_preset_with_codec("720p", "hevc")
        }));
        assert!(stages.iter().any(|(kind, active)| {
            *active && *kind == StageKind::video_preset_with_codec("720p", "h264")
        }));
        assert!(
            !stages
                .iter()
                .any(|(kind, active)| { *active && matches!(kind, StageKind::CodecEdge { .. }) })
        );
    }

    #[tokio::test]
    async fn prepare_output_ring_uses_source_codec_hint_before_ingest_meta() {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("pipe-hevc-hint").await;
        source.set_codec_hint("hevc");
        let output = test_output(
            "pipe-hevc-hint",
            OutputConfig::preset("720p").with_audio(AudioRouting::SelectTracks { tracks: vec![0] }),
            "rtmp://example/live/hint",
        );

        let prepared = prepare_output_ring(&engine, &output).await;

        assert_eq!(
            prepared.terminal_stage_key,
            StageKey::new(
                "pipe-hevc-hint",
                StageKind::audio_route(
                    "atrack:0",
                    StageKind::video_preset_with_codec("720p", "h264"),
                )
            )
        );
    }

    #[tokio::test]
    async fn prepare_output_ring_falls_back_gracefully_for_unrecognized_url_scheme() {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("pipe-bad-url").await;
        let output = test_output("pipe-bad-url", OutputConfig::source(), "not-a-valid-url");

        let prepared = prepare_output_ring(&engine, &output).await;

        assert!(Arc::ptr_eq(&source, &prepared.ring));
        assert_eq!(
            prepared.media_stage_key,
            StageKey::new("pipe-bad-url", StageKind::source())
        );
        assert_eq!(
            prepared.terminal_stage_key,
            StageKey::new("pipe-bad-url", StageKind::source())
        );
    }

    #[tokio::test]
    async fn prepare_hls_output_ring_reports_protocol_segmenter_terminal() {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("pipe-hls-output").await;
        let output = test_output(
            "pipe-hls-output",
            OutputConfig::source(),
            "https://upload.example.test/live/out.m3u8",
        );

        let prepared = prepare_output_ring(&engine, &output).await;

        assert!(Arc::ptr_eq(&source, &prepared.ring));
        assert_eq!(
            prepared.media_stage_key,
            StageKey::new("pipe-hls-output", StageKind::source())
        );
        assert_eq!(
            prepared.terminal_stage_key,
            StageKey::new(
                "pipe-hls-output",
                StageKind::hls_segmenter(StageKind::source())
            )
        );
        let stages = engine.active_transcoder_stages("pipe-hls-output").await;
        assert!(
            stages.is_empty(),
            "protocol segmenter stages must not be spawned through the FFmpeg runtime"
        );
    }

    #[tokio::test]
    async fn hevc_rtmp_selected_audio_terminal_ring_makes_progress() {
        use crate::media::mpegts::TsDemuxer;
        use crate::media::packet::MediaType;
        use crate::media::ring_buffer::Reader;
        use crate::planner::BackendPolicy;
        use crate::test_fixtures::bench_transport_fixture;

        let path = bench_transport_fixture("h265", "1_5m", true)
            .expect("multi-audio HEVC benchmark fixture");
        let file_bytes = std::fs::read(path).expect("read multi-audio HEVC fixture");
        let mut demuxer = TsDemuxer::new();
        let mut packets = Vec::new();
        for chunk in file_bytes.chunks(1316) {
            demuxer.feed(chunk);
            demuxer.drain_into(&mut packets);
        }
        demuxer.flush();
        demuxer.drain_into(&mut packets);
        let probe = demuxer.take_probe().expect("probe multi-audio fixture");
        let video = probe.video.expect("fixture video metadata");
        let audio_tracks = probe.audio_tracks;
        assert!(
            audio_tracks.len() >= 2,
            "fixture must expose multiple audio tracks"
        );

        let pipeline_id = "pipe-hevc-selected-audio-progress";
        let engine = Arc::new(MediaEngine::new_with_config(Arc::new(crate::AppConfig {
            backend_policy: BackendPolicy {
                internal_video_presets: true,
                internal_hevc_to_h264: true,
                ..Default::default()
            },
            ..Default::default()
        })));
        engine
            .try_register_ingest(pipeline_id, "stream-key", "file")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                pipeline_id,
                Some(video),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks(pipeline_id, audio_tracks.clone())
            .await;

        let source = engine.get_or_create_pipeline(pipeline_id).await;
        source.set_codec_hint("hevc");
        source.set_audio_tracks(vec![audio_tracks[0].clone()]);
        let selected_packets = packets
            .into_iter()
            .filter(|packet| {
                packet.media_type == MediaType::Video
                    || (packet.media_type == MediaType::Audio && packet.track_index == 0)
            })
            .collect::<Vec<_>>();
        if let Some(parameter_sets) = selected_packets.iter().find_map(|packet| {
            (packet.media_type == MediaType::Video)
                .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
                .flatten()
        }) {
            source.set_video_parameter_sets(parameter_sets);
        }

        let output = test_output(
            pipeline_id,
            OutputConfig::preset("720p").with_audio(AudioRouting::SelectTracks { tracks: vec![0] }),
            "rtmp://example/live/selected",
        );
        let prepared = prepare_output_ring(&engine, &output).await;
        let terminal_ring = prepared.ring.clone();
        let mut reader =
            Reader::new_live("selected-audio-terminal".to_string(), terminal_ring.clone());

        let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let source_ready = source
                .reader_snapshots()
                .iter()
                .any(|snapshot| snapshot.name.contains("video:720p"));
            let terminal_ready = terminal_ring
                .reader_snapshots()
                .iter()
                .any(|snapshot| snapshot.name == "selected-audio-terminal");
            if source_ready && terminal_ready {
                break;
            }
            assert!(
                tokio::time::Instant::now() < ready_deadline,
                "stage readers did not attach in time"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        source.push_batch(selected_packets.into_iter());

        let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut saw_video = false;
        let mut saw_audio = false;
        loop {
            while let Ok(Some(packet)) = reader.pull() {
                saw_video |= packet.media_type == MediaType::Video;
                saw_audio |= packet.media_type == MediaType::Audio && packet.track_index == 0;
            }
            if saw_video && saw_audio {
                break;
            }
            assert!(
                tokio::time::Instant::now() < output_deadline,
                "{}",
                selected_audio_progress_failure(
                    &engine,
                    pipeline_id,
                    &source,
                    &terminal_ring,
                    saw_video,
                    saw_audio,
                )
                .await
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    async fn selected_audio_progress_failure(
        engine: &Arc<MediaEngine>,
        pipeline_id: &str,
        source: &Arc<RingBuffer>,
        terminal_ring: &Arc<RingBuffer>,
        saw_video: bool,
        saw_audio: bool,
    ) -> String {
        let mut snapshots = engine.pipeline_stage_runtime_snapshots(pipeline_id).await;
        snapshots.sort_by_key(|snapshot| snapshot.key.to_string());
        let stages = snapshots
            .into_iter()
            .map(|snapshot| {
                format!(
                    "{} phase={:?} packets_in={} packets_out={} bytes_in={} bytes_out={} last_error={:?}",
                    snapshot.key,
                    snapshot.phase,
                    snapshot.packets_in,
                    snapshot.packets_out,
                    snapshot.bytes_in,
                    snapshot.bytes_out,
                    snapshot.last_error,
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "selected-audio terminal ring did not emit video and selected audio: \
             saw_video={saw_video} saw_audio={saw_audio} \
             source_write={} terminal_write={} source_readers={:?} terminal_readers={:?} stages=[{}]",
            source.get_write_idx(),
            terminal_ring.get_write_idx(),
            source.reader_snapshots(),
            terminal_ring.reader_snapshots(),
            stages,
        )
    }
}
