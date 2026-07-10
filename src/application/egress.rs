//! Application-layer output preparation that turns persisted output settings
//! into the runtime ring and transcoder wiring owned by the media engine.

use crate::domain::output_spec::{EgressProtocol, OutputUrlScheme, VideoCodecKind};
use crate::domain::stage::{StageKey, StageKind};
use crate::media::engine::MediaEngine;
use crate::media::ring_buffer::RingBuffer;
use crate::types::Output;
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

/// Prepare the output ring and graph terminal stage for dependency tracking.
pub async fn prepare_output_ring(engine: &Arc<MediaEngine>, output: &Output) -> PreparedOutput {
    let source_buf = engine.get_or_create_pipeline(&output.pipeline_id).await;
    let ingest_video_codec = engine.ingest_video_codec(&output.pipeline_id).await;
    let ingest_is_hevc = ingest_video_codec
        .as_deref()
        .map(VideoCodecKind::from_codec_name)
        .is_some_and(VideoCodecKind::is_hevc);
    let ingest_codec_override =
        (EgressProtocol::from_url(&output.url).is_rtmp() && ingest_is_hevc).then_some("hevc");

    let url_scheme = OutputUrlScheme::from_url(&output.url);
    let plan = if url_scheme.is_hls_family() {
        crate::planner::graph_plan::plan_hls_output_graph(
            &output.pipeline_id,
            ingest_video_codec.as_deref(),
            output,
            &engine.config.backend_policy,
        )
    } else {
        crate::planner::graph_plan::plan_pipeline_graph(
            &output.pipeline_id,
            ingest_video_codec.as_deref(),
            std::slice::from_ref(output),
            false,
            &engine.config.backend_policy,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::stage::StageKind;
    use crate::domain::state::DesiredOutputState;
    use crate::media::engine::VideoMeta;

    fn test_output(pipeline_id: &str, encoding: &str, url: &str) -> Output {
        Output {
            id: format!("{pipeline_id}-out"),
            pipeline_id: pipeline_id.to_string(),
            name: "Output".to_string(),
            url: url.to_string(),
            monitoring_url: None,
            desired_state: DesiredOutputState::Running,
            config: crate::domain::output_spec::OutputConfig::parse(encoding),
        }
    }

    #[tokio::test]
    async fn prepare_output_ring_reuses_source_ring_for_passthrough_output() {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("pipe-source").await;
        let output = test_output("pipe-source", "source", "srt://example:9000");

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
        let output = test_output("pipe-hevc", "source", "rtmp://example/live/test");

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
    async fn prepare_output_ring_shares_hevc_codec_edge_before_audio_selection() {
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

        let output_a = test_output("pipe-hevc-audio", "720p+atrack:0", "rtmp://example/live/a");
        let output_b = test_output("pipe-hevc-audio", "720p+atrack:1", "rtmp://example/live/b");

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
            1,
            "selected-audio RTMP outputs should share one HEVC->H.264 stage per video shape"
        );
        assert!(stages.iter().any(|(kind, active)| {
            *active
                && *kind == StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("720p"))
        }));
        assert_eq!(
            stages
                .iter()
                .filter(|(kind, active)| {
                    *active && matches!(kind, StageKind::AudioRoute { .. })
                })
                .count(),
            2,
            "audio selection should happen after the shared codec edge"
        );
    }

    #[tokio::test]
    async fn prepare_hls_output_ring_reports_protocol_segmenter_terminal() {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("pipe-hls-output").await;
        let output = test_output(
            "pipe-hls-output",
            "source",
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
        use crate::media::ring_buffer::{MediaType, Reader};
        use crate::planner::backend_policy::BackendPolicy;
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

        let output = test_output(pipeline_id, "720p+atrack:0", "rtmp://example/live/selected");
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
