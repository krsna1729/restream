#[path = "fault_runner/lifecycle.rs"]
mod lifecycle;
#[path = "fault_runner/recovery.rs"]
mod recovery;
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
    let mut results = recovery::recovery_live_cases(
        api,
        ports,
        fixture_h264,
        sink_port,
        hls_put_port,
        timeout,
        work_dir,
    )
    .await?;
    results
        .push(stage_crash::transcoder_crash_recovers(api, ports, fixture_h264, sink_port).await?);
    Ok(results)
}
