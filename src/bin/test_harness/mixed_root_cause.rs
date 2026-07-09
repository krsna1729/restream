//! Root-cause grouping for mixed matrix failures.

use super::*;

pub(crate) const MIXED_ROOT_CAUSE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MixedFailureCause {
    WaitingForCapacity,
    NoKeyframe,
    TimestampDiscontinuity,
    NoHlsSegments,
    ProtocolConnectFailure,
    NoProgress,
    ProbeMismatch,
    DecodeFailure,
    Unknown,
}

impl MixedFailureCause {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::WaitingForCapacity => "waiting_for_capacity",
            Self::NoKeyframe => "no_keyframe",
            Self::TimestampDiscontinuity => "timestamp_discontinuity",
            Self::NoHlsSegments => "no_hls_segments",
            Self::ProtocolConnectFailure => "protocol_connect_failure",
            Self::NoProgress => "no_progress",
            Self::ProbeMismatch => "probe_mismatch",
            Self::DecodeFailure => "decode_failure",
            Self::Unknown => "unknown",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::WaitingForCapacity => "Waiting for capacity",
            Self::NoKeyframe => "No keyframe",
            Self::TimestampDiscontinuity => "Timestamp discontinuity",
            Self::NoHlsSegments => "No HLS segments",
            Self::ProtocolConnectFailure => "Protocol connect failure",
            Self::NoProgress => "No output progress",
            Self::ProbeMismatch => "Probe mismatch",
            Self::DecodeFailure => "Decode failure",
            Self::Unknown => "Unknown",
        }
    }
}

pub(crate) fn classify_mixed_failure(message: &str) -> MixedFailureCause {
    let lower = message.to_ascii_lowercase();
    if lower.contains("capacity") || lower.contains("permit") || lower.contains("permits") {
        return MixedFailureCause::WaitingForCapacity;
    }
    if lower.contains("keyframe") {
        return MixedFailureCause::NoKeyframe;
    }
    if lower.contains("timestamp discontinuity")
        || lower.contains("dts gap")
        || lower.contains("duplicate dts")
        || lower.contains("non-monotone")
        || lower.contains("non-monotonic")
    {
        return MixedFailureCause::TimestampDiscontinuity;
    }
    if lower.contains("hls")
        && (lower.contains("no segment")
            || lower.contains("no playlist")
            || lower.contains("empty playlist")
            || lower.contains("playlist did not"))
    {
        return MixedFailureCause::NoHlsSegments;
    }
    if lower.contains("connection refused")
        || lower.contains("failed to connect")
        || lower.contains("connect failed")
        || lower.contains("connection reset")
        || lower.contains("connection timed out")
    {
        return MixedFailureCause::ProtocolConnectFailure;
    }
    if lower.contains("no progress")
        || lower.contains("stalled")
        || lower.contains("did not observe output progress")
        || lower.contains("firstoutput")
    {
        return MixedFailureCause::NoProgress;
    }
    if lower.contains("ffprobe")
        || (lower.contains("expected") && lower.contains("got"))
        || lower.contains("probe failed")
    {
        return MixedFailureCause::ProbeMismatch;
    }
    if lower.contains("decode") || lower.contains("decoder") {
        return MixedFailureCause::DecodeFailure;
    }
    MixedFailureCause::Unknown
}

pub(crate) fn mixed_root_cause_summary_json(failures: &[String]) -> Value {
    let mut groups = std::collections::BTreeMap::<MixedFailureCause, Vec<&String>>::new();
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
            json!({
                "cause": cause.as_str(),
                "label": cause.label(),
                "count": entries.len(),
                "scenarios": scenarios.into_iter().collect::<Vec<_>>(),
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
        assert_eq!(summary["causes"][1]["cause"], "protocol_connect_failure");
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
