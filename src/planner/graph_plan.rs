//! Unified graph planner for stage pipeline graphs.
//!
//! Generates a single StageGraphPlan from output specifications and active
//! preview requests, providing a single source of truth for transcoders,
//! HLS previews, and recordings.

use crate::domain::stage::{StageKey, StageKind};
use crate::planner::backend_policy::{BackendPolicy, StageBackend};
use crate::types::Output;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphRole {
    Output { output_id: String },
    HlsPreview,
    Recording,
    Diagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePlan {
    pub key: StageKey,
    pub kind: StageKind,
    pub input: Option<StageKey>,
    pub backend: StageBackend,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageEdge {
    pub from: StageKey,
    pub to: StageKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageGraphPlan {
    pub pipeline_id: String,
    pub stages: Vec<StagePlan>,
    pub edges: Vec<StageEdge>,
}

impl StageGraphPlan {
    pub fn new(pipeline_id: &str) -> Self {
        Self {
            pipeline_id: pipeline_id.to_string(),
            stages: Vec::new(),
            edges: Vec::new(),
        }
    }

    pub fn add_stage(&mut self, key: StageKey, backend: StageBackend) {
        if self.stages.iter().any(|s| s.key == key) {
            return;
        }
        let kind = key.kind.clone();
        let input = match &kind {
            StageKind::Source => None,
            StageKind::VideoPreset { .. } => {
                Some(StageKey::new(self.pipeline_id.as_str(), StageKind::Source))
            }
            StageKind::AudioRoute { upstream, .. } => {
                Some(StageKey::new(self.pipeline_id.as_str(), *upstream.clone()))
            }
            StageKind::CodecEdge { upstream, .. } => {
                Some(StageKey::new(self.pipeline_id.as_str(), *upstream.clone()))
            }
            StageKind::Preview { upstream, .. } => {
                Some(StageKey::new(self.pipeline_id.as_str(), *upstream.clone()))
            }
            StageKind::Hls => Some(StageKey::new(self.pipeline_id.as_str(), StageKind::Source)),
            StageKind::Recording => {
                Some(StageKey::new(self.pipeline_id.as_str(), StageKind::Source))
            }
        };

        if let Some(ref input_key) = input {
            self.edges.push(StageEdge {
                from: input_key.clone(),
                to: key.clone(),
            });
        }

        self.stages.push(StagePlan {
            key,
            kind,
            input,
            backend,
        });
    }
}

pub fn plan_pipeline_graph(
    pipeline_id: &str,
    ingest_codec: Option<&str>,
    outputs: &[Output],
    hls_preview_active: bool,
) -> StageGraphPlan {
    let mut plan = StageGraphPlan::new(pipeline_id);
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
