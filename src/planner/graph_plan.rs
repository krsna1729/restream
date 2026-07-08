//! Unified graph planner for stage pipeline graphs.
//!
//! Generates a single StageGraphPlan from output specifications and active
//! preview requests, providing a single source of truth for transcoders,
//! HLS previews, and recordings.

use crate::domain::stage::{StageKey, StageKind};
use crate::planner::backend_policy::{BackendPolicy, StageBackend};
use crate::runtime::graph::{GraphRole, StageGraphPlan};
use crate::types::Output;

pub fn plan_pipeline_graph(
    pipeline_id: &str,
    ingest_codec: Option<&str>,
    outputs: &[Output],
    hls_preview_active: bool,
) -> StageGraphPlan {
    let terminal_stage = StageKey::new(pipeline_id, StageKind::Source);
    let mut plan = StageGraphPlan::new(
        pipeline_id,
        GraphRole::Output {
            output_id: String::new(),
        },
        terminal_stage,
    );
    let policy = BackendPolicy::from_env();

    // 1. Source stage is always present
    plan.add_stage(
        StageKey::new(pipeline_id, StageKind::Source),
        StageBackend::AudioRouter, // Source doesn't run a transcoder
    );

    // 2. Add outputs stages
    for output in outputs {
        let encoding = output.encoding_string();
        let output_path = crate::application::output_path::OutputPath::resolve(
            pipeline_id,
            &encoding,
            &output.url,
        );

        for stage_key in output_path.needed_stage_keys(ingest_codec) {
            let backend = policy.select_backend(&stage_key.kind);
            plan.add_stage(stage_key, backend);
        }
    }

    // 3. Add HLS preview stage if active and ingest codec is HEVC/H.265
    if hls_preview_active {
        if let Some(codec) = ingest_codec {
            if codec.eq_ignore_ascii_case("hevc") || codec.eq_ignore_ascii_case("h265") {
                let preview_key =
                    StageKey::new(pipeline_id, StageKind::preview("720p", StageKind::source()));
                let backend = policy.select_backend(&preview_key.kind);
                plan.add_stage(preview_key, backend);
            }
        }
    }

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
) -> Option<StageGraphPlan> {
    let codec = ingest_codec?;
    if !is_hevc_preview_codec(codec) {
        return None;
    }

    let preview_key = StageKey::new(pipeline_id, StageKind::preview("720p", StageKind::source()));
    let policy = BackendPolicy::from_env();
    let terminal = preview_key.clone();

    let mut plan = StageGraphPlan::new(pipeline_id, GraphRole::HlsPreview, terminal);

    plan.add_stage(
        StageKey::new(pipeline_id, StageKind::Source),
        StageBackend::AudioRouter,
    );
    let backend = policy.select_backend(&preview_key.kind);
    plan.add_stage(preview_key, backend);

    Some(plan)
}

fn is_hevc_preview_codec(codec: &str) -> bool {
    codec.eq_ignore_ascii_case("hevc") || codec.eq_ignore_ascii_case("h265")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_hls_preview_graph_returns_plan_for_hevc() {
        let plan = plan_hls_preview_graph("pipe_1", Some("hevc"));
        assert!(plan.is_some());
        let plan = plan.unwrap();
        assert_eq!(plan.role, GraphRole::HlsPreview);
        assert_eq!(
            plan.terminal_stage,
            StageKey::new("pipe_1", StageKind::preview("720p", StageKind::source()))
        );
        assert_eq!(plan.stages.len(), 2);
        assert!(plan.stages.iter().any(|s| s.kind == StageKind::Source));
        assert!(
            plan.stages
                .iter()
                .any(|s| matches!(s.kind, StageKind::Preview { .. }))
        );
    }

    #[test]
    fn plan_hls_preview_graph_returns_none_for_h264() {
        assert!(plan_hls_preview_graph("pipe_1", Some("h264")).is_none());
    }

    #[test]
    fn plan_hls_preview_graph_returns_none_for_missing_codec() {
        assert!(plan_hls_preview_graph("pipe_1", None).is_none());
    }

    #[test]
    fn graph_role_hls_output_variant_exists() {
        let role = GraphRole::HlsOutput {
            output_id: "out_1".to_string(),
        };
        assert_eq!(
            role,
            GraphRole::HlsOutput {
                output_id: "out_1".to_string()
            }
        );
    }
}
