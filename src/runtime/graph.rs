use crate::domain::stage::{StageKey, StageKind};
use crate::planner::backend_policy::StageBackend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphRole {
    Output { output_id: String },
    HlsPreview,
    HlsOutput { output_id: String },
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
    pub role: GraphRole,
    pub terminal_stage: StageKey,
    pub stages: Vec<StagePlan>,
    pub edges: Vec<StageEdge>,
}

impl StageGraphPlan {
    pub fn new(pipeline_id: &str, role: GraphRole, terminal_stage: StageKey) -> Self {
        Self {
            pipeline_id: pipeline_id.to_string(),
            role,
            terminal_stage,
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
