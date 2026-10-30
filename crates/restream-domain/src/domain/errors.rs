//! Domain-level error types for stage and runtime failures.
//!
//! `StageError` is the structured domain error used in health snapshots and
//! output status explanations. The thin `BackendError` in
//! `media::ffmpeg::backend` is an execution-layer detail that can be converted
//! to `StageError` when surfacing to the API/health layer.

use std::fmt;

/// A structured error from a media stage execution.
///
/// Used in health snapshots and output status explanations to give operators
/// enough context to understand why a stage failed without reading raw logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageError {
    /// Short machine-readable code, e.g. `"metadata_timeout"`, `"connect_refused"`.
    pub code: String,
    /// Human-readable description of the failure.
    pub message: String,
    /// Whether this error is expected to resolve if the stage is retried.
    pub retryable: bool,
    /// Last few lines of the backend's stderr output, if available.
    pub stderr_tail: Option<String>,
}

impl StageError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
            stderr_tail: None,
        }
    }

    pub fn with_stderr(mut self, tail: impl Into<String>) -> Self {
        self.stderr_tail = Some(tail.into());
        self
    }

    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message, true)
    }

    pub fn permanent(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message, false)
    }
}

impl fmt::Display for StageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)?;
        if let Some(tail) = &self.stderr_tail {
            write!(f, " (stderr: {})", tail.trim())?;
        }
        Ok(())
    }
}

impl std::error::Error for StageError {}

impl From<String> for StageError {
    fn from(value: String) -> Self {
        Self::permanent("backend_error", value)
    }
}

/// A runtime-level error that describes a failure in the media graph.
///
/// Used where we need to attribute an error to a named entity (pipeline,
/// output, stage) without losing the structured code/retryability information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeError {
    /// Short machine-readable error code.
    pub code: String,
    /// Human-readable description.
    pub message: String,
    /// The entity (pipeline ID, output ID, stage key string, etc.) that failed.
    pub entity: String,
    /// Whether retrying the operation might succeed.
    pub retryable: bool,
}

impl RuntimeError {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        entity: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            entity: entity.into(),
            retryable,
        }
    }

    pub fn retryable(
        code: impl Into<String>,
        message: impl Into<String>,
        entity: impl Into<String>,
    ) -> Self {
        Self::new(code, message, entity, true)
    }

    pub fn permanent(
        code: impl Into<String>,
        message: impl Into<String>,
        entity: impl Into<String>,
    ) -> Self {
        Self::new(code, message, entity, false)
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} (entity: {})",
            self.code, self.message, self.entity
        )
    }
}

impl std::error::Error for RuntimeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_stage_error_display() {
        let err =
            StageError::retryable("metadata_timeout", "Timed out waiting for ingest metadata");
        assert_eq!(
            err.to_string(),
            "[metadata_timeout] Timed out waiting for ingest metadata"
        );
        assert!(err.retryable);
        assert!(err.stderr_tail.is_none());
    }

    #[test]
    fn domain_stage_error_with_stderr() {
        let err = StageError::permanent("connect_refused", "Remote refused connection")
            .with_stderr("Connection refused\nNo route to host");
        assert!(err.to_string().contains("stderr:"));
        assert!(!err.retryable);
    }

    #[test]
    fn stage_error_from_string() {
        let err = StageError::from("ffmpeg exited with code 1".to_string());
        assert_eq!(err.code, "backend_error");
        assert!(!err.retryable);
        assert!(err.message.contains("ffmpeg exited"));
    }

    #[test]
    fn runtime_error_display() {
        let err = RuntimeError::permanent(
            "stage_failed",
            "Video transcoder failed",
            "pipeline_abc:video:720p",
        );
        assert_eq!(
            err.to_string(),
            "[stage_failed] Video transcoder failed (entity: pipeline_abc:video:720p)"
        );
        assert!(!err.retryable);
    }

    #[test]
    fn runtime_error_retryable() {
        let err = RuntimeError::retryable("connect_timeout", "SRT connect timed out", "output_xyz");
        assert!(err.retryable);
        assert_eq!(err.entity, "output_xyz");
    }
}
