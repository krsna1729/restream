//! Application-layer output preparation that turns persisted output settings
//! into the runtime ring and transcoder wiring owned by the media engine.

use crate::domain::output_spec::{EgressProtocol, VideoCodecKind};
use crate::domain::stage::{StageKey, StageKind};
use crate::media::engine::MediaEngine;
use crate::media::ring_buffer::RingBuffer;
use crate::types::Output;
use std::sync::Arc;

/// Prepare the output ring and return the terminal stage key for dependency
/// tracking.
pub async fn prepare_output_ring(
    engine: &Arc<MediaEngine>,
    output: &Output,
) -> (Arc<RingBuffer>, Option<StageKey>) {
    let source_buf = engine.get_or_create_pipeline(&output.pipeline_id).await;
    let ingest_video_codec = engine.ingest_video_codec(&output.pipeline_id).await;
    let ingest_is_hevc = ingest_video_codec
        .as_deref()
        .map(VideoCodecKind::from_codec_name)
        .is_some_and(VideoCodecKind::is_hevc);
    let ingest_codec_override =
        (EgressProtocol::from_url(&output.url).is_rtmp() && ingest_is_hevc).then_some("hevc");

    let plan = crate::planner::graph_plan::plan_pipeline_graph(
        &output.pipeline_id,
        ingest_video_codec.as_deref(),
        std::slice::from_ref(output),
        false,
        &engine.config.backend_policy,
    );

    let mut current_bufs = std::collections::HashMap::new();
    current_bufs.insert(
        StageKey::new(output.pipeline_id.as_str(), StageKind::Source),
        source_buf.clone(),
    );

    for stage in &plan.stages {
        if stage.kind == StageKind::Source {
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

    let terminal_key = plan.terminal_stage.clone();
    let terminal_buf = current_bufs
        .get(&terminal_key)
        .cloned()
        .unwrap_or(source_buf);

    (terminal_buf, Some(terminal_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::stage::StageKind;
    use crate::media::engine::VideoMeta;

    fn test_output(pipeline_id: &str, encoding: &str, url: &str) -> Output {
        Output {
            id: format!("{pipeline_id}-out"),
            pipeline_id: pipeline_id.to_string(),
            name: "Output".to_string(),
            url: url.to_string(),
            monitoring_url: None,
            desired_state: "running".to_string(),
            config: crate::domain::output_spec::OutputConfig::parse(encoding),
        }
    }

    #[tokio::test]
    async fn prepare_output_ring_reuses_source_ring_for_passthrough_output() {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("pipe-source").await;
        let output = test_output("pipe-source", "source", "srt://example:9000");

        let (ring, terminal_key) = prepare_output_ring(&engine, &output).await;

        assert!(Arc::ptr_eq(&source, &ring));
        assert_eq!(
            terminal_key,
            Some(StageKey::new("pipe-source", StageKind::source()))
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

        let (ring, terminal_key) = prepare_output_ring(&engine, &output).await;

        assert!(Arc::ptr_eq(&expected, &ring));
        assert_eq!(ring.codec_hint_str(), "h264");
        assert_eq!(
            terminal_key,
            Some(StageKey::new(
                "pipe-hevc",
                StageKind::codec_edge("hevc_to_h264", StageKind::source())
            ))
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

        let (ring_a, _) = prepare_output_ring(&engine, &output_a).await;
        let (ring_b, _) = prepare_output_ring(&engine, &output_b).await;
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
}
