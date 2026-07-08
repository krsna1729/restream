//! Domain-level error types for stage and runtime failures.
//!
//! These are distinct from the thin `StageError` newtype in
//! `media::ffmpeg::backend`, which is an execution-layer detail. The domain
//! error types here carry structured fields that can be surfaced in API
//! responses and used by the health/alert layer.

use std::fmt;

/// A structured error from a media stage execution.
///
/// Used in health snapshots and output status explanations to give operators
/// enough context to understand why a stage failed without reading raw logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainStageError {
    /// Short machine-readable code, e.g. `"metadata_timeout"`, `"connect_refused"`.
    pub code: String,
    /// Human-readable description of the failure.
    pub message: String,
    /// Whether this error is expected to resolve if the stage is retried.
    pub retryable: bool,
    /// Last few lines of the backend's stderr output, if available.
    pub stderr_tail: Option<String>,
}

impl DomainStageError {
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

impl fmt::Display for DomainStageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)?;
        if let Some(tail) = &self.stderr_tail {
            write!(f, " (stderr: {})", tail.trim())?;
        }
        Ok(())
    }
}

impl std::error::Error for DomainStageError {}

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
        let err = DomainStageError::retryable(
            "metadata_timeout",
            "Timed out waiting for ingest metadata",
        );
        assert_eq!(
            err.to_string(),
            "[metadata_timeout] Timed out waiting for ingest metadata"
        );
        assert!(err.retryable);
        assert!(err.stderr_tail.is_none());
    }

    #[test]
    fn domain_stage_error_with_stderr() {
        let err = DomainStageError::permanent("connect_refused", "Remote refused connection")
            .with_stderr("Connection refused\nNo route to host");
        assert!(err.to_string().contains("stderr:"));
        assert!(!err.retryable);
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
