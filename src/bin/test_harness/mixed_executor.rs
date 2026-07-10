//! Scenario-executor selection for mixed-matrix input rows.

use std::future::Future;
use std::pin::Pin;

use super::*;

type ScenarioExecutorFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScenarioExecutorStep {
    Prepare,
    StartInput,
    PreFanoutChecks,
    CreateOutputs,
    WaitForProgress,
    RunProbes,
    Cleanup,
}

impl ScenarioExecutorStep {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::StartInput => "startInput",
            Self::PreFanoutChecks => "preFanoutChecks",
            Self::CreateOutputs => "createOutputs",
            Self::WaitForProgress => "waitForProgress",
            Self::RunProbes => "runProbes",
            Self::Cleanup => "cleanup",
        }
    }
}

const DEFAULT_SCENARIO_STEPS: [ScenarioExecutorStep; 7] = [
    ScenarioExecutorStep::Prepare,
    ScenarioExecutorStep::StartInput,
    ScenarioExecutorStep::PreFanoutChecks,
    ScenarioExecutorStep::CreateOutputs,
    ScenarioExecutorStep::WaitForProgress,
    ScenarioExecutorStep::RunProbes,
    ScenarioExecutorStep::Cleanup,
];

pub(crate) struct ScenarioExecutionContext<'a> {
    pub(crate) env: &'a MixedEnv,
    pub(crate) api: &'a RampApi,
    pub(crate) restream_pid: u32,
    pub(crate) case: MixedInputCase,
    pub(crate) resume: &'a mut MixedResume,
}

#[allow(dead_code)]
pub(crate) trait ScenarioExecutor {
    fn name(&self) -> &'static str;

    fn steps(&self) -> &'static [ScenarioExecutorStep] {
        &DEFAULT_SCENARIO_STEPS
    }

    fn step_names(&self) -> Vec<&'static str> {
        self.steps().iter().map(|step| step.as_str()).collect()
    }

    fn prepare<'a>(
        &'a mut self,
        _ctx: &'a mut ScenarioExecutionContext<'a>,
    ) -> ScenarioExecutorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn start_input<'a>(
        &'a mut self,
        _ctx: &'a mut ScenarioExecutionContext<'a>,
    ) -> ScenarioExecutorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn pre_fanout_checks<'a>(
        &'a mut self,
        _ctx: &'a mut ScenarioExecutionContext<'a>,
    ) -> ScenarioExecutorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn create_outputs<'a>(
        &'a mut self,
        _ctx: &'a mut ScenarioExecutionContext<'a>,
    ) -> ScenarioExecutorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn wait_for_progress<'a>(
        &'a mut self,
        _ctx: &'a mut ScenarioExecutionContext<'a>,
    ) -> ScenarioExecutorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn run_probes<'a>(
        &'a mut self,
        _ctx: &'a mut ScenarioExecutionContext<'a>,
    ) -> ScenarioExecutorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn cleanup<'a>(
        &'a mut self,
        _ctx: &'a mut ScenarioExecutionContext<'a>,
    ) -> ScenarioExecutorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn execute<'a>(
        &'a mut self,
        ctx: ScenarioExecutionContext<'a>,
    ) -> ScenarioExecutorFuture<'a, Value>;
}

#[derive(Default)]
struct FileIngestExecutor;

#[derive(Default)]
struct LiveSrtSingleTrackExecutor;

#[derive(Default)]
struct LiveSrtMultiTrackExecutor;

#[derive(Default)]
struct LiveRtmpExecutor;

impl ScenarioExecutor for FileIngestExecutor {
    fn name(&self) -> &'static str {
        "file-ingest"
    }

    fn execute<'a>(
        &'a mut self,
        ctx: ScenarioExecutionContext<'a>,
    ) -> ScenarioExecutorFuture<'a, Value> {
        Box::pin(async move {
            run_mixed_file_config(ctx.env, ctx.api, ctx.restream_pid, ctx.case, ctx.resume).await
        })
    }
}

impl ScenarioExecutor for LiveSrtSingleTrackExecutor {
    fn name(&self) -> &'static str {
        "live-srt-single-track"
    }

    fn execute<'a>(
        &'a mut self,
        ctx: ScenarioExecutionContext<'a>,
    ) -> ScenarioExecutorFuture<'a, Value> {
        Box::pin(async move {
            run_mixed_anchor_config(ctx.env, ctx.api, ctx.restream_pid, ctx.case, ctx.resume).await
        })
    }
}

impl ScenarioExecutor for LiveSrtMultiTrackExecutor {
    fn name(&self) -> &'static str {
        "live-srt-multi-track"
    }

    fn execute<'a>(
        &'a mut self,
        ctx: ScenarioExecutionContext<'a>,
    ) -> ScenarioExecutorFuture<'a, Value> {
        Box::pin(async move {
            run_mixed_live_config(ctx.env, ctx.api, ctx.restream_pid, ctx.case, ctx.resume).await
        })
    }
}

impl ScenarioExecutor for LiveRtmpExecutor {
    fn name(&self) -> &'static str {
        "live-rtmp"
    }

    fn execute<'a>(
        &'a mut self,
        ctx: ScenarioExecutionContext<'a>,
    ) -> ScenarioExecutorFuture<'a, Value> {
        Box::pin(async move {
            run_mixed_live_config(ctx.env, ctx.api, ctx.restream_pid, ctx.case, ctx.resume).await
        })
    }
}

pub(crate) fn scenario_executor_for_plan(
    plan: MixedScenarioPlan,
) -> Result<Box<dyn ScenarioExecutor>, String> {
    match (
        plan.source.adapter,
        plan.input.codec(),
        plan.input.is_multi_track(),
    ) {
        (MixedSourceAdapter::FileIngest, _, _) => Ok(Box::new(FileIngestExecutor)),
        (MixedSourceAdapter::SrtPublisher, MixedVideoCodec::H264, false) => {
            Ok(Box::new(LiveSrtSingleTrackExecutor))
        }
        (MixedSourceAdapter::SrtPublisher, _, true) => Ok(Box::new(LiveSrtMultiTrackExecutor)),
        (MixedSourceAdapter::SrtPublisher, MixedVideoCodec::H265, false) => {
            Ok(Box::new(LiveSrtMultiTrackExecutor))
        }
        (MixedSourceAdapter::RtmpPublisher, MixedVideoCodec::H264, false) => {
            Ok(Box::new(LiveRtmpExecutor))
        }
        _ => Err(format!(
            "unsupported mixed input case {}",
            plan.input.scenario_id()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_executor_selection_names_live_and_file_shapes() {
        for case in mixed_input_cases() {
            let plan = MixedScenarioPlan::for_input(*case);
            let executor = scenario_executor_for_plan(plan)
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

    #[test]
    fn scenario_executors_expose_symmetric_phase_f_steps() {
        for case in mixed_input_cases() {
            let plan = MixedScenarioPlan::for_input(*case);
            let executor = scenario_executor_for_plan(plan)
                .unwrap_or_else(|error| panic!("{}: {error}", case.scenario_id()));
            assert_eq!(
                executor.steps(),
                &DEFAULT_SCENARIO_STEPS,
                "{} should expose the canonical prepare/start/check/fanout/progress/probe/cleanup order",
                case.scenario_id()
            );
        }
    }
}
