#[path = "fault_recovery/egress.rs"]
mod egress;
#[path = "fault_recovery/resilience.rs"]
mod resilience;
#[path = "fault_recovery/srt_stall.rs"]
mod srt_stall;

#[cfg(test)]
pub(crate) use egress::{
    OutputRetryObservation, effective_fault_output_stall_siblings, output_retry_or_cleanup_phase_ok,
};
pub(crate) use egress::{
    fault_egress_retry, fault_output_stall, wait_for_output_retry_observation,
};
pub(crate) use resilience::{
    RECOVERY_WARM_VIDEO_MIN, create_pipeline, create_pipeline_with_stream_key, delete_pipeline_v1,
    disconnect_grace_remaining_bounded, fault_resilience, health_input_snapshot,
    input_disconnect_cleared, observe_final_output, recovery, wait_for_output_running,
    wait_for_output_running_and_sink_video_above, wait_for_sink_video_above,
};
pub(crate) use srt_stall::fault_srt_egress_stalled_destination;
