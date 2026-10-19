#[path = "fault_recovery/egress.rs"]
mod egress;
#[path = "fault_recovery/resilience.rs"]
mod resilience;
#[path = "fault_recovery/rtmps_egress.rs"]
mod rtmps_egress;
#[path = "fault_recovery/srt_stall.rs"]
mod srt_stall;

#[cfg(test)]
pub(crate) use egress::{
    OutputRetryObservation, effective_fault_output_stall_siblings, output_retry_or_cleanup_phase_ok,
};
pub(crate) use egress::{
    fault_egress_retry, fault_output_stall, wait_for_output_retry_observation,
    wait_for_outputs_live_and_progressing,
};
pub(crate) use resilience::{
    RECOVERY_WARM_VIDEO_MIN, create_pipeline, create_pipeline_with_stream_key, delete_pipeline_v1,
    disconnect_grace_remaining_bounded, fault_resilience, health_input_snapshot,
    input_disconnect_cleared, observe_final_output, recovery, wait_for_output_running,
    wait_for_output_running_and_sink_video_above, wait_for_sink_video_above,
};
pub(crate) use srt_stall::fault_srt_output_stall;

#[cfg(test)]
mod tests {
    use super::egress;
    #[test]
    fn retry_tuning_readback_rejects_a_missing_or_ineffective_override() {
        let path =
            std::env::temp_dir().join(format!("restream-retry-tuning-{}.log", std::process::id()));
        let expected = [(
            "RESTREAM_RECONCILE_INTERVAL_MS",
            "reconcilerIntervalMs",
            100,
        )];
        let cases = [
            ("no effective configuration", false),
            (r#"{"tuning":{"reconcilerIntervalMs":1000}}"#, false),
            (r#"{"tuning":{}}"#, false),
            (r#"{"tuning":{"reconcilerIntervalMs":100}}"#, true),
        ];
        let results: Vec<_> = cases
            .iter()
            .map(|(log, succeeds)| {
                std::fs::write(&path, log).unwrap();
                (egress::verify_effective_tuning(&path, &expected), *succeeds)
            })
            .collect();
        std::fs::remove_file(path).unwrap();
        for (result, succeeds) in results {
            assert_eq!(result.is_ok(), succeeds, "{result:?}");
        }
    }
}
