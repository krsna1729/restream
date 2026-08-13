//! Application-layer reconciliation logic that compares desired output and
//! recording state with engine reality and computes convergence actions.

use crate::application::models::Output;
use crate::application::ports::{MetaStore, PipelineStore, PipelineStoreError};
use crate::domain::output_spec::OutputConfig;
use crate::domain::stage::StageKey;
use crate::domain::state::DesiredOutputState;
use crate::media::engine::MediaEngine;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputRetryPolicy {
    pub max_retries: u32,
    pub base_ms: u64,
    pub max_ms: u64,
}

impl OutputRetryPolicy {
    /// Backoff delay before the next retry attempt, in milliseconds.
    ///
    /// `jitter_key` (in production, the output id) desynchronizes retries.
    /// Without jitter, outputs that fail their first dispatch attempt in the
    /// same narrow window (e.g. a burst of output creates hitting a busy
    /// shard) all compute the exact same delay and retry in a second,
    /// equally-synchronized wave against the same still-busy shard. Equal
    /// jitter -- half the computed delay fixed, half spread uniformly across
    /// `[0, half]` -- desynchronizes those retries while still guaranteeing
    /// at least half the intended backoff (unlike full jitter, which can
    /// degenerate to a near-zero wait right after a failure). The jitter is a
    /// deterministic hash of `(jitter_key, shift)`, not a fresh random draw
    /// per call, so repeated calls against the same failure (e.g. successive
    /// reconciler ticks polling `remaining_ms`) return a stable value instead
    /// of jittering around on every poll. Hashing the clamped `shift` rather
    /// than raw `retries` keeps retries beyond the shift limit -- which
    /// already clamp to the same delay -- on the same jitter too.
    pub fn backoff_ms(&self, retries: u32, jitter_key: &str) -> u64 {
        let shift = retries.min(16);
        let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
        let capped = self.base_ms.saturating_mul(multiplier).min(self.max_ms);
        let half = capped / 2;
        half + (jitter_fraction(jitter_key, shift) * half as f64) as u64
    }
}

/// Deterministic pseudo-random fraction in `[0, 1)` derived from `key` and
/// `retries`. Not a real RNG -- the goal is a stable, well-spread value per
/// `(key, retries)` pair so different outputs desynchronize, not
/// unpredictability.
fn jitter_fraction(key: &str, retries: u32) -> f64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    retries.hash(&mut hasher);
    (hasher.finish() as f64) / (u64::MAX as f64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFailureWindow {
    pub retries: u32,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStartAction {
    NotApplicable,
    SkipNoIngest,
    StartNow,
    MarkFailed,
    WaitRetry {
        retries: u32,
        backoff_ms: u64,
        remaining_ms: u64,
    },
}

pub fn decide_output_start_action(
    desired_state: DesiredOutputState,
    is_active: bool,
    effective_has_ingest: bool,
    failure: Option<OutputFailureWindow>,
    policy: OutputRetryPolicy,
    jitter_key: &str,
) -> OutputStartAction {
    if desired_state != DesiredOutputState::Running || is_active {
        return OutputStartAction::NotApplicable;
    }
    if !effective_has_ingest {
        return OutputStartAction::SkipNoIngest;
    }
    if let Some(failure) = failure {
        if failure.retries >= policy.max_retries {
            return OutputStartAction::MarkFailed;
        }
        let backoff_ms = policy.backoff_ms(failure.retries, jitter_key);
        if failure.elapsed_ms < backoff_ms {
            return OutputStartAction::WaitRetry {
                retries: failure.retries,
                backoff_ms,
                remaining_ms: backoff_ms.saturating_sub(failure.elapsed_ms),
            };
        }
    }
    OutputStartAction::StartNow
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStopAction {
    KeepRunning,
    StopBecauseIngestLost,
    StopRequested,
}

pub fn decide_output_stop_action(
    desired_state: DesiredOutputState,
    is_active: bool,
    effective_has_ingest: bool,
) -> OutputStopAction {
    match desired_state {
        DesiredOutputState::Running if is_active && !effective_has_ingest => {
            OutputStopAction::StopBecauseIngestLost
        }
        DesiredOutputState::Stopped if is_active => OutputStopAction::StopRequested,
        _ => OutputStopAction::KeepRunning,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingAction {
    Keep,
    Start,
    Stop,
}

pub fn decide_recording_action(
    recording_enabled: bool,
    effective_has_ingest: bool,
    recording_active: bool,
) -> RecordingAction {
    if recording_enabled && effective_has_ingest && !recording_active {
        RecordingAction::Start
    } else if recording_active && (!recording_enabled || !effective_has_ingest) {
        RecordingAction::Stop
    } else {
        RecordingAction::Keep
    }
}

pub fn next_output_retry_count(previous_retries: Option<u32>, had_progress: bool) -> u32 {
    if had_progress {
        1
    } else {
        previous_retries.unwrap_or(0).saturating_add(1).max(1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputRuntimeSnapshot {
    pub is_active: bool,
    pub effective_has_ingest: bool,
    pub ingest_video_codec: Option<String>,
}

pub async fn load_output_runtime_snapshot(
    engine: &MediaEngine,
    output: &Output,
    ingest_disconnect_grace_ms: u64,
) -> OutputRuntimeSnapshot {
    let is_active = engine.has_active_egress(&output.id).await;
    let has_ingest = engine.has_active_ingest(&output.pipeline_id).await;
    let within_disconnect_grace = engine
        .has_recent_ingest_disconnect(&output.pipeline_id, ingest_disconnect_grace_ms)
        .await;

    OutputRuntimeSnapshot {
        is_active,
        effective_has_ingest: has_ingest || within_disconnect_grace,
        ingest_video_codec: engine.ingest_video_codec(&output.pipeline_id).await,
    }
}

#[derive(Debug, Clone)]
pub struct OutputStageSweepInput<'a> {
    pub pipeline_id: &'a str,
    pub config: OutputConfig,
    pub url: &'a str,
    pub desired_state: DesiredOutputState,
    pub is_active: bool,
    pub effective_has_ingest: bool,
    pub ingest_video_codec: Option<String>,
}

pub fn collect_needed_stage_keys<'a>(
    outputs: impl IntoIterator<Item = OutputStageSweepInput<'a>>,
    policy: &crate::planner::BackendPolicy,
) -> HashSet<StageKey> {
    let mut needed_stages = HashSet::new();
    for output in outputs {
        if output.effective_has_ingest
            && (output.is_active || output.desired_state == DesiredOutputState::Running)
        {
            let planned_output = crate::planner::PlannedOutput::new("", output.config, output.url);
            let plan = crate::planner::plan_pipeline_graph(
                output.pipeline_id,
                output.ingest_video_codec.as_deref(),
                &[planned_output],
                false,
                policy,
            );
            for stage in plan.stages {
                if stage.kind != crate::domain::stage::StageKind::Source {
                    needed_stages.insert(stage.key);
                }
            }
        }
    }
    needed_stages
}

pub fn output_stage_sweep_input<'a>(
    output: &'a Output,
    snapshot: &OutputRuntimeSnapshot,
) -> OutputStageSweepInput<'a> {
    OutputStageSweepInput {
        pipeline_id: output.pipeline_id.as_str(),
        config: output.config.clone(),
        url: &output.url,
        desired_state: output.desired_state,
        is_active: snapshot.is_active,
        effective_has_ingest: snapshot.effective_has_ingest,
        ingest_video_codec: snapshot.ingest_video_codec.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingCommand {
    Start {
        pipeline_name: String,
        pipeline_id: String,
        input_source: Option<String>,
    },
    Stop {
        pipeline_id: String,
    },
}

pub async fn build_recording_reconcile_plan(
    engine: &MediaEngine,
    pipeline_catalog: &dyn PipelineStore,
    meta_store: &dyn MetaStore,
    ingest_disconnect_grace_ms: u64,
) -> Result<Vec<RecordingCommand>, PipelineStoreError> {
    let pipelines = pipeline_catalog.list_pipelines().await?;
    let mut commands = Vec::new();

    for pipeline in pipelines {
        let has_ingest = engine.has_active_ingest(&pipeline.id).await;
        let effective_has_ingest = has_ingest
            || engine
                .has_recent_ingest_disconnect(&pipeline.id, ingest_disconnect_grace_ms)
                .await;
        let rec_enabled =
            crate::application::recording::load_recording_enabled(meta_store, &pipeline.id).await;
        let rec_active = engine.is_recording_active(&pipeline.id).await;

        match decide_recording_action(rec_enabled, effective_has_ingest, rec_active) {
            RecordingAction::Keep => {}
            RecordingAction::Start => commands.push(RecordingCommand::Start {
                pipeline_name: pipeline.name,
                pipeline_id: pipeline.id,
                input_source: pipeline.input_source,
            }),
            RecordingAction::Stop => commands.push(RecordingCommand::Stop {
                pipeline_id: pipeline.id,
            }),
        }
    }

    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::models::Pipeline;
    use crate::application::ports::{MetaLookupFuture, PipelineListFuture};
    use crate::domain::audio_routing::AudioRouting;
    use crate::domain::output_spec::OutputConfig;
    use crate::domain::stage::StageKind;
    use crate::media::metadata::VideoMeta;
    // `Strategy` brings `prop_map` into scope for the `proptest!` block below.
    use proptest::strategy::Strategy;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn test_retry_policy() -> OutputRetryPolicy {
        OutputRetryPolicy {
            max_retries: 10,
            base_ms: 5_000,
            max_ms: 300_000,
        }
    }

    #[test]
    fn start_action_waits_during_backoff_window() {
        // Unjittered ceiling: base_ms 5_000 * 2^2 = 20_000. Equal jitter puts
        // the actual backoff in [10_000, 20_000], so assert the shape of the
        // decision (still waiting, retries/remaining_ms consistent with
        // whatever backoff_ms was actually chosen) rather than the exact
        // pre-jitter value.
        let action = decide_output_start_action(
            DesiredOutputState::Running,
            false,
            true,
            Some(OutputFailureWindow {
                retries: 2,
                elapsed_ms: 5_000,
            }),
            test_retry_policy(),
            "output-a",
        );

        match action {
            OutputStartAction::WaitRetry {
                retries,
                backoff_ms,
                remaining_ms,
            } => {
                assert_eq!(retries, 2);
                assert!(
                    (10_000..=20_000).contains(&backoff_ms),
                    "expected [10000, 20000], got {backoff_ms}"
                );
                assert_eq!(remaining_ms, backoff_ms - 5_000);
            }
            other => panic!("expected WaitRetry, got {other:?}"),
        }
    }

    #[test]
    fn backoff_ms_is_jittered_and_desynchronizes_different_outputs() {
        let policy = test_retry_policy();

        // Same output, same retry count, called repeatedly (as the
        // reconciler does every tick while a WaitRetry is pending): stable,
        // not a fresh random draw each call.
        let first = policy.backoff_ms(2, "output-a");
        let second = policy.backoff_ms(2, "output-a");
        assert_eq!(first, second, "jitter must be stable across repeated calls");

        // Different outputs failing in the same burst must not all compute
        // the exact same delay -- that's the thundering-herd bug this
        // jitter exists to prevent.
        let other_output = policy.backoff_ms(2, "output-b");
        assert_ne!(
            first, other_output,
            "different outputs should not share the same jittered backoff"
        );

        // Both stay within the equal-jitter band regardless of key.
        for backoff in [first, other_output] {
            assert!(
                (10_000..=20_000).contains(&backoff),
                "expected [10000, 20000], got {backoff}"
            );
        }
    }

    proptest::proptest! {
        /// This is the actual claim jitter exists to prove: when many outputs
        /// fail their first dispatch attempt in the same narrow burst window
        /// (the scenario a burst of ~32 concurrent output-creation workers
        /// hitting one busy shard produces), do their retries actually spread
        /// out, or do they still cluster into a smaller number of synchronized
        /// waves that hit the shard together?
        ///
        /// Without jitter every output in the burst shares the exact same
        /// `retries`, so `backoff_ms` was previously 100% concentrated: every
        /// one of them retried at literally the same computed delay. This
        /// checks the jittered replacement directly against that failure
        /// mode, on realistic output-id-shaped keys (`output_<12 hex chars>`,
        /// this repo's actual id format — not integers or short strings, which
        /// could hash unrealistically well or badly) and asserts a bound on
        /// how concentrated the busiest bucket is allowed to be.
        #[test]
        fn jitter_desynchronizes_a_burst_of_same_retry_outputs(
            output_ids in proptest::collection::hash_set(
                "[0-9a-f]{12}".prop_map(|hex| format!("output_{hex}")),
                20..200,
            ),
            retries in 0u32..8,
        ) {
            let policy = test_retry_policy();
            let n = output_ids.len();

            let mut counts: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
            for id in &output_ids {
                let backoff = policy.backoff_ms(retries, id);
                *counts.entry(backoff).or_insert(0) += 1;
            }

            // The unjittered baseline this replaces: every output in the
            // burst lands in exactly one bucket (the busiest bucket holds
            // 100% of the herd). Jitter must break that up substantially --
            // no single retry moment should still account for more than a
            // fifth of the burst. This is a concentration bound, not a
            // uniqueness requirement: some collisions across dozens of
            // outputs sharing a hashed backoff value are expected and fine,
            // as long as the herd is genuinely spread rather than reformed
            // into one or a few synchronized waves.
            let busiest = *counts.values().max().expect("at least one output");
            let max_allowed_busiest = (n as f64 * 0.2).ceil() as u32;
            proptest::prop_assert!(
                busiest <= max_allowed_busiest,
                "busiest retry moment has {busiest}/{n} outputs (retries={retries}), \
                 expected at most {max_allowed_busiest} -- jitter is not desynchronizing this burst"
            );

            // Every jittered value still respects the equal-jitter band, so
            // this never trades desynchronization for an unbounded wait.
            let ceiling = policy.backoff_ms(retries, "output_000000000000").max(
                policy.backoff_ms(retries, "output_ffffffffffff")
            );
            let unjittered_shift = retries.min(16);
            let unjittered = policy
                .base_ms
                .saturating_mul(1u64.checked_shl(unjittered_shift).unwrap_or(u64::MAX))
                .min(policy.max_ms);
            for backoff in counts.keys() {
                proptest::prop_assert!(*backoff <= ceiling.max(unjittered));
                proptest::prop_assert!(*backoff >= unjittered / 2);
            }
        }
    }

    #[test]
    fn start_action_is_not_applicable_when_already_active_or_not_desired_running() {
        assert_eq!(
            decide_output_start_action(
                DesiredOutputState::Running,
                true,
                true,
                None,
                test_retry_policy(),
                "output-a",
            ),
            OutputStartAction::NotApplicable
        );
        assert_eq!(
            decide_output_start_action(
                DesiredOutputState::Stopped,
                false,
                true,
                None,
                test_retry_policy(),
                "output-a",
            ),
            OutputStartAction::NotApplicable
        );
    }

    #[test]
    fn start_action_starts_now_without_prior_failure() {
        let action = decide_output_start_action(
            DesiredOutputState::Running,
            false,
            true,
            None,
            test_retry_policy(),
            "output-a",
        );

        assert_eq!(action, OutputStartAction::StartNow);
    }

    #[test]
    fn start_action_starts_now_once_backoff_window_elapses() {
        // elapsed_ms (20_000) is >= backoff_ms for every jittered value in
        // [10_000, 20_000], so this holds regardless of jitter_key.
        let action = decide_output_start_action(
            DesiredOutputState::Running,
            false,
            true,
            Some(OutputFailureWindow {
                retries: 2,
                elapsed_ms: 20_000,
            }),
            test_retry_policy(),
            "output-a",
        );

        assert_eq!(action, OutputStartAction::StartNow);
    }

    #[test]
    fn backoff_ms_clamps_retries_beyond_shift_limit() {
        let policy = test_retry_policy();

        // Same jitter_key: retries beyond the shift limit hash on the
        // clamped `shift`, not raw `retries`, so they must still match
        // exactly, jitter included.
        assert_eq!(
            policy.backoff_ms(16, "output-a"),
            policy.backoff_ms(u32::MAX, "output-a")
        );
    }

    #[test]
    fn start_action_marks_failed_after_max_retries() {
        let action = decide_output_start_action(
            DesiredOutputState::Running,
            false,
            true,
            Some(OutputFailureWindow {
                retries: 10,
                elapsed_ms: 999_999,
            }),
            test_retry_policy(),
            "output-a",
        );

        assert_eq!(action, OutputStartAction::MarkFailed);
    }

    #[test]
    fn start_action_skips_when_ingest_is_missing() {
        let action = decide_output_start_action(
            DesiredOutputState::Running,
            false,
            false,
            None,
            test_retry_policy(),
            "output-a",
        );

        assert_eq!(action, OutputStartAction::SkipNoIngest);
    }

    #[test]
    fn stop_action_distinguishes_requested_stop_from_ingest_loss() {
        assert_eq!(
            decide_output_stop_action(DesiredOutputState::Running, true, false),
            OutputStopAction::StopBecauseIngestLost
        );
        assert_eq!(
            decide_output_stop_action(DesiredOutputState::Stopped, true, true),
            OutputStopAction::StopRequested
        );
    }

    #[test]
    fn stop_action_keeps_running_by_default() {
        assert_eq!(
            decide_output_stop_action(DesiredOutputState::Running, false, false),
            OutputStopAction::KeepRunning
        );
        assert_eq!(
            decide_output_stop_action(DesiredOutputState::Running, true, true),
            OutputStopAction::KeepRunning
        );
        assert_eq!(
            decide_output_stop_action(DesiredOutputState::Stopped, false, true),
            OutputStopAction::KeepRunning
        );
    }

    #[test]
    fn recording_action_is_purely_state_driven() {
        assert_eq!(
            decide_recording_action(true, true, false),
            RecordingAction::Start
        );
        assert_eq!(
            decide_recording_action(false, true, true),
            RecordingAction::Stop
        );
        assert_eq!(
            decide_recording_action(true, true, true),
            RecordingAction::Keep
        );
    }

    #[test]
    fn retry_count_resets_after_progress() {
        assert_eq!(next_output_retry_count(None, false), 1);
        assert_eq!(next_output_retry_count(Some(1), false), 2);
        assert_eq!(next_output_retry_count(Some(4), true), 1);
    }

    #[test]
    fn stage_sweep_collects_only_needed_stage_keys() {
        let stages = collect_needed_stage_keys(
            [
                OutputStageSweepInput {
                    pipeline_id: "pipe",
                    config: OutputConfig::preset("720p")
                        .with_audio(AudioRouting::SelectTracks { tracks: vec![0] }),
                    url: "rtmp://example/live",
                    desired_state: DesiredOutputState::Running,
                    is_active: false,
                    effective_has_ingest: true,
                    ingest_video_codec: Some("hevc".to_string()),
                },
                OutputStageSweepInput {
                    pipeline_id: "pipe",
                    config: OutputConfig::source(),
                    url: "srt://example:9000",
                    desired_state: DesiredOutputState::Stopped,
                    is_active: false,
                    effective_has_ingest: true,
                    ingest_video_codec: Some("hevc".to_string()),
                },
            ],
            &crate::planner::BackendPolicy::default(),
        );

        let h264_720p = StageKind::video_preset_with_codec("720p", "h264");
        assert!(stages.contains(&StageKey::new("pipe", h264_720p.clone())));
        assert!(stages.contains(&StageKey::new(
            "pipe",
            StageKind::audio_route("atrack:0", h264_720p)
        )));
        assert_eq!(stages.len(), 2);
    }

    #[tokio::test]
    async fn output_runtime_snapshot_reads_active_ingest_and_codec() {
        let engine = MediaEngine::new();
        engine
            .try_register_ingest("pipe", "stream-key", "rtmp")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                "pipe",
                Some(VideoMeta {
                    codec: "hevc".to_string(),
                    ..Default::default()
                }),
                None,
                None,
            )
            .await;
        let output = crate::application::models::Output {
            id: "out-1".to_string(),
            pipeline_id: "pipe".to_string(),
            name: "Output".to_string(),
            url: "rtmp://example/live/test".to_string(),
            monitoring_url: None,
            desired_state: DesiredOutputState::Running,
            config: OutputConfig::source(),
        };

        let snapshot = load_output_runtime_snapshot(&engine, &output, 0).await;

        assert!(!snapshot.is_active);
        assert!(snapshot.effective_has_ingest);
        assert_eq!(snapshot.ingest_video_codec.as_deref(), Some("hevc"));
    }

    #[tokio::test]
    async fn output_runtime_snapshot_honors_recent_disconnect_grace() {
        let engine = MediaEngine::new();
        engine
            .try_register_ingest("pipe", "stream-key", "rtmp")
            .await
            .unwrap();
        engine.unregister_ingest("pipe").await;
        let output = crate::application::models::Output {
            id: "out-1".to_string(),
            pipeline_id: "pipe".to_string(),
            name: "Output".to_string(),
            url: "srt://example:9000".to_string(),
            monitoring_url: None,
            desired_state: DesiredOutputState::Running,
            config: OutputConfig::source(),
        };

        let snapshot = load_output_runtime_snapshot(&engine, &output, 1_000).await;

        assert!(!snapshot.is_active);
        assert!(snapshot.effective_has_ingest);
        assert_eq!(snapshot.ingest_video_codec, None);
    }

    #[test]
    fn output_stage_sweep_input_uses_snapshot_fields() {
        let output = crate::application::models::Output {
            id: "out-1".to_string(),
            pipeline_id: "pipe".to_string(),
            name: "Output".to_string(),
            url: "rtmp://example/live".to_string(),
            monitoring_url: None,
            desired_state: DesiredOutputState::Running,
            config: OutputConfig::preset("720p"),
        };
        let snapshot = OutputRuntimeSnapshot {
            is_active: true,
            effective_has_ingest: false,
            ingest_video_codec: Some("hevc".to_string()),
        };

        let input = output_stage_sweep_input(&output, &snapshot);

        assert_eq!(input.pipeline_id, "pipe");
        assert_eq!(input.config.stage_encoding_label(), "720p");
        assert!(input.is_active);
        assert!(!input.effective_has_ingest);
        assert_eq!(input.ingest_video_codec.as_deref(), Some("hevc"));
    }

    struct FakePipelineStore {
        pipelines: Vec<Pipeline>,
        error: Option<&'static str>,
    }

    impl PipelineStore for FakePipelineStore {
        fn get_pipeline_by_stream_key<'a>(
            &'a self,
            stream_key: &'a str,
        ) -> crate::application::ports::PipelineLookupFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .pipelines
                    .iter()
                    .find(|pipeline| pipeline.stream_key == stream_key)
                    .cloned())
            })
        }

        fn list_pipelines<'a>(&'a self) -> PipelineListFuture<'a> {
            Box::pin(async move {
                if let Some(message) = self.error {
                    return Err(PipelineStoreError::new(message));
                }
                Ok(self.pipelines.clone())
            })
        }

        fn get_pipeline<'a>(
            &'a self,
            id: &'a str,
        ) -> crate::application::ports::PipelineLookupFuture<'a> {
            Box::pin(async move { Ok(self.pipelines.iter().find(|p| p.id == id).cloned()) })
        }

        fn create_pipeline<'a>(
            &'a self,
            _id: &'a str,
            _name: &'a str,
            _stream_key: &'a str,
            _input_source: Option<&'a str>,
            _srt_ingest_policy: Option<&'a str>,
        ) -> crate::application::ports::PipelineCreateFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
        }

        fn update_pipeline<'a>(
            &'a self,
            _id: &'a str,
            _name: &'a str,
            _stream_key: &'a str,
            _input_source: Option<&'a str>,
            _srt_ingest_policy: Option<&'a str>,
        ) -> crate::application::ports::PipelineUpdateFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
        }

        fn delete_pipeline<'a>(
            &'a self,
            _id: &'a str,
        ) -> crate::application::ports::PipelineDeleteFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
        }

        fn get_ingest_host<'a>(
            &'a self,
        ) -> crate::application::ports::PipelineIngestHostFuture<'a> {
            Box::pin(async move { Ok(None) })
        }

        fn update_pipeline_input_source<'a>(
            &'a self,
            pipeline: &'a Pipeline,
            input_source: Option<&'a str>,
        ) -> crate::application::ports::PipelineUpdateFuture<'a> {
            Box::pin(async move {
                let mut updated = pipeline.clone();
                updated.input_source = input_source.map(ToOwned::to_owned);
                Ok(Some(updated))
            })
        }
    }

    struct FakeMetaStore {
        values: Mutex<HashMap<String, String>>,
    }

    impl MetaStore for FakeMetaStore {
        fn get_meta<'a>(&'a self, key: &'a str) -> MetaLookupFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .values
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(key)
                    .cloned())
            })
        }
    }

    #[tokio::test]
    async fn recording_reconcile_plan_starts_enabled_pipeline_with_ingest() {
        let engine = MediaEngine::new();
        engine
            .try_register_ingest("pipeline-1", "stream-one", "rtmp")
            .await
            .unwrap();
        let catalog = FakePipelineStore {
            pipelines: vec![Pipeline {
                id: "pipeline-1".to_string(),
                name: "Pipeline One".to_string(),
                stream_key: "stream-one".to_string(),
                input_source: Some("cam-1".to_string()),
                srt_ingest_policy: None,
            }],
            error: None,
        };
        let store = FakeMetaStore {
            values: Mutex::new(HashMap::from([(
                "recording_enabled:pipeline-1".to_string(),
                "1".to_string(),
            )])),
        };

        let commands = build_recording_reconcile_plan(&engine, &catalog, &store, 0)
            .await
            .unwrap();

        assert_eq!(
            commands,
            vec![RecordingCommand::Start {
                pipeline_name: "Pipeline One".to_string(),
                pipeline_id: "pipeline-1".to_string(),
                input_source: Some("cam-1".to_string()),
            }]
        );
    }

    #[tokio::test]
    async fn recording_reconcile_plan_stops_disabled_active_recording() {
        let engine = MediaEngine::new();
        let _token = engine.register_recording("pipeline-1").await;
        let catalog = FakePipelineStore {
            pipelines: vec![Pipeline {
                id: "pipeline-1".to_string(),
                name: "Pipeline One".to_string(),
                stream_key: "stream-one".to_string(),
                input_source: None,
                srt_ingest_policy: None,
            }],
            error: None,
        };
        let store = FakeMetaStore {
            values: Mutex::new(HashMap::new()),
        };

        let commands = build_recording_reconcile_plan(&engine, &catalog, &store, 0)
            .await
            .unwrap();

        assert_eq!(
            commands,
            vec![RecordingCommand::Stop {
                pipeline_id: "pipeline-1".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn recording_reconcile_plan_propagates_pipeline_store_error() {
        let engine = MediaEngine::new();
        let catalog = FakePipelineStore {
            pipelines: Vec::new(),
            error: Some("catalog unavailable"),
        };
        let store = FakeMetaStore {
            values: Mutex::new(HashMap::new()),
        };

        let result = build_recording_reconcile_plan(&engine, &catalog, &store, 0).await;

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("catalog unavailable".to_string())
        );
    }
}
