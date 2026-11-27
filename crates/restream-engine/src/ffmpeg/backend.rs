//! Execution context and error type for FFmpeg-backed stages, which run as
//! external `ffmpeg` processes (`external_transcoder`).

use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::events::EventLog;
use crate::stage_lifecycle::StageLifecycle;
use crate::stage_metrics::StageMetrics;
use crate::stage_registry::StageRegistry;
use restream_domain::domain::stage::StageKey;

/// Execution context passed to every FFmpeg stage.
#[derive(Clone)]
pub struct StageRunContext {
    pub stage_key: StageKey,
    pub pipeline_id: String,
    pub cancel: CancellationToken,
    pub lifecycle: Arc<StageLifecycle>,
    pub metrics: Arc<StageMetrics>,
    /// Where the stage registers its pipe metrics.
    pub stages: Arc<StageRegistry>,
    /// The engine-wide external FFmpeg slots (`EngineConfig::external_ffmpeg_permits`).
    pub ffmpeg_permits: Arc<Semaphore>,
    /// The engine's event log, for `StageStopped`.
    pub events: Arc<EventLog>,
}

/// Error type returned by FFmpeg stage execution.
///
/// This is a thin execution-layer error. It can be converted to the structured
/// `StageError` in `domain::errors` via its `From<String>` impl when surfacing
/// to the API/health layer.
#[derive(Debug, Clone)]
pub struct BackendError(pub String);

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for BackendError {}

impl From<String> for BackendError {
    fn from(value: String) -> Self {
        Self(value)
    }
}
