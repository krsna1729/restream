//! API/runtime adapter views that project `MediaEngine` state into HTTP-facing
//! JSON payloads.
//!
//! This layer still needs direct access to runtime registries, but it keeps
//! API-facing shaping out of the media modules themselves. The adapter families
//! stay split by responsibility here rather than inside `media`.

mod common;
mod graph;
mod graph_projection;
mod measure;
mod node_judgement;
mod resource_map;
mod stage_projection;
mod status;
mod status_projection;
mod telemetry;
mod telemetry_projection;

#[cfg(test)]
#[path = "engine_tests.rs"]
mod engine_tests;
#[cfg(test)]
mod stage_phase_tests;

pub(crate) use common::probe_snapshot;
pub(crate) use graph::processing_graph;
pub(crate) use measure::measure_snapshot;
pub(crate) use resource_map::{ResourceMapOptions, ResourceMapView, resource_map};
pub(crate) use status::{
    health_snapshot, health_snapshot_view, health_summary_snapshot, output_status,
};
pub(crate) use telemetry::{engine_telemetry, pipeline_telemetry, stage_telemetry_by_display};
