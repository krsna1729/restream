//! Manifest-shaped fault/recovery case axes and typed JSON validation.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::{HarnessSrtMode, TestPorts, harness_srt_ffmpeg_url, harness_srt_output_url};

/// Declarative cell for retry-budget exhaustion coverage against an unreachable sink.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetryBudgetCase {
    pub(crate) test_name: String,
    pub(crate) log_label: String,
    pub(crate) pipeline: String,
    pub(crate) output_name: String,
    pub(crate) protocol: HarnessPublisherProtocol,
    pub(crate) dead_sink_offset: u16,
}

/// Publisher transport details that vary across otherwise identical live scenarios.
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum HarnessPublisherProtocol {
    Rtmp,
    Srt,
}

impl HarnessPublisherProtocol {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Rtmp => "rtmp",
            Self::Srt => "srt",
        }
    }

    pub(crate) fn publish_url(self, ports: &TestPorts, stream_key: &str) -> String {
        match self {
            Self::Rtmp => format!("rtmp://127.0.0.1:{}/live/{stream_key}", ports.rtmp),
            Self::Srt => {
                harness_srt_ffmpeg_url(ports.srt, stream_key, HarnessSrtMode::Publish, None)
            }
        }
    }

    pub(crate) const fn ffmpeg_format(self) -> &'static str {
        match self {
            Self::Rtmp => "flv",
            Self::Srt => "mpegts",
        }
    }

    pub(crate) const fn map_all_streams(self) -> bool {
        matches!(self, Self::Srt)
    }

    pub(crate) fn retry_limit_output_url(self, dead_sink_port: u16) -> String {
        match self {
            Self::Rtmp => format!("rtmp://127.0.0.1:{dead_sink_port}/live/retry-limit"),
            Self::Srt => {
                harness_srt_output_url(dead_sink_port, "retry-limit", HarnessSrtMode::Publish)
            }
        }
    }
}

/// Declarative cell for transient ingest-drop recovery coverage.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryTransientCase {
    pub(crate) test_name: String,
    pub(crate) log_label: String,
    pub(crate) pipeline: String,
    pub(crate) output_name: String,
    pub(crate) sink_stream: String,
    pub(crate) protocol: HarnessPublisherProtocol,
    pub(crate) wait_input_off_after_drop: bool,
    pub(crate) require_media_ready_on_resume: bool,
    pub(crate) second_reconnect_checks_flapping: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InputPromotionCase {
    pub(crate) test_name: String,
    pub(crate) pipeline: String,
    pub(crate) output_name: String,
    pub(crate) sink_stream: String,
    pub(crate) protocol: HarnessPublisherProtocol,
}

/// Declarative cell for publisher disconnect fault coverage.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PublisherDisconnectCase {
    pub(crate) test_name: String,
    pub(crate) log_label: String,
    pub(crate) pipeline: String,
    pub(crate) protocol: HarnessPublisherProtocol,
}

/// Runtime feature whose cleanup is tied to input lifecycle.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum IngestLifecycleKind {
    FileIngest,
    HlsPreview,
    Recording,
}

/// How a file-ingest lifecycle cell reaches input-off.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FileIngestCompletion {
    EofRestart,
    Stop,
}

/// Declarative cell for input lifecycle cleanup coverage.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IngestLifecycleCase {
    pub(crate) test_name: String,
    pub(crate) pipeline: String,
    pub(crate) kind: IngestLifecycleKind,
    pub(crate) file_completion: Option<FileIngestCompletion>,
    pub(crate) input_off_timeout_secs: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FaultCasesManifest {
    pub(crate) publisher_disconnect: Vec<PublisherDisconnectCase>,
    pub(crate) retry_budget: Vec<RetryBudgetCase>,
    pub(crate) recovery_transient: Vec<RecoveryTransientCase>,
    pub(crate) input_promotion: Vec<InputPromotionCase>,
    pub(crate) ingest_lifecycle: Vec<IngestLifecycleCase>,
}

static FAULT_CASES_MANIFEST: OnceLock<FaultCasesManifest> = OnceLock::new();

pub(crate) fn fault_cases_manifest() -> &'static FaultCasesManifest {
    FAULT_CASES_MANIFEST.get_or_init(|| {
        serde_json::from_str(include_str!("fault_cases.json"))
            .expect("embedded fault_cases.json should define valid typed harness cases")
    })
}

pub(crate) fn publisher_disconnect_cases() -> &'static [PublisherDisconnectCase] {
    &fault_cases_manifest().publisher_disconnect
}

pub(crate) fn retry_budget_cases() -> &'static [RetryBudgetCase] {
    &fault_cases_manifest().retry_budget
}

pub(crate) fn recovery_transient_cases() -> &'static [RecoveryTransientCase] {
    &fault_cases_manifest().recovery_transient
}

pub(crate) fn input_promotion_cases() -> &'static [InputPromotionCase] {
    &fault_cases_manifest().input_promotion
}

pub(crate) fn ingest_lifecycle_case(
    test_name: &str,
) -> Result<&'static IngestLifecycleCase, String> {
    fault_cases_manifest()
        .ingest_lifecycle
        .iter()
        .find(|case| case.test_name == test_name)
        .ok_or_else(|| format!("ingest lifecycle case {test_name} missing from manifest"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_promotion_matrix_covers_rtmp_and_srt() {
        let cases = input_promotion_cases();

        assert_eq!(cases.len(), 2);
        assert!(matches!(cases[0].protocol, HarnessPublisherProtocol::Rtmp));
        assert!(matches!(cases[1].protocol, HarnessPublisherProtocol::Srt));
    }
}
