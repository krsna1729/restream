use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::process::ChildStdout;

use crate::ingest_publisher::{
    Container, IngestTimestamps, PUBLISH_CHUNK_BYTES, PublishTarget, SegmentPublisher,
};

use super::ExternalFileIngestRuntime;

/// Detach the pipe from CONTROL before registering it on the media reactor.
/// Timestamp state crosses the executor once per attempt, never per packet.
pub(super) async fn pump_stdout(
    runtime: Arc<ExternalFileIngestRuntime>,
    stdout: ChildStdout,
    timestamps: IngestTimestamps,
) -> Result<(IngestTimestamps, Result<(), String>), String> {
    let stdout = match stdout.into_owned_fd() {
        Ok(stdout) => stdout,
        Err(error) => {
            return Ok((
                timestamps,
                Err(format!("Failed to detach ffmpeg stdout: {error}")),
            ));
        }
    };
    crate::executor::run_with_class(
        crate::executor::MediaService {
            class: crate::executor::MediaServiceClass::FileIngest,
            pipeline_id: runtime.pipeline_id.clone(),
            stage: runtime.ingest_id.clone(),
        },
        runtime.registration.cancel_token.clone(),
        async move {
            let mut publisher = SegmentPublisher::new(Container::Ts.demux(), timestamps);
            // Keep the descriptor's nonblocking flags; from_std registers it with
            // the currently entered MEDIA runtime, not its original CONTROL reactor.
            let result = match ChildStdout::from_std(std::process::ChildStdout::from(stdout)) {
                Ok(stdout) => pump_stdout_inner(&runtime, stdout, &mut publisher).await,
                Err(error) => Err(format!("Failed to register ffmpeg stdout: {error}")),
            };
            (publisher.into_timestamps(), result)
        },
    )
    .await
}

async fn pump_stdout_inner(
    runtime: &ExternalFileIngestRuntime,
    mut stdout: ChildStdout,
    publisher: &mut SegmentPublisher,
) -> Result<(), String> {
    let target = PublishTarget::open(
        runtime.engine.clone(),
        runtime.pipeline_id.clone(),
        runtime.registration.clone(),
        runtime.ring_buffer.clone(),
    )
    .await?;
    let mut buf = vec![0u8; PUBLISH_CHUNK_BYTES];

    loop {
        let read = tokio::select! {
            biased;
            _ = runtime.registration.cancel_token.cancelled() => break,
            result = stdout.read(&mut buf) => result,
        }
        .map_err(|error| format!("Failed to read ffmpeg stdout: {error}"))?;

        if read == 0 {
            break;
        }
        // Cancellation is observed at every bounded (at most 64 KiB) read.
        publisher
            .publish(&target, &buf[..read])
            .await
            .map_err(|error| format!("Failed to demux ffmpeg stdout: {error}"))?;
    }

    Ok(())
}
