use std::sync::Arc;
use std::time::Instant;

use crate::media::engine::MediaEngine;

use super::checks::{
    check_active_outputs, check_engine_status, check_file_ingest_runtime, check_file_source,
    check_gop_analysis, check_ingest_stream_info, check_network_bandwidth,
    check_preview_recording_state, check_publisher_transport, check_ring_buffer_health,
    check_srt_listener_socket, check_system_resources,
};
use super::model::{DiagnosticsReport, FileDiagnosticsContext};

pub async fn run_diagnostics(
    engine: Arc<MediaEngine>,
    pipeline_id: String,
    probe_protocol: String,
    media_dir: String,
    file_context: Option<FileDiagnosticsContext>,
) -> DiagnosticsReport {
    let overall_start = Instant::now();
    let mut checks = Vec::new();

    checks.push(check_engine_status(0, &engine, &pipeline_id).await);

    if probe_protocol == "file" {
        checks.push(check_file_source(1, file_context.as_ref()).await);
        checks.push(check_ingest_stream_info(2, &engine, &pipeline_id).await);
        checks.push(check_gop_analysis(3, &engine, &pipeline_id).await);
        checks
            .push(check_file_ingest_runtime(4, &engine, &pipeline_id, file_context.as_ref()).await);
        checks.push(check_ring_buffer_health(5, &engine, &pipeline_id).await);
        checks.push(check_preview_recording_state(6, &engine, &pipeline_id).await);
        checks.push(check_active_outputs(7, &engine, &pipeline_id).await);
        checks.push(check_system_resources(8, &media_dir).await);
    } else {
        checks.push(check_ingest_stream_info(1, &engine, &pipeline_id).await);
        checks.push(check_gop_analysis(2, &engine, &pipeline_id).await);
        checks.push(check_publisher_transport(3, &engine, &pipeline_id, &probe_protocol).await);
        checks.push(check_ring_buffer_health(4, &engine, &pipeline_id).await);
        checks.push(check_active_outputs(5, &engine, &pipeline_id).await);
        checks.push(check_system_resources(6, &media_dir).await);
        checks.push(check_network_bandwidth(7).await);

        if probe_protocol == "srt" {
            checks.push(check_srt_listener_socket(8, &engine).await);
        }
    }

    DiagnosticsReport {
        protocol: probe_protocol,
        total_duration_ms: overall_start.elapsed().as_millis() as u64,
        checks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn file_diagnostics_return_one_ordered_batch_report() {
        let engine = Arc::new(MediaEngine::new());
        let report = run_diagnostics(
            engine,
            "pipe-test".to_string(),
            "file".to_string(),
            crate::config::DEFAULT_MEDIA_DIR.to_string(),
            None,
        )
        .await;

        assert_eq!(report.protocol, "file");
        assert_eq!(report.checks.len(), 9);
        assert_eq!(report.checks[0].name, "Engine Status");
        assert_eq!(report.checks[8].name, "System Resources");
        assert_eq!(
            report
                .checks
                .iter()
                .map(|check| check.index)
                .collect::<Vec<_>>(),
            (0..9).collect::<Vec<_>>()
        );
    }
}
