//! Backend adapter trait for FFmpeg-backed stages.
//!
//! External and internal FFmpeg execution differ only in mechanics; they share
//! the same plan, input pump, output normalizer, and lifecycle.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::domain::stage::StageKey;
use crate::media::engine::MediaEngine;
use crate::media::stage_lifecycle::StageLifecycle;
use crate::media::stage_metrics::StageMetrics;

use super::stage_input::StageInputPump;
use super::stage_output::StageOutputNormalizer;
use super::stage_plan::FfmpegStagePlan;

/// Execution context passed to every FFmpeg stage backend.
#[derive(Clone)]
pub struct StageRunContext {
    pub stage_key: StageKey,
    pub pipeline_id: String,
    pub cancel: CancellationToken,
    pub lifecycle: Arc<StageLifecycle>,
    pub metrics: Arc<StageMetrics>,
    pub engine: Arc<MediaEngine>,
}

/// Error type returned by backend adapters.
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

/// Trait implemented by external-process and in-process FFmpeg backends.
///
/// Backends receive a compiled plan, a shared input pump, and a shared output
/// normalizer. They must not write directly to the output ring.
pub trait FfmpegStageBackend: Send {
    /// Run the stage until cancellation or error.
    fn run(
        self,
        plan: FfmpegStagePlan,
        input: StageInputPump,
        output: StageOutputNormalizer,
        ctx: StageRunContext,
    ) -> impl std::future::Future<Output = Result<(), BackendError>> + Send;
}

/// External-process FFmpeg backend adapter.
#[derive(Default)]
pub struct ExternalFfmpegBackend;

impl FfmpegStageBackend for ExternalFfmpegBackend {
    async fn run(
        self,
        plan: FfmpegStagePlan,
        input: StageInputPump,
        output: StageOutputNormalizer,
        ctx: StageRunContext,
    ) -> Result<(), BackendError> {
        crate::media::external_transcoder::run_external_ffmpeg_backend(plan, input, output, ctx)
            .await
    }
}

/// In-process FFmpeg/backend adapter.
#[derive(Default)]
pub struct InternalFfmpegBackend;

impl FfmpegStageBackend for InternalFfmpegBackend {
    async fn run(
        self,
        plan: FfmpegStagePlan,
        input: StageInputPump,
        output: StageOutputNormalizer,
        ctx: StageRunContext,
    ) -> Result<(), BackendError> {
        crate::media::transcoder::run_internal_ffmpeg_backend(plan, input, output, ctx).await
    }
}
