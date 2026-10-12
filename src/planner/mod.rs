//! Planning policy for turning desired media stages into runtime decisions.

mod encoding_stage_plan;
mod graph_plan;
mod output_path;
mod stage_backend;

pub use graph_plan::{
    PlannedOutput, plan_hls_output_graph, plan_hls_preview_graph, plan_pipeline_graph,
    plan_recording_graph,
};
pub use stage_backend::select_backend;

#[cfg(test)]
pub(crate) use encoding_stage_plan::EncodingStagePlan;
