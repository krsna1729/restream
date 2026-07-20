//! External FFmpeg-backed file-ingest runtime.
//!
//! The application layer chooses when an ingest starts. This module owns the
//! spawned process, MPEG-TS transport, retry loop, and runtime cleanup.

mod process;
mod transport;

use std::path::PathBuf;
use std::sync::Arc;

use tracing::{error, warn};

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::ring_buffer::RingBuffer;

use process::SpawnedExternalFileIngest;
use transport::FileIngestTimestamps;

pub(crate) struct ExternalFileIngestSource {
    pub file_path: PathBuf,
    pub start_time: String,
    pub loop_enabled: bool,
    pub live_optimized: bool,
    pub target_gop_seconds: u32,
}

pub(crate) struct ExternalFileIngestRuntime {
    pub engine: Arc<MediaEngine>,
    pub ingest_id: String,
    pub pipeline_id: String,
    pub source: ExternalFileIngestSource,
    pub ring_buffer: Arc<RingBuffer>,
    pub registration: IngestRegistration,
}

/// Spawns and supervises one registered external file-ingest attempt.
///
/// The caller remains responsible for rolling back its registration when the
/// initial FFmpeg process cannot be spawned.
pub(crate) fn start_external_file_ingest(runtime: ExternalFileIngestRuntime) -> Result<(), String> {
    let spawned = process::spawn_child(&runtime.source)?;
    tokio::spawn(run_external_file_ingest(runtime, spawned));
    Ok(())
}

async fn run_external_file_ingest(
    runtime: ExternalFileIngestRuntime,
    mut spawned: SpawnedExternalFileIngest,
) {
    let cancel = runtime.registration.cancel_token.clone();
    let mut timestamps = FileIngestTimestamps::default();

    loop {
        runtime
            .engine
            .file_ingests
            .children
            .write()
            .await
            .insert(runtime.ingest_id.clone(), spawned.child);

        let stdout = transport::pump_stdout(&runtime, spawned.stdout, &mut timestamps);
        let stderr = process::capture_stderr(&runtime.ingest_id, spawned.stderr);
        let (stdout_result, stderr_result) = tokio::join!(stdout, stderr);

        let mut exit_status = None;
        if let Some(mut child) = runtime
            .engine
            .take_file_ingest_child(&runtime.ingest_id)
            .await
        {
            exit_status = child.wait().await.ok();
        }

        if let Err(err) = stdout_result
            && !cancel.is_cancelled()
        {
            error!(
                ingest_id = %runtime.ingest_id,
                err = %err,
                "file-ingest stdout reader failed"
            );
            runtime
                .engine
                .record_ingest_disconnect_if_current(
                    &runtime.pipeline_id,
                    &runtime.registration,
                    Some("stdout"),
                    Some(err),
                    true,
                )
                .await;
        }
        if let Err(err) = stderr_result
            && !cancel.is_cancelled()
        {
            error!(
                ingest_id = %runtime.ingest_id,
                err = %err,
                "file-ingest stderr reader failed"
            );
            runtime
                .engine
                .record_ingest_disconnect_if_current(
                    &runtime.pipeline_id,
                    &runtime.registration,
                    Some("stderr"),
                    Some(err.to_string()),
                    true,
                )
                .await;
        }

        if let Some(status) = exit_status
            && !status.success()
            && !cancel.is_cancelled()
        {
            warn!(
                ingest_id = %runtime.ingest_id,
                status = %status,
                "ffmpeg exited unsuccessfully"
            );
            runtime
                .engine
                .record_ingest_disconnect_if_current(
                    &runtime.pipeline_id,
                    &runtime.registration,
                    Some("exit"),
                    Some(format!("ffmpeg exited with status {status}")),
                    true,
                )
                .await;
        } else if exit_status.is_some() && !cancel.is_cancelled() && !runtime.source.loop_enabled {
            runtime
                .engine
                .record_ingest_disconnect_if_current(
                    &runtime.pipeline_id,
                    &runtime.registration,
                    Some("eof"),
                    Some("file ingest reached end of input".to_string()),
                    false,
                )
                .await;
        }

        if cancel.is_cancelled() || !runtime.source.loop_enabled {
            break;
        }

        match process::spawn_child(&runtime.source) {
            Ok(next) => spawned = next,
            Err(err) => {
                error!(
                    ingest_id = %runtime.ingest_id,
                    err = %err,
                    "file-ingest restart failed"
                );
                break;
            }
        }
    }

    runtime
        .engine
        .clear_file_ingest_running(&runtime.ingest_id)
        .await;
    runtime
        .engine
        .unregister_ingest_if_current(&runtime.pipeline_id, &runtime.registration)
        .await;
}
