//! Capacity contract types for stage backend admission.
//!
//! These types describe the capacity class of a stage backend and the current
//! utilization state. They are consumed by runtime health snapshots and alert
//! derivation.
//!
//! Filled out in Phase 7 (first-class stage lifecycle) and Phase 12
//! (health/alerts/diagnostics v2). Currently used as fields on
//! [`StageRuntimeSnapshot`](super::stage::StageRuntimeSnapshot).

/// The capacity class of a stage backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapacityClass {
    ExternalFfmpeg,
    InternalFfmpeg,
}
