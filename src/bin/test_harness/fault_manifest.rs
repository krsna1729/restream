//! Manifest-shaped fault/recovery case axes and typed JSON validation.

use std::sync::OnceLock;

use serde::Deserialize;

use super::TestPorts;

/// Declarative cell for retry-budget exhaustion coverage against an unreachable sink.
#[derive(Deserialize)]
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
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum HarnessPublisherProtocol {
    Rtmp,
    Srt,
}

impl HarnessPublisherProtocol {
    pub(crate) fn publish_url(self, ports: &TestPorts, stream_key: &str) -> String {
        match self {
            Self::Rtmp => format!("rtmp://127.0.0.1:{}/live/{stream_key}", ports.rtmp),
            Self::Srt => format!(
                "srt://127.0.0.1:{}?streamid=publish:live/{stream_key}&pkt_size=1316",
                ports.srt
            ),
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
            Self::Srt => format!(
                "srt://127.0.0.1:{dead_sink_port}?streamid=publish:live/retry-limit&pkt_size=1316"
            ),
        }
    }
}

/// Declarative cell for transient ingest-drop recovery coverage.
#[derive(Deserialize)]
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

pub(crate) fn ingest_lifecycle_case(
    test_name: &str,
) -> Result<&'static IngestLifecycleCase, String> {
    fault_cases_manifest()
        .ingest_lifecycle
        .iter()
        .find(|case| case.test_name == test_name)
        .ok_or_else(|| format!("ingest lifecycle case {test_name} missing from manifest"))
}
