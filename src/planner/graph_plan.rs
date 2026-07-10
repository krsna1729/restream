//! Unified graph planner for stage pipeline graphs.
//!
//! Generates a single StageGraphPlan from output specifications and active
//! preview requests, providing a single source of truth for transcoders,
//! HLS previews, and recordings.

use crate::domain::ids::{OutputId, PipelineId};
use crate::domain::stage::{StageKey, StageKind};
use crate::planner::backend_policy::{BackendPolicy, StageBackend};
use crate::runtime::graph::{GraphRole, StageGraphPlan};
use crate::types::Output;

pub fn plan_pipeline_graph(
    pipeline_id: &str,
    ingest_codec: Option<&str>,
    outputs: &[Output],
    hls_preview_active: bool,
    policy: &BackendPolicy,
) -> StageGraphPlan {
    let pipeline_id_typed = PipelineId::new(pipeline_id);
    let terminal_stage = StageKey::new(pipeline_id_typed.clone(), StageKind::Source);
    let output_id_typed = outputs
        .first()
        .map(|o| OutputId::new(&o.id))
        .unwrap_or_else(|| OutputId::new(""));

    let mut plan = StageGraphPlan::new(
        pipeline_id_typed.clone(),
        GraphRole::Output {
            output_id: output_id_typed,
        },
        terminal_stage,
    );
    // 1. Source stage is always present
    plan.add_stage(
        StageKey::new(pipeline_id_typed.clone(), StageKind::Source),
        StageBackend::AudioRouter, // Source doesn't run a transcoder
    );

    // 2. Add outputs stages
    for (index, output) in outputs.iter().enumerate() {
        let encoding = output.encoding_string();
        let output_path = crate::application::output_path::OutputPath::resolve(
            pipeline_id,
            &encoding,
            &output.url,
        );
        if index == 0 {
            plan.terminal_stage = output_path.terminal_stage_key(ingest_codec);
        }

        for stage_key in output_path.needed_stage_keys(ingest_codec) {
            let backend = policy.select_backend(&stage_key.kind);
            plan.add_stage(stage_key, backend);
        }
    }

    if hls_preview_active
        && let Some(codec) = ingest_codec
        && (codec.eq_ignore_ascii_case("hevc") || codec.eq_ignore_ascii_case("h265"))
    {
        let preview_key = StageKey::new(
            pipeline_id_typed.clone(),
            StageKind::preview("720p", StageKind::source()),
        );
        let backend = policy.select_backend(&preview_key.kind);
        plan.add_stage(preview_key.clone(), backend);
        if outputs.is_empty() {
            plan.terminal_stage = preview_key;
        }
    }

    plan
}

/// Plan a persistent HLS output graph.
///
/// HLS output uses the same terminal media stages as RTMP/SRT egress, followed
/// by the protocol segmenter/uploader. Keeping a dedicated role lets runtime
/// and diagnostics distinguish persistent HLS output planning from generic
/// output and browser-preview planning without forking stage vocabulary.
pub fn plan_hls_output_graph(
    pipeline_id: &str,
    ingest_codec: Option<&str>,
    output: &Output,
    policy: &BackendPolicy,
) -> StageGraphPlan {
    let mut plan = plan_pipeline_graph(
        pipeline_id,
        ingest_codec,
        std::slice::from_ref(output),
        false,
        policy,
    );
    let media_terminal = plan.terminal_stage.clone();
    let hls_kind = StageKind::hls_segmenter(media_terminal.kind.clone());
    let hls_key = StageKey::new(PipelineId::new(pipeline_id), hls_kind.clone());
    plan.add_stage(hls_key.clone(), policy.select_backend(&hls_kind));
    plan.terminal_stage = hls_key;
    plan.role = GraphRole::HlsOutput {
        output_id: OutputId::new(&output.id),
    };
    plan
}

/// Plan the HLS preview graph for a pipeline.
///
/// This is a pure planning function — it does not create ring buffers or
/// spawn stages. The caller (`plan_hls_preview` in `hls_preview.rs`) uses
/// this plan to drive runtime execution.
pub fn plan_hls_preview_graph(
    pipeline_id: &str,
    ingest_codec: Option<&str>,
    policy: &BackendPolicy,
) -> Option<StageGraphPlan> {
    let codec = ingest_codec?;
    let pipeline_id_typed = PipelineId::new(pipeline_id);
    let source_kind = StageKind::source();
    let terminal_kind = if is_hevc_preview_codec(codec) {
        StageKind::hls_segmenter(StageKind::preview("720p", source_kind.clone()))
    } else {
        StageKind::hls_segmenter(source_kind.clone())
    };
    let terminal = StageKey::new(pipeline_id_typed.clone(), terminal_kind.clone());

    let mut plan = StageGraphPlan::new(pipeline_id_typed.clone(), GraphRole::HlsPreview, terminal);

    plan.add_stage(
        StageKey::new(pipeline_id_typed.clone(), source_kind.clone()),
        StageBackend::AudioRouter,
    );
    if is_hevc_preview_codec(codec) {
        let preview_key = StageKey::new(
            pipeline_id_typed.clone(),
            StageKind::preview("720p", source_kind),
        );
        let backend = policy.select_backend(&preview_key.kind);
        plan.add_stage(preview_key, backend);
    }
    let hls_backend = policy.select_backend(&terminal_kind);
    plan.add_stage(StageKey::new(pipeline_id_typed, terminal_kind), hls_backend);

    Some(plan)
}

/// Plan the recording graph for a pipeline.
///
/// Recordings consume the source stage and write an input-scoped media artifact.
/// The writer itself lives outside the FFmpeg stage runner, but its lifecycle
/// and diagnostics should still use the shared graph vocabulary.
pub fn plan_recording_graph(pipeline_id: &str, policy: &BackendPolicy) -> StageGraphPlan {
    let pipeline_id_typed = PipelineId::new(pipeline_id);
    let recording_key = StageKey::new(pipeline_id_typed.clone(), StageKind::recording());
    let mut plan = StageGraphPlan::new(
        pipeline_id_typed.clone(),
        GraphRole::Recording,
        recording_key.clone(),
    );

    plan.add_stage(
        StageKey::new(pipeline_id_typed, StageKind::Source),
        StageBackend::AudioRouter,
    );
    plan.add_stage(
        recording_key,
        policy.select_backend(&StageKind::recording()),
    );
    plan
}

fn is_hevc_preview_codec(codec: &str) -> bool {
    codec.eq_ignore_ascii_case("hevc") || codec.eq_ignore_ascii_case("h265")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::state::DesiredOutputState;

    #[test]
    fn plan_hls_preview_graph_returns_plan_for_hevc() {
        let policy = BackendPolicy::default();
        let plan = plan_hls_preview_graph("pipe_1", Some("hevc"), &policy);
        assert!(plan.is_some());
        let plan = plan.unwrap();
        assert_eq!(plan.role, GraphRole::HlsPreview);
        assert_eq!(
            plan.terminal_stage,
            StageKey::new(
                "pipe_1",
                StageKind::hls_segmenter(StageKind::preview("720p", StageKind::source()))
            )
        );
        assert_eq!(plan.stages.len(), 3);
        assert!(plan.stages.iter().any(|s| s.kind == StageKind::Source));
        assert!(
            plan.stages
                .iter()
                .any(|s| matches!(s.kind, StageKind::Preview { .. }))
        );
        assert!(plan.stages.iter().any(|s| {
            matches!(
                s.kind,
                StageKind::HlsSegmenter {
                    ref upstream
                } if matches!(upstream.as_ref(), StageKind::Preview { .. })
            )
        }));
        assert!(plan.edges.iter().any(|edge| {
            edge.from == StageKey::new("pipe_1", StageKind::preview("720p", StageKind::source()))
                && edge.to
                    == StageKey::new(
                        "pipe_1",
                        StageKind::hls_segmenter(StageKind::preview("720p", StageKind::source())),
                    )
        }));
    }

    #[test]
    fn plan_hls_preview_graph_models_h264_source_to_segmenter() {
        let policy = BackendPolicy::default();
        let plan = plan_hls_preview_graph("pipe_1", Some("h264"), &policy).unwrap();

        assert_eq!(plan.role, GraphRole::HlsPreview);
        assert_eq!(
            plan.terminal_stage,
            StageKey::new("pipe_1", StageKind::hls_segmenter(StageKind::source()))
        );
        assert_eq!(plan.stages.len(), 2);
        assert!(plan.stages.iter().any(|s| s.kind == StageKind::Source));
        assert!(plan.stages.iter().any(|s| {
            s.kind == StageKind::hls_segmenter(StageKind::source())
                && s.backend == StageBackend::HlsSegmenter
        }));
        assert!(plan.edges.iter().any(|edge| {
            edge.from == StageKey::new("pipe_1", StageKind::source())
                && edge.to == StageKey::new("pipe_1", StageKind::hls_segmenter(StageKind::source()))
        }));
    }

    #[test]
    fn plan_hls_preview_graph_returns_none_for_missing_codec() {
        let policy = BackendPolicy::default();
        assert!(plan_hls_preview_graph("pipe_1", None, &policy).is_none());
    }

    #[test]
    fn plan_pipeline_graph_sets_terminal_stage_from_output_path() {
        let policy = BackendPolicy::default();
        let output = Output {
            id: "out_1".to_string(),
            pipeline_id: "pipe_1".to_string(),
            name: "Output".to_string(),
            url: "rtmp://example/live".to_string(),
            monitoring_url: None,
            desired_state: DesiredOutputState::Running,
            config: crate::domain::output_spec::OutputConfig::parse("720p+atrack:0"),
        };

        let plan = plan_pipeline_graph("pipe_1", Some("hevc"), &[output], false, &policy);

        assert_eq!(
            plan.terminal_stage,
            StageKey::new(
                "pipe_1",
                StageKind::audio_route(
                    "atrack:0",
                    StageKind::codec_edge(
                        "hevc_to_h264",
                        StageKind::video_preset_with_codec("720p", "hevc")
                    ),
                )
            )
        );
    }

    #[test]
    fn plan_pipeline_graph_sets_preview_terminal_when_preview_only() {
        let policy = BackendPolicy::default();
        let plan = plan_pipeline_graph("pipe_1", Some("hevc"), &[], true, &policy);

        assert_eq!(
            plan.terminal_stage,
            StageKey::new("pipe_1", StageKind::preview("720p", StageKind::source()))
        );
    }

    #[test]
    fn plan_recording_graph_uses_recording_role_and_backend() {
        let policy = BackendPolicy::default();
        let plan = plan_recording_graph("pipe_1", &policy);

        assert_eq!(plan.role, GraphRole::Recording);
        assert_eq!(
            plan.terminal_stage,
            StageKey::new("pipe_1", StageKind::recording())
        );
        assert!(plan.stages.iter().any(|stage| {
            stage.kind == StageKind::recording() && stage.backend == StageBackend::Recording
        }));
        assert!(plan.edges.iter().any(|edge| {
            edge.from == StageKey::new("pipe_1", StageKind::source())
                && edge.to == StageKey::new("pipe_1", StageKind::recording())
        }));
    }

    #[test]
    fn hls_output_graph_terminates_at_protocol_segmenter() {
        let policy = BackendPolicy::default();
        let output = Output {
            id: "out_1".to_string(),
            pipeline_id: "pipe_1".to_string(),
            name: "HLS Output".to_string(),
            url: "https://example.com/live/out.m3u8".to_string(),
            monitoring_url: None,
            desired_state: DesiredOutputState::Running,
            config: crate::domain::output_spec::OutputConfig::parse("720p"),
        };
        let plan = plan_hls_output_graph("pipe_1", Some("hevc"), &output, &policy);

        assert_eq!(
            plan.role,
            GraphRole::HlsOutput {
                output_id: OutputId::new("out_1")
            }
        );
        assert_eq!(
            plan.terminal_stage,
            StageKey::new(
                "pipe_1",
                StageKind::hls_segmenter(StageKind::video_preset("720p"))
            )
        );
        assert!(plan.stages.iter().any(|stage| {
            stage.kind == StageKind::hls_segmenter(StageKind::video_preset("720p"))
                && stage.backend == StageBackend::HlsSegmenter
        }));
        assert!(
            plan.edges.iter().any(|edge| {
                edge.from == StageKey::new("pipe_1", StageKind::video_preset("720p"))
                    && edge.to
                        == StageKey::new(
                            "pipe_1",
                            StageKind::hls_segmenter(StageKind::video_preset("720p")),
                        )
            }),
            "HLS output graph should show media stage feeding protocol segmenter"
        );
    }
}
