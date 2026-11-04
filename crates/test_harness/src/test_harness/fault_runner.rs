#[path = "fault_runner/hls_put_timeout.rs"]
mod hls_put_timeout;
#[path = "fault_runner/ingest_gap_retry.rs"]
mod ingest_gap_retry;
#[path = "fault_runner/lifecycle.rs"]
mod lifecycle;
#[path = "fault_runner/sink_flaps.rs"]
mod sink_flaps;
#[path = "fault_runner/srt_publisher_replacement.rs"]
mod srt_publisher_replacement;
#[path = "fault_runner/stage_crash.rs"]
mod stage_crash;

pub(super) use lifecycle::{run_ingest_lifecycle_case, run_publisher_disconnect_case};

use super::*;

/// The recovery fault cases, ending with a transcoder stage crash.
pub(super) async fn recovery_live_cases(
    api: &mut RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    hls_put_port: u16,
    timeout: Duration,
    work_dir: &Path,
) -> Result<Vec<Value>, String> {
    let mut results = Vec::new();
    for case in recovery_transient_cases() {
        results.push(run_recovery_transient_case(api, ports, fixture_h264, sink_port, case).await?);
    }
    for case in input_promotion_cases() {
        results.push(
            run_input_promotion_case(api, ports, fixture_h264, sink_port, timeout, case).await?,
        );
    }
    results.push(
        srt_publisher_replacement::rapid_srt_replacement_preserves_egress(
            api,
            ports,
            fixture_h264,
            sink_port,
            timeout,
        )
        .await?,
    );
    results.push(
        ingest_gap_retry::egress_retry_survives_ingest_gap(
            api,
            ports,
            fixture_h264,
            sink_port,
            timeout,
        )
        .await?,
    );
    results.push(
        hls_put_timeout::hls_put_timeout_recovers_after_restart(
            api,
            ports,
            fixture_h264,
            hls_put_port,
            timeout,
        )
        .await?,
    );
    results.push(
        sink_flaps::rtmp_sink_flaps_surface_instability(
            api,
            ports,
            fixture_h264,
            sink_port,
            timeout,
        )
        .await?,
    );
    results.push(
        sink_flaps::srt_sink_flaps_surface_instability(api, ports, fixture_h264, timeout, work_dir)
            .await?,
    );
    results
        .push(stage_crash::transcoder_crash_recovers(api, ports, fixture_h264, sink_port).await?);
    Ok(results)
}
