//! Mixed-runner check-selection and resume-gate helpers.

use std::time::Duration;

use crate::{env_secs, scaled_output_progress_timeout};

use super::{MixedInputCase, MixedVideoCodec};

/// Output checks that require live output progress before assertions begin.
pub(crate) fn mixed_output_checks_need_live_progress_gate(only_checks: Option<&[String]>) -> bool {
    let check_selected =
        |check: &str| only_checks.is_none_or(|items| items.iter().any(|item| item == check));
    let direct_signal_sinks = only_checks.is_some_and(|items| {
        !items.is_empty()
            && items
                .iter()
                .all(|item| item == "signal" || item == "soak-drift")
    });
    check_selected("ffprobe") || (check_selected("signal") && !direct_signal_sinks)
}

/// Excludes helper outputs (for example, HLS preview) from progress gates.
pub(crate) fn mixed_progress_output_ids(
    output_ids: &[String],
    non_progress_output_id: &str,
) -> Vec<String> {
    output_ids
        .iter()
        .filter(|output_id| output_id.as_str() != non_progress_output_id)
        .cloned()
        .collect()
}

/// Progress timeout for mixed scenarios, scaled by fanout and tunable by env.
pub(crate) fn mixed_output_progress_timeout(output_count: usize) -> Duration {
    let base_secs = env_secs("MIXED_PROGRESS_TIMEOUT_BASE_SECS", 60);
    let per_output_secs = env_secs("MIXED_PROGRESS_TIMEOUT_PER_OUTPUT_SECS", 3);
    let cap_secs = env_secs("MIXED_PROGRESS_TIMEOUT_CAP_SECS", 360);
    scaled_output_progress_timeout(output_count, base_secs, per_output_secs, cap_secs)
}

/// Progress timeout for a concrete mixed input case.
///
/// HEVC rows can launch both scaled HEVC stages and HEVC→H.264 codec edges.
/// The steady-state probes already account for that codec-edge startup cost;
/// this gate needs the same scenario-specific budget so it does not fail just
/// before the shared stages produce their first bytes.
pub(crate) fn mixed_output_progress_timeout_for_case(
    case: MixedInputCase,
    output_count: usize,
) -> Duration {
    let base = mixed_output_progress_timeout(output_count);
    if !matches!(case.codec(), MixedVideoCodec::H265) {
        return base;
    }

    let extra_secs = env_secs("MIXED_HEVC_PROGRESS_EXTRA_SECS", 90);
    let cap_secs = env_secs("MIXED_PROGRESS_TIMEOUT_CAP_SECS", 360);
    (base + Duration::from_secs(extra_secs)).min(Duration::from_secs(cap_secs))
}

/// Resume gate that skips mixed checks until a requested assertion id is reached.
pub(crate) struct MixedResume {
    pub(crate) target: Option<String>,
    pub(crate) active: bool,
}

impl MixedResume {
    pub(crate) fn new(target: Option<String>) -> Self {
        Self {
            active: target.is_none(),
            target,
        }
    }

    pub(crate) fn allows(&mut self, id: &str) -> bool {
        if self.active {
            return true;
        }
        if self.target.as_deref() == Some(id) {
            self.active = true;
            return true;
        }
        false
    }
}
