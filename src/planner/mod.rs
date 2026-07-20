//! Planning policy for turning desired media stages into runtime decisions.

mod backend_policy;
mod encoding_stage_plan;
mod graph_plan;
mod output_path;

pub use backend_policy::BackendPolicy;
pub use graph_plan::{
    PlannedOutput, plan_hls_output_graph, plan_hls_preview_graph, plan_pipeline_graph,
    plan_recording_graph,
};

#[cfg(test)]
pub(crate) use encoding_stage_plan::EncodingStagePlan;
