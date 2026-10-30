use crate::domain::ids::{OutputId, PipelineId};
use crate::domain::stage::{StageKey, StageKind};
use crate::domain::state::StageBackendKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphRole {
    Output { output_id: OutputId },
    HlsPreview,
    HlsOutput { output_id: OutputId },
    Recording,
    Diagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePlan {
    pub key: StageKey,
    pub kind: StageKind,
    pub input: Option<StageKey>,
    pub backend: StageBackendKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageEdge {
    pub from: StageKey,
    pub to: StageKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageGraphPlan {
    pub pipeline_id: PipelineId,
    pub role: GraphRole,
    pub terminal_stage: StageKey,
    pub stages: Vec<StagePlan>,
    pub edges: Vec<StageEdge>,
}

impl StageGraphPlan {
    pub fn new(pipeline_id: PipelineId, role: GraphRole, terminal_stage: StageKey) -> Self {
        Self {
            pipeline_id,
            role,
            terminal_stage,
            stages: Vec::new(),
            edges: Vec::new(),
        }
    }

    pub fn add_stage(&mut self, key: StageKey, backend: StageBackendKind) {
        if self.stages.iter().any(|s| s.key == key) {
            return;
        }
        let kind = key.kind.clone();
        let input = match &kind {
            StageKind::Source => None,
            StageKind::VideoPreset { .. } => {
                Some(StageKey::new(self.pipeline_id.clone(), StageKind::Source))
            }
            StageKind::AudioRoute { upstream, .. } => {
                Some(StageKey::new(self.pipeline_id.clone(), *upstream.clone()))
            }
            StageKind::CodecEdge { upstream, .. } => {
                Some(StageKey::new(self.pipeline_id.clone(), *upstream.clone()))
            }
            StageKind::Preview { upstream, .. } => {
                Some(StageKey::new(self.pipeline_id.clone(), *upstream.clone()))
            }
            StageKind::HlsSegmenter { upstream } => {
                Some(StageKey::new(self.pipeline_id.clone(), *upstream.clone()))
            }
            StageKind::Hls => Some(StageKey::new(self.pipeline_id.clone(), StageKind::Source)),
            StageKind::Recording => {
                Some(StageKey::new(self.pipeline_id.clone(), StageKind::Source))
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
