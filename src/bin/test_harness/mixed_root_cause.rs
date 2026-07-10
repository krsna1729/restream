//! Root-cause grouping for mixed matrix failures.

use super::*;

pub(crate) const MIXED_ROOT_CAUSE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FailureCause {
    OutputNoProgress,
    OutputBlockedByStage,
    StageWaitingForCapacity,
    StageNoFirstOutput,
    StageNoKeyframe,
    StageNoParameterSets,
    TimestampDiscontinuity,
    ProbeProtocolConnectFailed,
    HlsNoSegments,
    RecordingNotFound,
    RecordingWrongScenario,
    RecordingTmpFileExposed,
    RuntimeLogError,
    LifecycleDidNotStop,
    HarnessInfrastructure,
    ProbeMismatch,
    DecodeFailure,
    Unknown,
}

impl FailureCause {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::OutputNoProgress => "output_no_progress",
            Self::OutputBlockedByStage => "output_blocked_by_stage",
            Self::StageWaitingForCapacity => "stage_waiting_for_capacity",
            Self::StageNoFirstOutput => "stage_no_first_output",
            Self::StageNoKeyframe => "stage_no_keyframe",
            Self::StageNoParameterSets => "stage_no_parameter_sets",
            Self::TimestampDiscontinuity => "timestamp_discontinuity",
            Self::ProbeProtocolConnectFailed => "probe_protocol_connect_failed",
            Self::HlsNoSegments => "hls_no_segments",
            Self::RecordingNotFound => "recording_not_found",
            Self::RecordingWrongScenario => "recording_wrong_scenario",
            Self::RecordingTmpFileExposed => "recording_tmp_file_exposed",
            Self::RuntimeLogError => "runtime_log_error",
            Self::LifecycleDidNotStop => "lifecycle_did_not_stop",
            Self::HarnessInfrastructure => "harness_infrastructure",
            Self::ProbeMismatch => "probe_mismatch",
            Self::DecodeFailure => "decode_failure",
            Self::Unknown => "unknown",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::OutputNoProgress => "Output no progress",
            Self::OutputBlockedByStage => "Output blocked by stage",
            Self::StageWaitingForCapacity => "Stage waiting for capacity",
            Self::StageNoFirstOutput => "Stage produced no first output",
            Self::StageNoKeyframe => "Stage produced no keyframe",
            Self::StageNoParameterSets => "Stage produced no parameter sets",
            Self::TimestampDiscontinuity => "Timestamp discontinuity",
            Self::ProbeProtocolConnectFailed => "Probe protocol connect failed",
            Self::HlsNoSegments => "HLS no segments",
            Self::RecordingNotFound => "Recording not found",
            Self::RecordingWrongScenario => "Recording wrong scenario",
            Self::RecordingTmpFileExposed => "Recording tmp file exposed",
            Self::RuntimeLogError => "Runtime log error",
            Self::LifecycleDidNotStop => "Lifecycle did not stop",
            Self::HarnessInfrastructure => "Harness infrastructure",
            Self::ProbeMismatch => "Probe mismatch",
            Self::DecodeFailure => "Decode failure",
            Self::Unknown => "Unknown",
        }
    }
}

pub(crate) fn classify_mixed_failure(message: &str) -> FailureCause {
    let lower = message.to_ascii_lowercase();
    if lower.contains("blockedby=") || lower.contains("blocked by stage") {
        return FailureCause::OutputBlockedByStage;
    }
    if lower.contains("capacity") || lower.contains("permit") || lower.contains("permits") {
        return FailureCause::StageWaitingForCapacity;
    }
    if lower.contains("firstoutput") || lower.contains("first output") {
        return FailureCause::StageNoFirstOutput;
    }
    if lower.contains("hls")
        && (lower.contains("no segment")
            || lower.contains("404")
            || lower.contains("no playlist")
            || lower.contains("empty playlist")
            || lower.contains("playlist did not"))
    {
        return FailureCause::HlsNoSegments;
    }
    if lower.contains("keyframe") {
        return FailureCause::StageNoKeyframe;
    }
    if lower.contains("parameter set") || lower.contains("sps") || lower.contains("pps") {
        return FailureCause::StageNoParameterSets;
    }
    if lower.contains("timestamp discontinuity")
        || lower.contains("dts gap")
        || lower.contains("duplicate dts")
        || lower.contains("non-monotone")
        || lower.contains("non-monotonic")
    {
        return FailureCause::TimestampDiscontinuity;
    }
    if lower.contains("connection refused")
        || lower.contains("failed to connect")
        || lower.contains("connect failed")
        || lower.contains("connection reset")
        || lower.contains("connection timed out")
    {
        return FailureCause::ProbeProtocolConnectFailed;
    }
    if lower.contains("recording") && lower.contains("not found") {
        return FailureCause::RecordingNotFound;
    }
    if lower.contains("recording") && lower.contains("wrong scenario") {
        return FailureCause::RecordingWrongScenario;
    }
    if lower.contains("recording") && lower.contains(".tmp") {
        return FailureCause::RecordingTmpFileExposed;
    }
    if lower.contains("runtime log") || lower.contains("bad log") {
        return FailureCause::RuntimeLogError;
    }
    if lower.contains("lifecycle") && lower.contains("stop") {
        return FailureCause::LifecycleDidNotStop;
    }
    if lower.contains("harness infrastructure") || lower.contains("preflight") {
        return FailureCause::HarnessInfrastructure;
    }
    if lower.contains("no progress")
        || lower.contains("stalled")
        || lower.contains("did not observe output progress")
    {
        return FailureCause::OutputNoProgress;
    }
    if lower.contains("ffprobe")
        || (lower.contains("expected") && lower.contains("got"))
        || lower.contains("probe failed")
    {
        return FailureCause::ProbeMismatch;
    }
    if lower.contains("decode") || lower.contains("decoder") {
        return FailureCause::DecodeFailure;
    }
    FailureCause::Unknown
}

pub(crate) fn mixed_root_cause_summary_json(failures: &[String]) -> Value {
    let mut groups = std::collections::BTreeMap::<FailureCause, Vec<&String>>::new();
    for failure in failures {
        groups
            .entry(classify_mixed_failure(failure))
            .or_default()
            .push(failure);
    }
    let mut causes: Vec<Value> = groups
        .into_iter()
        .map(|(cause, entries)| {
            let scenarios = entries
                .iter()
                .filter_map(|failure| mixed_failure_scenario(failure))
                .collect::<std::collections::BTreeSet<_>>();
            let cells = entries
                .iter()
                .filter_map(|failure| mixed_failure_cell(failure))
                .collect::<std::collections::BTreeSet<_>>();
            json!({
                "cause": cause.as_str(),
                "label": cause.label(),
                "count": entries.len(),
                "scenarios": scenarios.into_iter().collect::<Vec<_>>(),
                "cells": cells.into_iter().collect::<Vec<_>>(),
                "examples": entries.iter().take(3).map(|entry| (*entry).clone()).collect::<Vec<_>>(),
            })
        })
        .collect();
    causes.sort_by(|left, right| {
        right["count"]
            .as_u64()
            .cmp(&left["count"].as_u64())
            .then_with(|| left["cause"].as_str().cmp(&right["cause"].as_str()))
    });
    json!({
        "schemaVersion": MIXED_ROOT_CAUSE_SCHEMA_VERSION,
        "totalFailures": failures.len(),
        "causes": causes,
    })
}

pub(crate) fn write_mixed_root_cause_summary(
    scenario_path: &Path,
    failures: &[String],
) -> Result<PathBuf, String> {
    let summary_path = mixed_root_cause_summary_path(scenario_path);
    write_json_pretty_atomic(&summary_path, &mixed_root_cause_summary_json(failures))?;
    Ok(summary_path)
}

pub(crate) fn mixed_root_cause_summary_path(scenario_path: &Path) -> PathBuf {
    scenario_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("root-cause-summary.json")
}

fn mixed_failure_scenario(failure: &str) -> Option<String> {
    if let Some((prefix, _)) = failure.split_once(" failed:")
        && let Some(scenario) = prefix.split_whitespace().last()
        && scenario.starts_with("mixed.")
    {
        return Some(scenario.to_string());
    }
    failure
        .split([' ', ':'])
        .find(|part| part.starts_with("mixed."))
        .map(str::to_string)
}

fn mixed_failure_cell(failure: &str) -> Option<String> {
    if let Some((_, rest)) = failure.split_once(" / ")
        && let Some((cell, duplicate_and_more)) = rest.split_once(" / ")
        && duplicate_and_more.starts_with("out")
    {
        return Some(cell.trim().to_string());
    }
    ["cell=", "cellId=", "cell_id="]
        .into_iter()
        .find_map(|needle| {
            failure.split_once(needle).and_then(|(_, rest)| {
                rest.split([' ', ',', ';', '\n'])
                    .next()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_cause_summary_groups_repeated_failure_classes() {
        let failures = vec![
            "mixed input case mixed.live.rtmp.h265.a2.bf2 failed: stream 0 has DTS gap 0.900000s"
                .to_string(),
            "mixed.live.srt.h264.a1.bf0: failed to connect to sink".to_string(),
            "mixed input case mixed.live.rtmp.h264.a1.bf0 failed: stream 0 has duplicate DTS"
                .to_string(),
        ];

        let summary = mixed_root_cause_summary_json(&failures);

        assert_eq!(summary["schemaVersion"], MIXED_ROOT_CAUSE_SCHEMA_VERSION);
        assert_eq!(summary["totalFailures"], 3);
        assert_eq!(summary["causes"][0]["cause"], "timestamp_discontinuity");
        assert_eq!(summary["causes"][0]["count"], 2);
        assert_eq!(
            summary["causes"][1]["cause"],
            "probe_protocol_connect_failed"
        );
    }

    #[test]
    fn root_cause_classifier_covers_governor_taxonomy() {
        let cases = [
            (
                "mixed.foo / rtmp-h264 / out0\n blockedBy=transcode phase=waitingForCapacity",
                FailureCause::OutputBlockedByStage,
            ),
            (
                "stage waiting for capacity permits",
                FailureCause::StageWaitingForCapacity,
            ),
            ("stage no first output", FailureCause::StageNoFirstOutput),
            ("missing keyframe", FailureCause::StageNoKeyframe),
            (
                "missing SPS/PPS parameter sets",
                FailureCause::StageNoParameterSets,
            ),
            (
                "stream 0 timestamp discontinuity",
                FailureCause::TimestampDiscontinuity,
            ),
            (
                "ffprobe failed to connect to sink",
                FailureCause::ProbeProtocolConnectFailed,
            ),
            ("HLS 404 no segments yet", FailureCause::HlsNoSegments),
            ("recording not found", FailureCause::RecordingNotFound),
            (
                "recording wrong scenario identity",
                FailureCause::RecordingWrongScenario,
            ),
            (
                "recording exposed segment.tmp",
                FailureCause::RecordingTmpFileExposed,
            ),
            ("runtime log error found", FailureCause::RuntimeLogError),
            ("lifecycle did not stop", FailureCause::LifecycleDidNotStop),
            (
                "harness infrastructure preflight",
                FailureCause::HarnessInfrastructure,
            ),
            (
                "did not observe output progress",
                FailureCause::OutputNoProgress,
            ),
        ];

        for (message, expected) in cases {
            assert_eq!(classify_mixed_failure(message), expected, "{message}");
        }
    }

    #[test]
    fn root_cause_summary_includes_cell_identity_when_present() {
        let failures = vec![
            "mixed.live.rtmp.h264.a1.bf0 / rtmp-h264-pass / out0\n  blockedBy=transcode"
                .to_string(),
        ];

        let summary = mixed_root_cause_summary_json(&failures);

        assert_eq!(summary["causes"][0]["cause"], "output_blocked_by_stage");
        assert_eq!(summary["causes"][0]["cells"][0], "rtmp-h264-pass");
    }

    #[test]
    fn timestamp_discontinuity_grouped_by_root_cause() {
        let failures = vec![
            "mixed.live.srt.h264.a1.bf0 failed: stream 0 has timestamp discontinuity".to_string(),
            "mixed.live.rtmp.h264.a1.bf0 failed: stream 0 has duplicate DTS".to_string(),
        ];

        let summary = mixed_root_cause_summary_json(&failures);

        assert_eq!(summary["causes"][0]["cause"], "timestamp_discontinuity");
        assert_eq!(summary["causes"][0]["count"], 2);
    }

    #[test]
    fn hls_no_segments_reports_preview_stage_state() {
        let failures = vec![
            "mixed.live.srt.h265.a1.bf0 / hls-preview / out0 failed: HLS 404 no segments yet; phase=waitingForKeyframe terminalStage=hls:preview".to_string(),
        ];

        let summary = mixed_root_cause_summary_json(&failures);

        assert_eq!(summary["causes"][0]["cause"], "hls_no_segments");
        assert_eq!(summary["causes"][0]["cells"][0], "hls-preview");
        assert!(
            summary["causes"][0]["examples"][0]
                .as_str()
                .is_some_and(|example| example.contains("waitingForKeyframe"))
        );
    }

    #[test]
    fn root_cause_summary_path_sits_next_to_scenario_json() {
        let path = Path::new("/tmp/restream-mixed/scenario.json");
        assert_eq!(
            mixed_root_cause_summary_path(path),
            Path::new("/tmp/restream-mixed/root-cause-summary.json")
        );
    }
}
