use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::application::ingest::{
    FileIngestConfig, PipelineFileIngestState, ResolveFileIngestError,
    clear_stream_key_file_ingests, load_pipeline_file_ingest_state, persist_pipeline_file_ingest,
    remove_pipeline_file_ingest, resolve_file_ingest_context,
};
use crate::application::ports::{IngestLookup, IngestWriter, PipelineStore};
use crate::infrastructure::sqlite_ports::{SqliteIngestLookup, SqlitePipelineStore};
use crate::media::engine::MediaEngine;
use crate::types::{Ingest, Pipeline};

use super::error::{ApiError, ApiResult};
use super::pipeline_service::PipelineService;

pub struct FileIngestConfigInput {
    pub filename: String,
    pub loop_flag: bool,
    pub start_time: String,
    pub live_optimized: bool,
    pub target_gop_seconds: u32,
}

pub struct SpawnedFileIngestChild {
    pub child: Child,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileIngestStartError {
    NotFound,
    MissingPipelineForStreamKey,
    IngestLookup,
    PipelineStore(String),
    AlreadyRunning,
    InvalidMediaPath,
    MediaFileNotFound,
    PipelineAlreadyActive,
    Spawn(String),
}

pub struct FileIngestService {
    ingest_lookup: Arc<dyn IngestLookup>,
    ingest_writer: Arc<dyn IngestWriter>,
    pipeline_store: Arc<dyn PipelineStore>,
    pipeline_service: PipelineService,
}

impl FileIngestService {
    pub fn new(db: SqlitePool, pipeline_service: PipelineService) -> Self {
        let ingest_store = Arc::new(SqliteIngestLookup::new(db.clone()));
        Self {
            ingest_lookup: ingest_store.clone(),
            ingest_writer: ingest_store,
            pipeline_store: Arc::new(SqlitePipelineStore::new(db)),
            pipeline_service,
        }
    }

    pub fn with_ports(
        ingest_lookup: Arc<dyn IngestLookup>,
        ingest_writer: Arc<dyn IngestWriter>,
        pipeline_store: Arc<dyn PipelineStore>,
        pipeline_service: PipelineService,
    ) -> Self {
        Self {
            ingest_lookup,
            ingest_writer,
            pipeline_store,
            pipeline_service,
        }
    }

    pub fn build_file_ingest_args(ingest: &Ingest, file_path: &Path) -> Vec<String> {
        let mut args = vec![
            "-nostdin".into(),
            "-hide_banner".into(),
            "-loglevel".into(),
            "warning".into(),
            "-re".into(),
        ];
        if ingest.loop_flag {
            args.extend(["-stream_loop".into(), "-1".into()]);
        }
        if !ingest.start_time.is_empty() {
            args.extend(["-ss".into(), ingest.start_time.clone()]);
        }
        args.extend(["-i".into(), file_path.to_string_lossy().into_owned()]);
        if ingest.live_optimized {
            let target_gop_seconds = ingest.target_gop_seconds.max(1);
            args.extend([
                "-map".into(),
                "0:v:0".into(),
                "-map".into(),
                "0:a?".into(),
                "-c:v".into(),
                "libx264".into(),
                "-preset".into(),
                "veryfast".into(),
                "-tune".into(),
                "zerolatency".into(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                "-sc_threshold".into(),
                "0".into(),
                "-force_key_frames".into(),
                format!("expr:gte(t,n_forced*{target_gop_seconds})"),
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                "192k".into(),
                "-ar".into(),
                "48000".into(),
            ]);
        } else {
            args.extend(["-map".into(), "0".into(), "-c".into(), "copy".into()]);
        }
        args.extend([
            "-mpegts_flags".into(),
            "resend_headers+pat_pmt_at_frames".into(),
            "-pes_payload_size".into(),
            "0".into(),
            "-omit_video_pes_length".into(),
            "0".into(),
            "-flush_packets".into(),
            "1".into(),
            "-muxdelay".into(),
            "0".into(),
            "-muxpreload".into(),
            "0".into(),
            "-f".into(),
            "mpegts".into(),
            "pipe:1".into(),
        ]);
        args
    }

    pub fn spawn_file_ingest_child(
        ingest: &Ingest,
        file_path: &Path,
    ) -> Result<SpawnedFileIngestChild, String> {
        let ffmpeg_bin = crate::ffmpeg_extract::ensure_ffmpeg_extracted();
        let args = Self::build_file_ingest_args(ingest, file_path);
        let mut child = Command::new(ffmpeg_bin)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn ffmpeg: {e}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture ffmpeg stdout".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "Failed to capture ffmpeg stderr".to_string())?;

        Ok(SpawnedFileIngestChild {
            child,
            stdout,
            stderr,
        })
    }

    pub fn resolve_media_file_path(
        media_dir: &Path,
        filename: &str,
    ) -> Result<PathBuf, FileIngestStartError> {
        if filename.trim().is_empty() {
            return Err(FileIngestStartError::InvalidMediaPath);
        }

        let relative = Path::new(filename);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(FileIngestStartError::InvalidMediaPath);
        }

        let media_root = media_dir
            .canonicalize()
            .map_err(|_| FileIngestStartError::MediaFileNotFound)?;
        let file_path = media_root.join(relative);
        let canonical_file = file_path
            .canonicalize()
            .map_err(|_| FileIngestStartError::MediaFileNotFound)?;

        if !canonical_file.starts_with(&media_root) || !canonical_file.is_file() {
            return Err(FileIngestStartError::InvalidMediaPath);
        }

        Ok(canonical_file)
    }

    pub async fn get_pipeline(&self, id: &str) -> ApiResult<Pipeline> {
        self.pipeline_service.get_by_id(id).await
    }

    pub async fn load_pipeline_file_ingest_state(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline: &Pipeline,
    ) -> ApiResult<PipelineFileIngestState> {
        load_pipeline_file_ingest_state(self.ingest_lookup.as_ref(), engine, pipeline)
            .await
            .map_err(|_| ApiError::internal("load pipeline file ingest state"))
    }

    pub async fn delete_ingest_with_runtime_cleanup(
        &self,
        engine: &Arc<MediaEngine>,
        id: &str,
    ) -> ApiResult<()> {
        let ingest = self
            .ingest_lookup
            .get_ingest(id)
            .await
            .map_err(|err| ApiError::internal(format!("get ingest: {err}")))?
            .ok_or_else(|| ApiError::not_found("Ingest not found"))?;

        clear_stream_key_file_ingests(
            self.pipeline_store.as_ref(),
            self.ingest_lookup.as_ref(),
            engine,
            &ingest.stream_key,
        )
        .await
        .map_err(|err| ApiError::internal(format!("clear file ingest state: {err:?}")))?;

        let _ = engine.stop_file_ingest_child(id).await;
        engine.clear_file_ingest_running(id).await;
        let deleted = self
            .ingest_writer
            .delete_ingest(id)
            .await
            .map_err(|err| ApiError::internal(format!("delete ingest: {err}")))?;
        if deleted {
            Ok(())
        } else {
            Err(ApiError::not_found("Ingest not found"))
        }
    }

    pub async fn stop_ingest_with_runtime_cleanup(
        &self,
        engine: &Arc<MediaEngine>,
        id: &str,
    ) -> ApiResult<Ingest> {
        let ingest = self
            .ingest_lookup
            .get_ingest(id)
            .await
            .map_err(|err| ApiError::internal(format!("get ingest: {err}")))?
            .ok_or_else(|| ApiError::not_found("Ingest not found"))?;

        clear_stream_key_file_ingests(
            self.pipeline_store.as_ref(),
            self.ingest_lookup.as_ref(),
            engine,
            &ingest.stream_key,
        )
        .await
        .map_err(|err| ApiError::internal(format!("clear file ingest state: {err:?}")))?;

        let _ = engine.stop_file_ingest_child(id).await;
        engine.clear_file_ingest_running(id).await;

        Ok(ingest)
    }

    pub async fn start_ingest(
        &self,
        engine: Arc<MediaEngine>,
        media_dir: &Path,
        id: &str,
    ) -> Result<Ingest, FileIngestStartError> {
        let resolved = match resolve_file_ingest_context(
            self.ingest_lookup.as_ref(),
            self.pipeline_store.as_ref(),
            id,
        )
        .await
        {
            Ok(Some(context)) => context,
            Ok(None) => return Err(FileIngestStartError::NotFound),
            Err(ResolveFileIngestError::MissingPipelineForStreamKey(_)) => {
                return Err(FileIngestStartError::MissingPipelineForStreamKey);
            }
            Err(ResolveFileIngestError::IngestLookup(_)) => {
                return Err(FileIngestStartError::IngestLookup);
            }
            Err(ResolveFileIngestError::PipelineStore(err)) => {
                return Err(FileIngestStartError::PipelineStore(err.to_string()));
            }
        };
        let ingest = resolved.ingest;
        let pipeline = resolved.pipeline;

        if engine.is_file_ingest_running(id).await {
            return Err(FileIngestStartError::AlreadyRunning);
        }

        let file_path = Self::resolve_media_file_path(media_dir, &ingest.filename)?;

        let ring_buffer = engine.get_or_create_pipeline(&pipeline.id).await;
        let Some(registration) = engine
            .try_register_ingest_attempt(&pipeline.id, &ingest.stream_key, "file")
            .await
        else {
            return Err(FileIngestStartError::PipelineAlreadyActive);
        };

        engine.mark_file_ingest_running(&ingest.id).await;

        if crate::media::file_ingest::use_internal_file_ingest(&engine.config)
            && !ingest.live_optimized
        {
            if let Err(err) = crate::media::file_ingest::spawn_internal_file_ingest(
                engine.clone(),
                tokio::runtime::Handle::current(),
                ingest.id.clone(),
                pipeline.id.clone(),
                file_path,
                ingest.start_time.clone(),
                ingest.loop_flag,
                ring_buffer,
                registration,
            ) {
                engine.clear_file_ingest_running(&ingest.id).await;
                engine.unregister_ingest(&pipeline.id).await;
                return Err(FileIngestStartError::Spawn(err));
            }
        } else {
            let spawned = match Self::spawn_file_ingest_child(&ingest, &file_path) {
                Ok(child) => child,
                Err(err) => {
                    engine.clear_file_ingest_running(&ingest.id).await;
                    engine.unregister_ingest(&pipeline.id).await;
                    return Err(FileIngestStartError::Spawn(err));
                }
            };

            tokio::spawn(Self::run_file_ingest_task(
                engine.clone(),
                ingest.clone(),
                pipeline,
                file_path,
                ring_buffer,
                registration,
                spawned,
            ));
        }

        Ok(ingest)
    }

    async fn pump_file_ingest_stdout(
        engine: Arc<MediaEngine>,
        pipeline: Pipeline,
        ring_buffer: Arc<crate::media::ring_buffer::RingBuffer>,
        mut stdout: ChildStdout,
        cancel: CancellationToken,
        timestamps: &mut crate::media::file_ingest::ContinuousTimestampState,
    ) -> Result<(), String> {
        let (bytes_received, ingest_metrics, cached_keyframe_times) = {
            engine
                .with_active_ingest(&pipeline.id, |ingest| {
                    (
                        ingest.bytes_received.clone(),
                        ingest.metrics.clone(),
                        ingest.keyframe_times.clone(),
                    )
                })
                .await
                .ok_or_else(|| format!("Active ingest missing for pipeline {}", pipeline.id))?
        };

        let mut demuxer = crate::media::mpegts::TsDemuxer::new();
        let mut packets = Vec::with_capacity(crate::media::MEDIA_PRODUCER_BATCH_PACKETS);
        let mut probe_sent = false;
        let mut buf = vec![0u8; 64 * 1024];

        loop {
            let read = tokio::select! {
                _ = cancel.cancelled() => break,
                res = stdout.read(&mut buf) => res,
            }
            .map_err(|e| format!("Failed to read ffmpeg stdout: {e}"))?;

            if read == 0 {
                break;
            }

            demuxer.feed(&buf[..read]);
            if demuxer.drain_into(&mut packets) > 0 {
                for pkt in &mut packets {
                    timestamps.apply(pkt);
                    if pkt.media_type == crate::media::ring_buffer::MediaType::Video
                        && let Some(parameter_sets) =
                            crate::media::codec::annexb_parameter_sets(&pkt.payload)
                    {
                        ring_buffer.set_video_parameter_sets(parameter_sets);
                    }
                    if pkt.media_type == crate::media::ring_buffer::MediaType::Video
                        && pkt.is_keyframe
                    {
                        let mut times = cached_keyframe_times
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        times.push(pkt.pts);
                        if times.len() > 30 {
                            times.remove(0);
                        }
                    }
                }
                ring_buffer.push_drained_batch_capped(&mut packets);
            }

            if !probe_sent && let Some(probe) = demuxer.take_probe() {
                probe_sent = true;
                let first_audio = probe.audio_tracks.first().cloned();
                let video_sequence_header = probe.video_sequence_header.clone();
                let selected_video_track_index = probe.video.as_ref().map(|_| 0);
                engine
                    .update_ingest_meta(&pipeline.id, probe.video, first_audio, None)
                    .await;
                if let Some(sequence_header) = video_sequence_header {
                    engine
                        .cache_sequence_header(&pipeline.id, true, sequence_header)
                        .await;
                }
                engine
                    .update_ingest_video_track_selection(
                        &pipeline.id,
                        probe.video_track_count,
                        selected_video_track_index,
                    )
                    .await;
                if !probe.audio_tracks.is_empty() {
                    engine
                        .update_ingest_audio_tracks(&pipeline.id, probe.audio_tracks)
                        .await;
                }
            }

            bytes_received.fetch_add(read as u64, std::sync::atomic::Ordering::Relaxed);
            ingest_metrics.record_in(read as u64);
        }

        Ok(())
    }

    async fn log_file_ingest_stderr(
        ingest_id: &str,
        mut stderr: ChildStderr,
    ) -> Result<(), std::io::Error> {
        const STDERR_CAP: usize = 64 * 1024;
        let mut buf = [0u8; 4096];
        let mut all = Vec::new();
        let mut truncated = false;

        loop {
            match stderr.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let remaining = STDERR_CAP.saturating_sub(all.len());
                    if remaining > 0 {
                        all.extend_from_slice(&buf[..n.min(remaining)]);
                    } else if !truncated {
                        truncated = true;
                        warn!(ingest_id = %ingest_id, cap = STDERR_CAP, "ffmpeg stderr truncated");
                    }
                }
                Err(e) => return Err(e),
            }
        }

        if !all.is_empty() {
            warn!(ingest_id = %ingest_id, stderr = %String::from_utf8_lossy(&all).trim(), "ffmpeg stderr");
        }

        Ok(())
    }

    async fn run_file_ingest_task(
        engine: Arc<MediaEngine>,
        ingest: Ingest,
        pipeline: Pipeline,
        file_path: PathBuf,
        ring_buffer: Arc<crate::media::ring_buffer::RingBuffer>,
        registration: crate::media::engine::IngestRegistration,
        mut spawned: SpawnedFileIngestChild,
    ) {
        let cancel = registration.cancel_token.clone();
        let mut timestamps = crate::media::file_ingest::ContinuousTimestampState::default();
        loop {
            engine
                .file_ingests
                .children
                .write()
                .await
                .insert(ingest.id.clone(), spawned.child);

            let stderr_id = ingest.id.clone();
            let stdout_fut = Self::pump_file_ingest_stdout(
                engine.clone(),
                pipeline.clone(),
                ring_buffer.clone(),
                spawned.stdout,
                cancel.clone(),
                &mut timestamps,
            );
            let stderr_fut = Self::log_file_ingest_stderr(&stderr_id, spawned.stderr);
            let (stdout_res, stderr_res) = tokio::join!(stdout_fut, stderr_fut);

            let mut exit_status = None;
            if let Some(mut child) = engine.take_file_ingest_child(&ingest.id).await {
                exit_status = child.wait().await.ok();
            }

            if let Err(err) = stdout_res
                && !cancel.is_cancelled()
            {
                error!(ingest_id = %ingest.id, err = %err, "file-ingest stdout reader failed");
                engine
                    .record_ingest_disconnect_if_current(
                        &pipeline.id,
                        &registration,
                        Some("stdout"),
                        Some(err.to_string()),
                        true,
                    )
                    .await;
            }
            if let Err(err) = stderr_res
                && !cancel.is_cancelled()
            {
                error!(ingest_id = %ingest.id, err = %err, "file-ingest stderr reader failed");
                engine
                    .record_ingest_disconnect_if_current(
                        &pipeline.id,
                        &registration,
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
                warn!(ingest_id = %ingest.id, status = %status, "ffmpeg exited unsuccessfully");
                engine
                    .record_ingest_disconnect_if_current(
                        &pipeline.id,
                        &registration,
                        Some("exit"),
                        Some(format!("ffmpeg exited with status {status}")),
                        true,
                    )
                    .await;
            } else if exit_status.is_some() && !cancel.is_cancelled() && !ingest.loop_flag {
                engine
                    .record_ingest_disconnect_if_current(
                        &pipeline.id,
                        &registration,
                        Some("eof"),
                        Some("file ingest reached end of input".to_string()),
                        false,
                    )
                    .await;
            }

            if cancel.is_cancelled() || !ingest.loop_flag {
                break;
            }

            match Self::spawn_file_ingest_child(&ingest, &file_path) {
                Ok(next) => spawned = next,
                Err(err) => {
                    error!(ingest_id = %ingest.id, err = %err, "file-ingest restart failed");
                    break;
                }
            }
        }

        engine.clear_file_ingest_running(&ingest.id).await;
        engine
            .unregister_ingest_if_current(&pipeline.id, &registration)
            .await;
    }

    pub async fn apply_file_ingest_payload(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline: &Pipeline,
        previous_stream_key: Option<&str>,
        payload: Option<Option<FileIngestConfigInput>>,
    ) -> ApiResult<PipelineFileIngestState> {
        if let Some(previous_stream_key) =
            previous_stream_key.filter(|previous| *previous != pipeline.stream_key.as_str())
        {
            clear_stream_key_file_ingests(
                self.pipeline_store.as_ref(),
                self.ingest_lookup.as_ref(),
                engine,
                previous_stream_key,
            )
            .await
            .map_err(|_| ApiError::internal("clear stream key file ingests (previous)"))?;
        }

        if let Some(payload) = payload {
            clear_stream_key_file_ingests(
                self.pipeline_store.as_ref(),
                self.ingest_lookup.as_ref(),
                engine,
                &pipeline.stream_key,
            )
            .await
            .map_err(|_| ApiError::internal("clear stream key file ingests (current)"))?;

            match payload {
                Some(input) => {
                    persist_pipeline_file_ingest(
                        self.ingest_lookup.as_ref(),
                        self.ingest_writer.as_ref(),
                        self.pipeline_store.as_ref(),
                        pipeline,
                        &FileIngestConfig {
                            filename: input.filename,
                            loop_flag: input.loop_flag,
                            start_time: input.start_time,
                            live_optimized: input.live_optimized,
                            target_gop_seconds: input.target_gop_seconds,
                        },
                        || {
                            let bytes: [u8; 8] = rand::random();
                            format!(
                                "ingest_{}",
                                bytes
                                    .iter()
                                    .map(|b| format!("{:02x}", b))
                                    .collect::<String>()
                            )
                        },
                    )
                    .await
                    .map_err(|_| ApiError::internal("persist pipeline file ingest"))?;
                }
                None => {
                    remove_pipeline_file_ingest(
                        self.ingest_lookup.as_ref(),
                        self.ingest_writer.as_ref(),
                        self.pipeline_store.as_ref(),
                        pipeline,
                    )
                    .await
                    .map_err(|_| ApiError::internal("remove pipeline file ingest"))?;
                }
            }
        }

        self.load_pipeline_file_ingest_state(engine, pipeline).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::{
        IngestCatalogFuture, IngestDeleteFuture, IngestLookupError, IngestLookupFuture,
        IngestUpdateFuture, IngestWriteError, IngestWriteFuture, PipelineCreateFuture,
        PipelineDeleteFuture, PipelineIngestHostFuture, PipelineListFuture, PipelineLookupFuture,
        PipelineStoreError, PipelineUpdateFuture,
    };
    use crate::application::services::PipelineService;

    fn ingest_with(live_optimized: bool) -> Ingest {
        Ingest {
            id: "ing-1".to_string(),
            filename: "clip.mp4".to_string(),
            stream_key: "stream-key".to_string(),
            loop_flag: true,
            start_time: "00:00:05".to_string(),
            live_optimized,
            target_gop_seconds: 4,
        }
    }

    fn has_arg_pair(args: &[String], first: &str, second: &str) -> bool {
        args.windows(2)
            .any(|window| window[0] == first && window[1] == second)
    }

    fn service(pool: SqlitePool) -> FileIngestService {
        FileIngestService::new(pool.clone(), PipelineService::new(pool))
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "restream-file-ingest-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn setup_ingest(pool: &SqlitePool, ingest_id: &str) {
        crate::db::setup_database_schema(pool).await.unwrap();
        crate::db::create_pipeline(pool, "pipe-1", "Pipeline", "stream-key", None, None)
            .await
            .unwrap();
        crate::db::create_ingest(
            pool,
            ingest_id,
            "clip.mp4",
            "stream-key",
            false,
            "",
            false,
            crate::types::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
        )
        .await
        .unwrap();
    }

    struct PersistFailingLookup {
        ingest: Ingest,
    }

    impl IngestLookup for PersistFailingLookup {
        fn get_ingest<'a>(&'a self, _id: &'a str) -> IngestLookupFuture<'a> {
            Box::pin(async move { Ok(Some(self.ingest.clone())) })
        }

        fn get_ingest_by_stream_key<'a>(&'a self, _stream_key: &'a str) -> IngestLookupFuture<'a> {
            Box::pin(async move { Err(IngestLookupError::new("lookup failed during persist")) })
        }

        fn list_ingests<'a>(&'a self) -> IngestCatalogFuture<'a> {
            Box::pin(async move { Ok(vec![self.ingest.clone()]) })
        }

        fn list_ingests_for_filename<'a>(&'a self, _filename: &'a str) -> IngestCatalogFuture<'a> {
            Box::pin(async move { Ok(vec![self.ingest.clone()]) })
        }

        fn list_ingests_for_stream_key<'a>(
            &'a self,
            _stream_key: &'a str,
        ) -> IngestCatalogFuture<'a> {
            Box::pin(async move { Ok(vec![self.ingest.clone()]) })
        }
    }

    struct NoopIngestWriter;

    impl IngestWriter for NoopIngestWriter {
        fn create_ingest<'a>(
            &'a self,
            _id: &'a str,
            _filename: &'a str,
            _stream_key: &'a str,
            _loop_flag: bool,
            _start_time: &'a str,
            _live_optimized: bool,
            _target_gop_seconds: u32,
        ) -> IngestWriteFuture<'a> {
            Box::pin(async move { Err(IngestWriteError::new("unexpected create")) })
        }

        fn update_ingest<'a>(
            &'a self,
            _id: &'a str,
            _filename: &'a str,
            _stream_key: &'a str,
            _loop_flag: bool,
            _start_time: &'a str,
            _live_optimized: bool,
            _target_gop_seconds: u32,
        ) -> IngestUpdateFuture<'a> {
            Box::pin(async move { Err(IngestWriteError::new("unexpected update")) })
        }

        fn delete_ingest<'a>(&'a self, _id: &'a str) -> IngestDeleteFuture<'a> {
            Box::pin(async move { Ok(false) })
        }
    }

    struct StaticPipelineStore {
        pipeline: Pipeline,
    }

    impl PipelineStore for StaticPipelineStore {
        fn get_pipeline<'a>(&'a self, id: &'a str) -> PipelineLookupFuture<'a> {
            Box::pin(async move { Ok((self.pipeline.id == id).then(|| self.pipeline.clone())) })
        }

        fn get_pipeline_by_stream_key<'a>(
            &'a self,
            stream_key: &'a str,
        ) -> PipelineLookupFuture<'a> {
            Box::pin(async move {
                Ok((self.pipeline.stream_key == stream_key).then(|| self.pipeline.clone()))
            })
        }

        fn list_pipelines<'a>(&'a self) -> PipelineListFuture<'a> {
            Box::pin(async move { Ok(vec![self.pipeline.clone()]) })
        }

        fn create_pipeline<'a>(
            &'a self,
            _id: &'a str,
            _name: &'a str,
            _stream_key: &'a str,
            _input_source: Option<&'a str>,
            _srt_ingest_policy: Option<&'a str>,
        ) -> PipelineCreateFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
        }

        fn update_pipeline<'a>(
            &'a self,
            _id: &'a str,
            _name: &'a str,
            _stream_key: &'a str,
            _input_source: Option<&'a str>,
            _srt_ingest_policy: Option<&'a str>,
        ) -> PipelineUpdateFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
        }

        fn delete_pipeline<'a>(&'a self, _id: &'a str) -> PipelineDeleteFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
        }

        fn get_ingest_host<'a>(&'a self) -> PipelineIngestHostFuture<'a> {
            Box::pin(async move { Ok(None) })
        }

        fn update_pipeline_input_source<'a>(
            &'a self,
            pipeline: &'a Pipeline,
            input_source: Option<&'a str>,
        ) -> PipelineUpdateFuture<'a> {
            Box::pin(async move {
                let mut updated = pipeline.clone();
                updated.input_source = input_source.map(ToOwned::to_owned);
                Ok(Some(updated))
            })
        }
    }

    #[test]
    fn build_file_ingest_args_uses_copy_path_by_default() {
        let args = FileIngestService::build_file_ingest_args(
            &ingest_with(false),
            Path::new("/media/clip.mp4"),
        );

        assert!(has_arg_pair(&args, "-stream_loop", "-1"));
        assert!(has_arg_pair(&args, "-ss", "00:00:05"));
        assert!(has_arg_pair(&args, "-c", "copy"));
        assert!(has_arg_pair(&args, "-f", "mpegts"));
        assert_eq!(args.last().map(String::as_str), Some("pipe:1"));
    }

    #[test]
    fn build_file_ingest_args_transcodes_live_optimized_inputs() {
        let args = FileIngestService::build_file_ingest_args(
            &ingest_with(true),
            Path::new("/media/live.mp4"),
        );

        assert!(has_arg_pair(&args, "-c:v", "libx264"));
        assert!(has_arg_pair(&args, "-c:a", "aac"));
        assert!(has_arg_pair(
            &args,
            "-force_key_frames",
            "expr:gte(t,n_forced*4)"
        ));
        assert!(!has_arg_pair(&args, "-c", "copy"));
    }

    #[test]
    fn build_file_ingest_args_clamps_live_gop_seconds() {
        let mut ingest = ingest_with(true);
        ingest.target_gop_seconds = 0;

        let args = FileIngestService::build_file_ingest_args(&ingest, Path::new("/media/live.mp4"));

        assert!(has_arg_pair(
            &args,
            "-force_key_frames",
            "expr:gte(t,n_forced*1)"
        ));
    }

    #[tokio::test]
    async fn apply_file_ingest_payload_surfaces_persist_failure() {
        let pipeline = Pipeline {
            id: "pipe-1".to_string(),
            name: "Pipeline".to_string(),
            stream_key: "stream-key".to_string(),
            input_source: None,
            srt_ingest_policy: None,
        };
        let ingest = ingest_with(false);
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        let service = FileIngestService::with_ports(
            Arc::new(PersistFailingLookup { ingest }),
            Arc::new(NoopIngestWriter),
            Arc::new(StaticPipelineStore {
                pipeline: pipeline.clone(),
            }),
            PipelineService::new(pool),
        );
        let engine = Arc::new(MediaEngine::new());

        let err = service
            .apply_file_ingest_payload(
                &engine,
                &pipeline,
                None,
                Some(Some(FileIngestConfigInput {
                    filename: "replacement.mp4".to_string(),
                    loop_flag: false,
                    start_time: String::new(),
                    live_optimized: false,
                    target_gop_seconds: 2,
                })),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            ApiError::Internal(message) if message == "persist pipeline file ingest"
        ));
    }

    #[test]
    fn resolve_media_file_path_accepts_existing_relative_file() {
        let media_dir = temp_dir("resolve-ok");
        let file = media_dir.join("clip.mp4");
        std::fs::write(&file, b"clip").unwrap();

        let resolved = FileIngestService::resolve_media_file_path(&media_dir, "clip.mp4").unwrap();

        assert_eq!(resolved, file.canonicalize().unwrap());
        let _ = std::fs::remove_dir_all(media_dir);
    }

    #[test]
    fn resolve_media_file_path_rejects_parent_traversal() {
        let media_dir = temp_dir("resolve-parent");
        let outside_dir = temp_dir("resolve-parent-outside");
        let outside = outside_dir.join("clip.mp4");
        std::fs::write(&outside, b"clip").unwrap();

        let err = FileIngestService::resolve_media_file_path(
            &media_dir,
            "../resolve-parent-outside/clip.mp4",
        )
        .unwrap_err();

        assert_eq!(err, FileIngestStartError::InvalidMediaPath);
        let _ = std::fs::remove_dir_all(media_dir);
        let _ = std::fs::remove_dir_all(outside_dir);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_media_file_path_rejects_symlink_escape() {
        let media_dir = temp_dir("resolve-symlink");
        let outside_dir = temp_dir("resolve-symlink-outside");
        let outside = outside_dir.join("clip.mp4");
        std::fs::write(&outside, b"clip").unwrap();
        std::os::unix::fs::symlink(&outside, media_dir.join("linked.mp4")).unwrap();

        let err = FileIngestService::resolve_media_file_path(&media_dir, "linked.mp4").unwrap_err();

        assert_eq!(err, FileIngestStartError::InvalidMediaPath);
        let _ = std::fs::remove_dir_all(media_dir);
        let _ = std::fs::remove_dir_all(outside_dir);
    }

    #[tokio::test]
    async fn stop_ingest_with_runtime_cleanup_clears_running_state() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        setup_ingest(&pool, "ing-stop").await;
        let engine = Arc::new(MediaEngine::new());
        engine.mark_file_ingest_running("ing-stop").await;

        let ingest = service(pool)
            .stop_ingest_with_runtime_cleanup(&engine, "ing-stop")
            .await
            .unwrap();

        assert_eq!(ingest.id, "ing-stop");
        assert!(!engine.is_file_ingest_running("ing-stop").await);
    }

    #[tokio::test]
    async fn delete_ingest_with_runtime_cleanup_deletes_and_clears_running_state() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        setup_ingest(&pool, "ing-delete").await;
        let engine = Arc::new(MediaEngine::new());
        engine.mark_file_ingest_running("ing-delete").await;
        let service = service(pool.clone());

        service
            .delete_ingest_with_runtime_cleanup(&engine, "ing-delete")
            .await
            .unwrap();

        assert!(!engine.is_file_ingest_running("ing-delete").await);
        assert!(
            crate::db::get_ingest(&pool, "ing-delete")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn start_ingest_returns_not_found_for_missing_ingest() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        let engine = Arc::new(MediaEngine::new());

        let err = service(pool)
            .start_ingest(engine, std::env::temp_dir().as_path(), "missing")
            .await
            .unwrap_err();

        assert_eq!(err, FileIngestStartError::NotFound);
    }

    #[tokio::test]
    async fn start_ingest_requires_pipeline_for_stream_key() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        crate::db::create_ingest(
            &pool,
            "ing-orphan",
            "clip.mp4",
            "missing-stream-key",
            false,
            "",
            false,
            crate::types::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS,
        )
        .await
        .unwrap();
        let engine = Arc::new(MediaEngine::new());

        let err = service(pool)
            .start_ingest(engine, std::env::temp_dir().as_path(), "ing-orphan")
            .await
            .unwrap_err();

        assert_eq!(err, FileIngestStartError::MissingPipelineForStreamKey);
    }

    #[tokio::test]
    async fn start_ingest_rejects_already_running_ingest_before_file_check() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        setup_ingest(&pool, "ing-running").await;
        let engine = Arc::new(MediaEngine::new());
        engine.mark_file_ingest_running("ing-running").await;

        let err = service(pool)
            .start_ingest(engine, std::env::temp_dir().as_path(), "ing-running")
            .await
            .unwrap_err();

        assert_eq!(err, FileIngestStartError::AlreadyRunning);
    }

    #[tokio::test]
    async fn start_ingest_rejects_missing_media_file() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        setup_ingest(&pool, "ing-missing-media").await;
        let engine = Arc::new(MediaEngine::new());

        let err = service(pool)
            .start_ingest(engine, std::env::temp_dir().as_path(), "ing-missing-media")
            .await
            .unwrap_err();

        assert_eq!(err, FileIngestStartError::MediaFileNotFound);
    }
}
