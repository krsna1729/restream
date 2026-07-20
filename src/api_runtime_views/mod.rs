//! API/runtime adapter views that project `MediaEngine` state into HTTP-facing
//! JSON payloads.
//!
//! This layer still needs direct access to runtime registries, but it keeps
//! API-facing shaping out of the media modules themselves. The adapter families
//! stay split by responsibility here rather than inside `media`.

mod common;
mod graph;
mod graph_projection;
mod resource_map;
mod stage_projection;
mod status;
mod status_projection;
mod telemetry;
mod telemetry_projection;

pub(crate) use common::probe_snapshot;
pub(crate) use graph::processing_graph;
#[cfg(test)]
pub(crate) use graph_projection::processing_graph_stage_node;
pub(crate) use resource_map::{ResourceMapOptions, ResourceMapView, resource_map};
#[cfg(test)]
pub(crate) use stage_projection::stage_runtime_snapshot_json;
pub(crate) use status::{health_snapshot, health_summary_snapshot, output_status};
pub(crate) use telemetry::{engine_telemetry, pipeline_telemetry, stage_telemetry_by_display};
