//! Scenario-executor selection for mixed-matrix input rows.

use super::*;

/// Step names reported in the scenario artifact's `plan.executorSteps`.
pub(crate) const MIXED_EXECUTOR_STEPS: [&str; 7] = [
    "prepare",
    "startInput",
    "preFanoutChecks",
    "createOutputs",
    "waitForProgress",
    "runProbes",
    "cleanup",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MixedExecutor {
    FileIngest,
    LiveSrtSingleTrack,
    LiveSrtMultiTrack,
    LiveRtmp,
}

impl MixedExecutor {
    pub(crate) fn for_plan(plan: MixedScenarioPlan) -> Result<Self, String> {
        match (
            plan.source.adapter,
            plan.input.codec(),
            plan.input.is_multi_track(),
        ) {
            (MixedSourceAdapter::FileIngest, _, _) => Ok(Self::FileIngest),
            (MixedSourceAdapter::SrtPublisher, MixedVideoCodec::H264, false) => {
                Ok(Self::LiveSrtSingleTrack)
            }
            (MixedSourceAdapter::SrtPublisher, _, true)
            | (MixedSourceAdapter::SrtPublisher, MixedVideoCodec::H265, false) => {
                Ok(Self::LiveSrtMultiTrack)
            }
            (MixedSourceAdapter::RtmpPublisher, MixedVideoCodec::H264, false) => Ok(Self::LiveRtmp),
            _ => Err(format!(
                "unsupported mixed input case {}",
                plan.input.scenario_id()
            )),
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::FileIngest => "file-ingest",
            Self::LiveSrtSingleTrack => "live-srt-single-track",
            Self::LiveSrtMultiTrack => "live-srt-multi-track",
            Self::LiveRtmp => "live-rtmp",
        }
    }

    pub(crate) async fn execute(
        self,
        env: &MixedEnv,
        api: &RampApi,
        restream_pid: u32,
        case: MixedInputCase,
        resume: &mut MixedResume,
    ) -> Result<Value, String> {
        match self {
            Self::FileIngest => run_mixed_file_config(env, api, restream_pid, case, resume).await,
            Self::LiveSrtSingleTrack => {
                run_mixed_anchor_config(env, api, restream_pid, case, resume).await
            }
            Self::LiveSrtMultiTrack | Self::LiveRtmp => {
                run_mixed_live_config(env, api, restream_pid, case, resume).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_executor_selection_names_live_and_file_shapes() {
        for case in mixed_input_cases() {
            let plan = MixedScenarioPlan::for_input(*case);
            let executor = MixedExecutor::for_plan(plan)
                .unwrap_or_else(|error| panic!("{}: {error}", case.scenario_id()));
            let expected = match (case.protocol(), case.codec(), case.is_multi_track()) {
                (MixedInputProtocol::File, _, _) => "file-ingest",
                (MixedInputProtocol::Srt, MixedVideoCodec::H264, false) => "live-srt-single-track",
                (MixedInputProtocol::Srt, _, true) => "live-srt-multi-track",
                (MixedInputProtocol::Srt, MixedVideoCodec::H265, false) => "live-srt-multi-track",
                (MixedInputProtocol::Rtmp, _, _) => "live-rtmp",
            };
            assert_eq!(executor.name(), expected, "{}", case.scenario_id());
        }
    }
}
