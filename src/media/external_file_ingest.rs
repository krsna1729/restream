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
    let runtime = Arc::new(runtime);
    let cleanup = CoordinatorCleanup::new(runtime.clone(), spawned.child.id());
    tokio::spawn(run_external_file_ingest(runtime, spawned, cleanup));
    Ok(())
}

/// Even an aborted supervisor must reap its child and remove its registration
/// on CONTROL. Constructed before spawn to cover an abort before the first poll.
struct CoordinatorCleanup {
    runtime: Arc<ExternalFileIngestRuntime>,
    control: tokio::runtime::Handle,
    armed: bool,
    child_pid: Option<u32>,
}

impl CoordinatorCleanup {
    fn new(runtime: Arc<ExternalFileIngestRuntime>, child_pid: Option<u32>) -> Self {
        Self {
            runtime,
            control: tokio::runtime::Handle::current(),
            armed: true,
            child_pid,
        }
    }

    async fn finish(&mut self) {
        cleanup_runtime(&self.runtime, self.child_pid).await;
        self.armed = false;
    }

    async fn fail(&mut self, error: String) {
        self.runtime.registration.cancel_token.cancel();
        self.runtime
            .engine
            .record_ingest_disconnect_if_current(
                &self.runtime.pipeline_id,
                &self.runtime.registration,
                Some("media-executor"),
                Some(error),
                true,
            )
            .await;
        self.finish().await;
    }
}

impl Drop for CoordinatorCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.runtime.registration.cancel_token.cancel();
        let runtime = self.runtime.clone();
        let child_pid = self.child_pid;
        self.control.spawn(async move {
            cleanup_runtime(&runtime, child_pid).await;
        });
    }
}

async fn take_owned_child(
    runtime: &ExternalFileIngestRuntime,
    child_pid: Option<u32>,
) -> Option<tokio::process::Child> {
    let mut children = runtime.engine.file_ingests.children.write().await;
    if child_pid.is_some()
        && children
            .get(&runtime.ingest_id)
            .is_some_and(|child| child.id() == child_pid)
    {
        children.remove(&runtime.ingest_id)
    } else {
        None
    }
}

async fn stop_owned_child(runtime: &ExternalFileIngestRuntime, child_pid: Option<u32>) {
    if let Some(mut child) = take_owned_child(runtime, child_pid).await {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

async fn cleanup_runtime(runtime: &ExternalFileIngestRuntime, child_pid: Option<u32>) {
    stop_owned_child(runtime, child_pid).await;
    {
        // Replacement registration needs sessions.write(): hold this guard
        // through active removal so a newer generation cannot be cleared.
        // Never nest children and active guards (snapshots take active first).
        let sessions = runtime.engine.ingests.sessions.read().await;
        if sessions
            .get(&runtime.registration.input_id)
            .is_none_or(|session| {
                session.pipeline_id == runtime.pipeline_id
                    && session.attempt_id == runtime.registration.attempt_id
            })
        {
            runtime
                .engine
                .file_ingests
                .active
                .write()
                .await
                .remove(&runtime.ingest_id);
        }
    }
    runtime
        .engine
        .unregister_ingest_if_current(&runtime.pipeline_id, &runtime.registration)
        .await;
}

async fn run_external_file_ingest(
    runtime: Arc<ExternalFileIngestRuntime>,
    mut spawned: SpawnedExternalFileIngest,
    mut cleanup: CoordinatorCleanup,
) {
    let cancel = runtime.registration.cancel_token.clone();
    let mut timestamps = FileIngestTimestamps::default();

    loop {
        cleanup.child_pid = spawned.child.id();
        let child_pid = cleanup.child_pid;
        {
            let sessions = runtime.engine.ingests.sessions.read().await;
            if sessions
                .get(&runtime.registration.input_id)
                .is_none_or(|session| session.attempt_id != runtime.registration.attempt_id)
            {
                break;
            }
            runtime
                .engine
                .file_ingests
                .children
                .write()
                .await
                .insert(runtime.ingest_id.clone(), spawned.child);
        }

        let stdout = async {
            let result = transport::pump_stdout(runtime.clone(), spawned.stdout, timestamps).await;
            // Reap before joining stderr: a failed reader or cancelled media
            // task can otherwise leave FFmpeg blocked on stdout indefinitely.
            if result.as_ref().map_or(true, |(_, res)| res.is_err()) || cancel.is_cancelled() {
                stop_owned_child(&runtime, child_pid).await;
            }
            result
        };
        let stderr = process::capture_stderr(&runtime.ingest_id, spawned.stderr);
        let readers = async { tokio::join!(stdout, stderr) };
        tokio::pin!(readers);
        let (stdout_result, stderr_result) = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                stop_owned_child(&runtime, child_pid).await;
                readers.await
            }
            results = &mut readers => results,
        };
        let stdout_result = match stdout_result {
            Ok((returned_timestamps, result)) => {
                timestamps = returned_timestamps;
                result
            }
            Err(error) => {
                cleanup.fail(error).await;
                return;
            }
        };

        let mut exit_status = None;
        if let Some(mut child) = take_owned_child(&runtime, child_pid).await {
            exit_status = child.wait().await.ok();
        }
        cleanup.child_pid = None;

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

    cleanup.finish().await;
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;

    use tokio::process::{ChildStdout, Command};

    use super::*;
    use crate::media::mpegts::{TsDemuxer, TsMuxer};
    use crate::media::packet::MediaType;
    use crate::media::ring_buffer::PublishWake;

    async fn registered_runtime(name: &str) -> Arc<ExternalFileIngestRuntime> {
        let engine = Arc::new(MediaEngine::new_with_config(Arc::new(
            crate::AppConfig::default(),
        )));
        let ring_buffer = engine.get_or_create_pipeline(name).await;
        let registration = engine
            .try_register_ingest_attempt(name, name, "file")
            .await
            .expect("register file ingest");
        Arc::new(ExternalFileIngestRuntime {
            engine,
            ingest_id: name.to_string(),
            pipeline_id: name.to_string(),
            source: ExternalFileIngestSource {
                file_path: PathBuf::new(),
                start_time: String::new(),
                loop_enabled: true,
                live_optimized: false,
                target_gop_seconds: 4,
            },
            ring_buffer,
            registration,
        })
    }

    fn stdout_pair() -> (ChildStdout, UnixStream) {
        let (reader, writer) = UnixStream::pair().expect("pipe stand-in");
        reader.set_nonblocking(true).expect("nonblocking reader");
        let stdout = ChildStdout::from_std(std::process::ChildStdout::from(OwnedFd::from(reader)))
            .expect("register stdout on CONTROL");
        (stdout, writer)
    }

    fn timestamped_audio_ts() -> Vec<u8> {
        let fixture = crate::test_fixtures::canonical_h264_ts_fixture().expect("TS fixture");
        let data = std::fs::read(fixture).expect("read TS fixture");
        let mut demuxer = TsDemuxer::new();
        demuxer.feed(&data);
        demuxer.flush();
        let probe = demuxer.take_probe().expect("fixture probe");
        let audio = probe.audio_tracks.first().expect("fixture audio").clone();
        let payload = demuxer
            .drain()
            .into_iter()
            .find(|packet| {
                packet.media_type == MediaType::Audio && packet.track_index == audio.track_index
            })
            .expect("fixture audio packet")
            .payload;
        let mut muxer = TsMuxer::new(None, std::slice::from_ref(&audio));
        let mut ts = Vec::new();
        for timestamp in [100, 200] {
            ts.extend_from_slice(muxer.mux_packet(
                MediaType::Audio,
                audio.track_index,
                timestamp,
                timestamp,
                false,
                &payload,
            ));
        }
        ts
    }

    struct PublicationSignal(mpsc::Sender<()>);

    impl PublishWake for PublicationSignal {
        fn wake(&self) {
            let _ = self.0.send(());
        }
    }

    #[tokio::test]
    async fn media_file_pump_publishes_with_control_blocked_and_preserves_retry_timestamps() {
        let runtime = registered_runtime("file-media-progress").await;
        let (published, publication) = mpsc::channel();
        runtime
            .ring_buffer
            .publication_subscribers()
            .subscribe(Arc::new(PublicationSignal(published)));
        let ts = timestamped_audio_ts();
        let mut timestamps = FileIngestTimestamps::default();
        for expected in [[100, 249], [250, 399]] {
            let (stdout, mut writer) = stdout_pair();
            let mut pump =
                std::pin::pin!(transport::pump_stdout(runtime.clone(), stdout, timestamps,));
            // Poll the production API once to launch the MEDIA owner. The
            // current-thread CONTROL runtime cannot poll during recv_timeout.
            assert!(futures_util::poll!(pump.as_mut()).is_pending());
            let start = runtime.ring_buffer.get_write_idx();
            writer.write_all(&ts).expect("write TS");
            publication
                .recv_timeout(Duration::from_secs(3))
                .expect("MEDIA publication while CONTROL is blocked");
            drop(writer);
            let (returned, result) = tokio::time::timeout(Duration::from_secs(3), pump)
                .await
                .expect("pump EOF")
                .expect("media task joined");
            result.expect("pump succeeded");
            timestamps = returned;
            let packets: Vec<_> = (start..runtime.ring_buffer.get_write_idx())
                .map(|index| {
                    runtime
                        .ring_buffer
                        .read_at(index)
                        .expect("published packet")
                })
                .collect();
            assert_eq!(
                packets.iter().map(|packet| packet.dts).collect::<Vec<_>>(),
                expected,
            );
            assert!(packets.iter().all(|packet| packet.pts == packet.dts));
        }
        cleanup_runtime(&runtime, None).await;
    }

    #[tokio::test]
    async fn media_file_pump_cancels_pending_read_with_control_blocked() {
        let runtime = registered_runtime("file-media-cancel").await;
        let (stdout, mut writer) = stdout_pair();
        writer
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("peer read deadline");
        let mut pump = std::pin::pin!(transport::pump_stdout(
            runtime.clone(),
            stdout,
            FileIngestTimestamps::default(),
        ));
        assert!(futures_util::poll!(pump.as_mut()).is_pending());
        runtime.registration.cancel_token.cancel();
        // A closed peer proves the real pipe reader terminated without a
        // CONTROL poll; merely observing a cancelled token would not.
        assert_eq!(writer.read(&mut [0u8; 1]).expect("MEDIA closes stdout"), 0);
        let (_, result) = tokio::time::timeout(Duration::from_secs(3), pump)
            .await
            .expect("cancelled pump returned")
            .expect("media task joined");
        result.expect("cancellation is successful termination");
        cleanup_runtime(&runtime, None).await;
    }

    async fn assert_coordinator_cleanup(abort: bool) {
        let runtime = registered_runtime(if abort {
            "file-control-abort"
        } else {
            "file-control-cancel"
        })
        .await;
        runtime
            .engine
            .mark_file_ingest_running(&runtime.ingest_id)
            .await;
        let mut child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("idle child");
        let pid = child.id().expect("child PID");
        let spawned = SpawnedExternalFileIngest {
            stdout: child.stdout.take().expect("stdout"),
            stderr: child.stderr.take().expect("stderr"),
            child,
        };
        let cleanup = CoordinatorCleanup::new(runtime.clone(), Some(pid));
        let coordinator = tokio::spawn(run_external_file_ingest(runtime.clone(), spawned, cleanup));
        tokio::time::timeout(Duration::from_secs(3), async {
            while !runtime
                .engine
                .file_ingest_dependency_snapshot(&runtime.ingest_id)
                .await
                .child_registered
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("child registered");
        if abort {
            coordinator.abort();
            assert!(
                coordinator
                    .await
                    .expect_err("aborted coordinator")
                    .is_cancelled()
            );
        } else {
            runtime.registration.cancel_token.cancel();
            tokio::time::timeout(Duration::from_secs(3), coordinator)
                .await
                .expect("coordinator cancellation")
                .expect("coordinator joined");
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let snapshot = runtime
                    .engine
                    .file_ingest_dependency_snapshot(&runtime.ingest_id)
                    .await;
                let registered = runtime
                    .engine
                    .with_ingest_session(&runtime.registration, |_| ())
                    .await
                    .is_some();
                if !snapshot.marked_active && !snapshot.child_registered && !registered {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("child and ingest registry cleanup");
        assert!(runtime.registration.cancel_token.is_cancelled());
        // SAFETY: signal zero only queries whether the child still exists.
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[tokio::test]
    async fn control_file_coordinator_cancellation_reaps_child_and_unregisters() {
        assert_coordinator_cleanup(false).await;
    }

    #[tokio::test]
    async fn control_file_coordinator_abort_reaps_child_and_unregisters() {
        assert_coordinator_cleanup(true).await;
    }

    #[tokio::test]
    async fn cleanup_clears_active_marker_after_session_removal() {
        let runtime = registered_runtime("file-removed-session").await;
        runtime
            .engine
            .mark_file_ingest_running(&runtime.ingest_id)
            .await;
        runtime.engine.unregister_ingest(&runtime.pipeline_id).await;
        cleanup_runtime(&runtime, None).await;
        assert!(
            !runtime
                .engine
                .file_ingest_dependency_snapshot(&runtime.ingest_id)
                .await
                .marked_active
        );
    }

    #[tokio::test]
    async fn cleanup_preserves_replacement_session_active_marker() {
        let runtime = registered_runtime("file-replacement-session").await;
        runtime
            .engine
            .mark_file_ingest_running(&runtime.ingest_id)
            .await;
        runtime.engine.unregister_ingest(&runtime.pipeline_id).await;
        let replacement = runtime
            .engine
            .try_register_ingest_attempt(
                &runtime.pipeline_id,
                &runtime.registration.input_id,
                "file",
            )
            .await
            .expect("replacement registration");
        cleanup_runtime(&runtime, None).await;
        assert!(
            runtime
                .engine
                .file_ingest_dependency_snapshot(&runtime.ingest_id)
                .await
                .marked_active
        );
        assert!(
            runtime
                .engine
                .with_ingest_session(&replacement, |_| ())
                .await
                .is_some()
        );
        assert!(!replacement.cancel_token.is_cancelled());
        runtime.engine.unregister_ingest(&runtime.pipeline_id).await;
        runtime
            .engine
            .clear_file_ingest_running(&runtime.ingest_id)
            .await;
    }
}
