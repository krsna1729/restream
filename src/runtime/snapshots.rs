//! Compatibility re-export for media-owned snapshot DTOs.
//!
//! Media produces these live quality, metadata, and dependency snapshots. Keep
//! this runtime path stable for API, diagnostics, and harness consumers while
//! the owning definitions live under `media::snapshots`.

pub use crate::media::snapshots::*;
