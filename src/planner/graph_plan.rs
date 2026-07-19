//! Unified graph planner for stage pipeline graphs.
//!
//! Generates a single StageGraphPlan from output specifications and active
//! preview requests, providing a single source of truth for transcoders,
//! HLS previews, and recordings.

use crate::domain::ids::{OutputId, PipelineId};
use crate::domain::output_spec::OutputConfig;
use crate::domain::stage::{StageKey, StageKind};
use crate::domain::state::StageBackendKind;
use crate::planner::backend_policy::BackendPolicy;
use crate::planner::output_path::OutputPath;
use crate::runtime::graph::{GraphRole, StageGraphPlan};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedOutput {
    pub id: OutputId,
    pub config: OutputConfig,
    pub url: String,
}

impl PlannedOutput {
    pub fn new(id: impl Into<OutputId>, config: OutputConfig, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            config,
            url: url.into(),
        }
    }
}

pub fn plan_pipeline_graph(
    pipeline_id: &str,
    ingest_codec: Option<&str>,
    outputs: &[PlannedOutput],
    hls_preview_active: bool,
    policy: &BackendPolicy,
) -> StageGraphPlan {
    let pipeline_id_typed = PipelineId::new(pipeline_id);
    let terminal_stage = StageKey::new(pipeline_id_typed.clone(), StageKind::Source);
    let output_id_typed = outputs
        .first()
        .map(|o| o.id.clone())
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
        StageBackendKind::AudioRouter, // Source doesn't run a transcoder
    );

    // 2. Add outputs stages
    for (index, output) in outputs.iter().enumerate() {
        let output_path = OutputPath::resolve(pipeline_id, output.config.clone(), &output.url);
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
    output: &PlannedOutput,
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
        output_id: output.id.clone(),
    };
    plan
}

/// Plan the HLS preview graph for a pipeline.
///
/// This is a pure planning function — it does not create ring buffers or
/// spawn stages. The runtime resolver in `media::hls::preview_graph` uses
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
        StageBackendKind::AudioRouter,
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
        StageBackendKind::AudioRouter,
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
    use crate::domain::audio_routing::AudioRouting;
    use crate::domain::output_spec::RtmpOutputMode;
    use proptest::prelude::*;
    use std::collections::HashSet;

    fn output_for_case(
        index: usize,
        video_case: u8,
        audio_case: u8,
        protocol_case: u8,
    ) -> PlannedOutput {
        let config = match video_case % 3 {
            0 => OutputConfig::source(),
            1 => OutputConfig::preset("720p"),
            _ => OutputConfig::preset("1080p"),
        };
        let config = match audio_case % 3 {
            0 => config,
            1 => config.with_audio(AudioRouting::SelectTracks { tracks: vec![0] }),
            _ => config.with_audio(AudioRouting::SelectTracks { tracks: vec![0, 1] }),
        };
        let url = match protocol_case % 3 {
            0 => format!("rtmp://example/live/out-{index}"),
            1 => format!("srt://example:9000?streamid=publish:out-{index}"),
            _ => format!("https://example/live/out-{index}.m3u8"),
        };

        PlannedOutput {
            id: OutputId::new(format!("out_{index}")),
            config,
            url,
        }
    }

    fn contains_unqualified_video_preset(kind: &StageKind) -> bool {
        match kind {
            StageKind::VideoPreset {
                output_codec: None, ..
            } => true,
            StageKind::AudioRoute { upstream, .. }
            | StageKind::CodecEdge { upstream, .. }
            | StageKind::Preview { upstream, .. }
            | StageKind::HlsSegmenter { upstream } => contains_unqualified_video_preset(upstream),
            _ => false,
        }
    }

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
                && s.backend == StageBackendKind::HlsSegmenter
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
        let output = PlannedOutput::new(
            "out_1",
            OutputConfig::preset("720p").with_audio(AudioRouting::SelectTracks { tracks: vec![0] }),
            "rtmp://example/live",
        );

        let plan = plan_pipeline_graph("pipe_1", Some("hevc"), &[output], false, &policy);

        assert_eq!(
            plan.terminal_stage,
            StageKey::new(
                "pipe_1",
                StageKind::audio_route(
                    "atrack:0",
                    StageKind::video_preset_with_codec("720p", "h264"),
                )
            )
        );
    }

    #[test]
    fn plan_pipeline_graph_enhanced_rtmp_hevc_skips_codec_edge() {
        let policy = BackendPolicy::default();
        let output = PlannedOutput::new(
            "out_1",
            OutputConfig::preset("720p")
                .with_audio(AudioRouting::SelectTracks { tracks: vec![0] })
                .with_rtmp_mode(RtmpOutputMode::Enhanced),
            "rtmp://example/live",
        );

        let plan = plan_pipeline_graph("pipe_1", Some("hevc"), &[output], false, &policy);

        assert!(
            plan.stages
                .iter()
                .all(|stage| !matches!(stage.kind, StageKind::CodecEdge { .. }))
        );
        assert_eq!(
            plan.terminal_stage,
            StageKey::new(
                "pipe_1",
                StageKind::audio_route(
                    "atrack:0",
                    StageKind::video_preset_with_codec("720p", "hevc")
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
            stage.kind == StageKind::recording() && stage.backend == StageBackendKind::Recording
        }));
        assert!(plan.edges.iter().any(|edge| {
            edge.from == StageKey::new("pipe_1", StageKind::source())
                && edge.to == StageKey::new("pipe_1", StageKind::recording())
        }));
    }

    #[test]
    fn hls_output_graph_terminates_at_protocol_segmenter() {
        let policy = BackendPolicy::default();
        let output = PlannedOutput::new(
            "out_1",
            OutputConfig::preset("720p"),
            "https://example.com/live/out.m3u8",
        );
        let plan = plan_hls_output_graph("pipe_1", Some("hevc"), &output, &policy);

        assert_eq!(
            plan.role,
            GraphRole::HlsOutput {
                output_id: OutputId::new("out_1")
            }
        );
        let h264_720p = StageKind::video_preset_with_codec("720p", "h264");
        assert_eq!(
            plan.terminal_stage,
            StageKey::new("pipe_1", StageKind::hls_segmenter(h264_720p.clone()))
        );
        assert!(plan.stages.iter().any(|stage| {
            stage.kind == StageKind::hls_segmenter(h264_720p.clone())
                && stage.backend == StageBackendKind::HlsSegmenter
        }));
        assert!(
            plan.edges.iter().any(|edge| {
                edge.from == StageKey::new("pipe_1", h264_720p.clone())
                    && edge.to
                        == StageKey::new("pipe_1", StageKind::hls_segmenter(h264_720p.clone()))
            }),
            "HLS output graph should show media stage feeding protocol segmenter"
        );
    }

    proptest! {
        #[test]
        fn plan_pipeline_graph_preserves_stage_identity_invariants(
            ingest_is_hevc in any::<bool>(),
            cases in prop::collection::vec((0_u8..3, 0_u8..3, 0_u8..3), 1..8),
        ) {
            let outputs = cases
                .iter()
                .enumerate()
                .map(|(index, (video_case, audio_case, protocol_case))| {
                    output_for_case(index, *video_case, *audio_case, *protocol_case)
                })
                .collect::<Vec<_>>();
            let policy = BackendPolicy::default();
            let ingest_codec = if ingest_is_hevc { Some("hevc") } else { Some("h264") };
            let plan = plan_pipeline_graph(
                "pipe_prop",
                ingest_codec,
                &outputs,
                false,
                &policy,
            );
            let stage_keys = plan
                .stages
                .iter()
                .map(|stage| stage.key.clone())
                .collect::<HashSet<_>>();

            prop_assert!(
                stage_keys.contains(&plan.terminal_stage),
                "terminal stage {} must be present in the planned stages {:?}",
                plan.terminal_stage,
                stage_keys
            );
            prop_assert_eq!(stage_keys.len(), plan.stages.len(), "stage keys must be unique");

            for stage in &plan.stages {
                prop_assert_eq!(stage.key.pipeline.as_str(), "pipe_prop");
                prop_assert_eq!(&stage.kind, &stage.key.kind);
            }

            for edge in &plan.edges {
                prop_assert!(
                    stage_keys.contains(&edge.from),
                    "edge input {} must be planned for edge to {}",
                    edge.from,
                    edge.to
                );
                prop_assert!(
                    stage_keys.contains(&edge.to),
                    "edge output {} must be planned",
                    edge.to
                );
            }

            if ingest_is_hevc {
                for stage in &plan.stages {
                    prop_assert!(
                        !contains_unqualified_video_preset(&stage.kind),
                        "HEVC ingest plan leaked unqualified video stage {:?}",
                        stage.kind
                    );
                }
                prop_assert!(
                    !contains_unqualified_video_preset(&plan.terminal_stage.kind),
                    "HEVC ingest terminal stage leaked unqualified video stage {:?}",
                    plan.terminal_stage.kind
                );
            }
        }

        #[test]
        fn plan_hls_output_graph_preserves_protocol_segmenter_invariants(
            ingest_is_hevc in any::<bool>(),
            video_case in 0_u8..3,
            audio_case in 0_u8..3,
        ) {
            let output = output_for_case(0, video_case, audio_case, 2);
            let policy = BackendPolicy::default();
            let ingest_codec = if ingest_is_hevc { Some("hevc") } else { Some("h264") };
            let plan = plan_hls_output_graph("pipe_prop", ingest_codec, &output, &policy);
            let stage_keys = plan
                .stages
                .iter()
                .map(|stage| stage.key.clone())
                .collect::<HashSet<_>>();

            prop_assert_eq!(
                plan.role,
                GraphRole::HlsOutput {
                    output_id: OutputId::new("out_0")
                }
            );
            let terminal_is_hls_segmenter =
                matches!(plan.terminal_stage.kind, StageKind::HlsSegmenter { .. });
            prop_assert!(terminal_is_hls_segmenter);
            prop_assert!(stage_keys.contains(&plan.terminal_stage));
            prop_assert_eq!(stage_keys.len(), plan.stages.len(), "stage keys must be unique");
            prop_assert!(
                plan.stages.iter().any(|stage| {
                    stage.key == plan.terminal_stage
                        && stage.backend == StageBackendKind::HlsSegmenter
                }),
                "terminal HLS protocol stage must use HLS segmenter backend"
            );
            prop_assert!(
                plan.edges.iter().any(|edge| edge.to == plan.terminal_stage),
                "HLS terminal stage must have an input edge"
            );

            if ingest_is_hevc {
                for stage in &plan.stages {
                    prop_assert!(
                        !contains_unqualified_video_preset(&stage.kind),
                        "HEVC HLS output plan leaked unqualified video stage {:?}",
                        stage.kind
                    );
                }
            }
        }

        #[test]
        fn plan_hls_preview_graph_preserves_codec_specific_terminal_shape(
            codec_case in 0_u8..3,
        ) {
            let codec = match codec_case {
                0 => "h264",
                1 => "hevc",
                _ => "h265",
            };
            let policy = BackendPolicy::default();
            let plan = plan_hls_preview_graph("pipe_prop", Some(codec), &policy)
                .expect("known codec should produce a preview graph");
            let stage_keys = plan
                .stages
                .iter()
                .map(|stage| stage.key.clone())
                .collect::<HashSet<_>>();

            prop_assert_eq!(plan.role, GraphRole::HlsPreview);
            let terminal_is_hls_segmenter =
                matches!(plan.terminal_stage.kind, StageKind::HlsSegmenter { .. });
            prop_assert!(terminal_is_hls_segmenter);
            prop_assert!(stage_keys.contains(&plan.terminal_stage));
            prop_assert_eq!(stage_keys.len(), plan.stages.len(), "stage keys must be unique");
            let terminal_has_hls_backend = plan.stages.iter().any(|stage| {
                stage.key == plan.terminal_stage && stage.backend == StageBackendKind::HlsSegmenter
            });
            prop_assert!(terminal_has_hls_backend);

            let is_hevc = is_hevc_preview_codec(codec);
            prop_assert_eq!(
                plan.stages.iter().any(|stage| matches!(stage.kind, StageKind::Preview { .. })),
                is_hevc,
                "only HEVC preview should insert a preview transcode stage"
            );
            if is_hevc {
                prop_assert!(
                    plan.edges.iter().any(|edge| {
                        matches!(edge.from.kind, StageKind::Preview { .. })
                            && edge.to == plan.terminal_stage
                    }),
                    "HEVC preview segmenter should consume the preview stage"
                );
            } else {
                prop_assert!(
                    plan.edges.iter().any(|edge| {
                        edge.from == StageKey::new("pipe_prop", StageKind::source())
                            && edge.to == plan.terminal_stage
                    }),
                    "H.264 preview segmenter should consume source directly"
                );
            }
        }
    }
}
