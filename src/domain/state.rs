//! Typed runtime and desired-state enums for all domain objects.
//!
//! These replace ad-hoc string fields at internal boundaries. DB rows and API
//! responses may still use string representations; these types are the
//! canonical internal form. Each enum provides `as_str()`, `Display`,
//! `From<&str>` with a fallback variant, and `Default`.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::domain::stage::StageKey;

/// Backend kind for a media stage, used in lifecycle tracking.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StageBackendKind {
    AudioRouter,
    ExternalFfmpeg,
    InternalFfmpeg,
    HlsSegmenter,
    Recording,
}

/// First-class stage lifecycle phase.
///
/// Replaces the coarse `StageRegistered`/`StageStopped` event pair with
/// explicit phases so that outputs can explain why they are waiting on an
/// upstream stage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "phase")]
pub enum StagePhase {
    /// Stage exists in the graph plan but has not been registered yet.
    Planned,
    /// Stage has been registered in the runtime manager.
    Registered,
    WaitingForDependency {
        dependency: StageKey,
    },
    WaitingForMetadata,
    WaitingForParameterSets,
    WaitingForKeyframe,
    WaitingForCapacity {
        backend: StageBackendKind,
    },
    CapacityAcquired {
        backend: StageBackendKind,
    },
    /// Backend is being started (e.g. spawning FFmpeg process).
    StartingBackend {
        backend: StageBackendKind,
    },
    BackendSpawned {
        backend: StageBackendKind,
        pid: Option<u32>,
    },
    FirstInput,
    /// Backend is running and has received input but has not produced output yet.
    RunningNoOutputYet,
    FirstOutput,
    Producing,
    Failed,
    Stopping,
    Stopped,
}

/// Whether a user has requested an output to run or stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum DesiredOutputState {
    /// The output should be actively streaming.
    Running,
    /// The output has been explicitly stopped.
    #[default]
    Stopped,
    /// The output has entered a terminal failure and will not auto-retry.
    Failed,
}

impl DesiredOutputState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for DesiredOutputState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for DesiredOutputState {
    fn from(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "failed" => Self::Failed,
            _ => Self::Stopped,
        }
    }
}

impl From<String> for DesiredOutputState {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Coarse lifecycle state of an active or recently active egress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum EgressStatus {
    #[default]
    Running,
    Stopped,
    Failed,
}

impl EgressStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for EgressStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for EgressStatus {
    fn from(s: &str) -> Self {
        match s {
            "stopped" => Self::Stopped,
            "failed" => Self::Failed,
            _ => Self::Running,
        }
    }
}

impl From<String> for EgressStatus {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Observable runtime phase of an egress output.
///
/// This is what operators see when they query output status. It is separate
/// from `StagePhase` which describes the upstream media stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum EgressPhase {
    /// Egress worker has been registered but has not begun protocol work.
    Starting,
    /// Waiting for an upstream media stage to become ready.
    #[default]
    WaitingUpstream,
    /// Resolving a remote host.
    Resolving,
    /// Attempting to connect to the remote endpoint.
    Connecting,
    /// RTMP handshake is in progress.
    Handshaking,
    /// RTMP application connection is in progress.
    ConnectingApp,
    /// Connected and actively sending media.
    Sending,
    /// HLS output is segmenting locally.
    Segmenting,
    /// HLS output is uploading playlist or segment objects.
    Uploading,
    /// Temporarily failed and will retry.
    Retrying,
    /// Permanently failed, not retrying.
    Failed,
    /// Cleanly stopped by operator request.
    Stopped,
}

impl EgressPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::WaitingUpstream => "waitingUpstream",
            Self::Resolving => "resolving",
            Self::Connecting => "connecting",
            Self::Handshaking => "handshaking",
            Self::ConnectingApp => "connecting_app",
            Self::Sending => "sending",
            Self::Segmenting => "segmenting",
            Self::Uploading => "uploading",
            Self::Retrying => "retrying",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Failed | Self::Stopped)
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Sending | Self::Segmenting | Self::Uploading)
    }
}

impl fmt::Display for EgressPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for EgressPhase {
    fn from(s: &str) -> Self {
        match s {
            "starting" => Self::Starting,
            "waitingUpstream" | "waiting_upstream" => Self::WaitingUpstream,
            "resolving" => Self::Resolving,
            "connecting" => Self::Connecting,
            "handshaking" => Self::Handshaking,
            "connecting_app" | "connectingApp" => Self::ConnectingApp,
            "sending" => Self::Sending,
            "segmenting" => Self::Segmenting,
            "uploading" => Self::Uploading,
            "retrying" => Self::Retrying,
            "failed" => Self::Failed,
            "stopped" => Self::Stopped,
            _ => Self::WaitingUpstream,
        }
    }
}

impl From<String> for EgressPhase {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Observable runtime phase of an active ingest source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum IngestPhase {
    /// No active connection.
    #[default]
    Inactive,
    /// Client is connecting; handshake not complete.
    Connecting,
    /// Media is flowing.
    Receiving,
    /// Connection was lost and we are waiting for a reconnect.
    Reconnecting,
    /// The ingest has failed and will not accept further connections.
    Failed,
}

impl IngestPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Connecting => "connecting",
            Self::Receiving => "receiving",
            Self::Reconnecting => "reconnecting",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for IngestPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for IngestPhase {
    fn from(s: &str) -> Self {
        match s {
            "connecting" => Self::Connecting,
            "receiving" => Self::Receiving,
            "reconnecting" => Self::Reconnecting,
            "failed" => Self::Failed,
            _ => Self::Inactive,
        }
    }
}

impl From<String> for IngestPhase {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Lifecycle phase of a recording artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum RecordingPhase {
    /// Recording is actively writing media.
    #[default]
    Recording,
    /// Recording has stopped and the file is being finalized (remuxed/renamed).
    Finalizing,
    /// Recording is complete and available for playback.
    Ready,
    /// Recording failed; file may be incomplete or absent.
    Failed,
}

impl RecordingPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Finalizing => "finalizing",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Ready | Self::Failed)
    }
}

impl fmt::Display for RecordingPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for RecordingPhase {
    fn from(s: &str) -> Self {
        match s {
            "finalizing" => Self::Finalizing,
            "ready" => Self::Ready,
            "failed" => Self::Failed,
            _ => Self::Recording,
        }
    }
}

impl From<String> for RecordingPhase {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Status of an async job (e.g. agent operation, plan execution).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum JobStatus {
    /// Queued but not yet started.
    #[default]
    Pending,
    /// Currently executing.
    Running,
    /// Completed successfully.
    Succeeded,
    /// Completed with an error.
    Failed,
    /// Cancelled before or during execution.
    Cancelled,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for JobStatus {
    fn from(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Pending,
        }
    }
}

impl From<String> for JobStatus {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Aggregate health state of a pipeline or component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum HealthState {
    /// All outputs are running normally.
    Healthy,
    /// Some outputs are degraded (retrying, waiting, or stalled) but the
    /// pipeline is partially functional.
    Degraded,
    /// No outputs are running and at least one has failed.
    Failed,
    /// State cannot be determined (e.g. no outputs configured).
    #[default]
    Unknown,
}

impl HealthState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for HealthState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for HealthState {
    fn from(s: &str) -> Self {
        match s {
            "healthy" => Self::Healthy,
            "degraded" => Self::Degraded,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        }
    }
}

impl From<String> for HealthState {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desired_output_state_roundtrip() {
        for (s, expected) in [
            ("running", DesiredOutputState::Running),
            ("stopped", DesiredOutputState::Stopped),
            ("failed", DesiredOutputState::Failed),
            ("unknown_garbage", DesiredOutputState::Stopped),
        ] {
            let state = DesiredOutputState::from(s);
            assert_eq!(state, expected, "from({s:?})");
            if s != "unknown_garbage" {
                assert_eq!(state.as_str(), s);
                assert_eq!(state.to_string(), s);
            }
        }
    }

    #[test]
    fn egress_phase_roundtrip() {
        for (s, expected) in [
            ("starting", EgressPhase::Starting),
            ("waitingUpstream", EgressPhase::WaitingUpstream),
            ("resolving", EgressPhase::Resolving),
            ("connecting", EgressPhase::Connecting),
            ("handshaking", EgressPhase::Handshaking),
            ("connecting_app", EgressPhase::ConnectingApp),
            ("sending", EgressPhase::Sending),
            ("segmenting", EgressPhase::Segmenting),
            ("uploading", EgressPhase::Uploading),
            ("retrying", EgressPhase::Retrying),
            ("failed", EgressPhase::Failed),
            ("stopped", EgressPhase::Stopped),
        ] {
            let phase = EgressPhase::from(s);
            assert_eq!(phase, expected, "from({s:?})");
            assert_eq!(phase.as_str(), s);
        }

        assert!(EgressPhase::Failed.is_terminal());
        assert!(EgressPhase::Stopped.is_terminal());
        assert!(!EgressPhase::Sending.is_terminal());
        assert!(EgressPhase::Sending.is_active());
        assert!(!EgressPhase::Retrying.is_active());
    }

    #[test]
    fn egress_status_roundtrip() {
        for (s, expected) in [
            ("running", EgressStatus::Running),
            ("stopped", EgressStatus::Stopped),
            ("failed", EgressStatus::Failed),
            ("unknown", EgressStatus::Running),
        ] {
            let status = EgressStatus::from(s);
            assert_eq!(status, expected, "from({s:?})");
            if s != "unknown" {
                assert_eq!(status.as_str(), s);
                assert_eq!(status.to_string(), s);
            }
        }
    }

    #[test]
    fn ingest_phase_roundtrip() {
        for (s, expected) in [
            ("inactive", IngestPhase::Inactive),
            ("connecting", IngestPhase::Connecting),
            ("receiving", IngestPhase::Receiving),
            ("reconnecting", IngestPhase::Reconnecting),
            ("failed", IngestPhase::Failed),
        ] {
            let phase = IngestPhase::from(s);
            assert_eq!(phase, expected, "from({s:?})");
            assert_eq!(phase.as_str(), s);
        }
    }

    #[test]
    fn recording_phase_roundtrip() {
        for (s, expected) in [
            ("recording", RecordingPhase::Recording),
            ("finalizing", RecordingPhase::Finalizing),
            ("ready", RecordingPhase::Ready),
            ("failed", RecordingPhase::Failed),
        ] {
            let phase = RecordingPhase::from(s);
            assert_eq!(phase, expected, "from({s:?})");
            assert_eq!(phase.as_str(), s);
        }

        assert!(RecordingPhase::Ready.is_terminal());
        assert!(RecordingPhase::Failed.is_terminal());
        assert!(!RecordingPhase::Recording.is_terminal());
    }

    #[test]
    fn job_status_roundtrip() {
        for (s, expected) in [
            ("pending", JobStatus::Pending),
            ("running", JobStatus::Running),
            ("succeeded", JobStatus::Succeeded),
            ("failed", JobStatus::Failed),
            ("cancelled", JobStatus::Cancelled),
        ] {
            let status = JobStatus::from(s);
            assert_eq!(status, expected, "from({s:?})");
            assert_eq!(status.as_str(), s);
        }

        assert!(JobStatus::Succeeded.is_terminal());
        assert!(JobStatus::Cancelled.is_terminal());
        assert!(!JobStatus::Running.is_terminal());
    }

    #[test]
    fn health_state_roundtrip() {
        for (s, expected) in [
            ("healthy", HealthState::Healthy),
            ("degraded", HealthState::Degraded),
            ("failed", HealthState::Failed),
            ("unknown", HealthState::Unknown),
        ] {
            let state = HealthState::from(s);
            assert_eq!(state, expected, "from({s:?})");
            assert_eq!(state.as_str(), s);
        }
    }

    #[test]
    fn state_serde_roundtrip() {
        let phase = EgressPhase::Sending;
        let json = serde_json::to_string(&phase).unwrap();
        assert_eq!(json, "\"sending\"");
        let back: EgressPhase = serde_json::from_str(&json).unwrap();
        assert_eq!(back, phase);
    }
}
