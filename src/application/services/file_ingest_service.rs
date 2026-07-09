use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::process::{Child, ChildStderr, ChildStdout, Command};

use crate::application::ingest::{
    FileIngestConfig, PipelineFileIngestState, clear_stream_key_file_ingests,
    load_pipeline_file_ingest_state, persist_pipeline_file_ingest, remove_pipeline_file_ingest,
};
use crate::application::ports::{
    IngestLookup, IngestWriter, SqliteIngestLookup, SqlitePipelineStore,
};
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

pub struct FileIngestService {
    db: SqlitePool,
    pipeline_service: PipelineService,
}

impl FileIngestService {
    pub fn new(db: SqlitePool, pipeline_service: PipelineService) -> Self {
        Self {
            db,
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

    pub async fn get_pipeline(&self, id: &str) -> ApiResult<Pipeline> {
        self.pipeline_service.get_by_id(id).await
    }

    pub async fn delete_ingest_with_runtime_cleanup(&self, engine: &Arc<MediaEngine>, id: &str) {
        let ingest_store = SqliteIngestLookup::new(self.db.clone());
        let pipeline_store = SqlitePipelineStore::new(self.db.clone());

        if let Ok(Some(ingest)) = ingest_store.get_ingest(id).await {
            let _ = clear_stream_key_file_ingests(
                &pipeline_store,
                &ingest_store,
                engine,
                &ingest.stream_key,
            )
            .await;
        }
        let _ = engine.stop_file_ingest_child(id).await;
        engine.clear_file_ingest_running(id).await;
        let _ = ingest_store.delete_ingest(id).await;
    }

    pub async fn stop_ingest_with_runtime_cleanup(
        &self,
        engine: &Arc<MediaEngine>,
        id: &str,
    ) -> ApiResult<Ingest> {
        let ingest_store = SqliteIngestLookup::new(self.db.clone());
        let pipeline_store = SqlitePipelineStore::new(self.db.clone());
        let ingest = ingest_store
            .get_ingest(id)
            .await
            .map_err(|err| ApiError::internal(format!("get ingest: {err}")))?
            .ok_or_else(|| ApiError::not_found("Ingest not found"))?;

        clear_stream_key_file_ingests(&pipeline_store, &ingest_store, engine, &ingest.stream_key)
            .await
            .map_err(|err| ApiError::internal(format!("clear file ingest state: {err:?}")))?;

        let _ = engine.stop_file_ingest_child(id).await;
        engine.clear_file_ingest_running(id).await;

        Ok(ingest)
    }

    pub async fn apply_file_ingest_payload(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline: &Pipeline,
        previous_stream_key: Option<&str>,
        payload: Option<Option<FileIngestConfigInput>>,
    ) -> ApiResult<PipelineFileIngestState> {
        let ingest_store = SqliteIngestLookup::new(self.db.clone());
        let pipeline_store = SqlitePipelineStore::new(self.db.clone());

        if let Some(previous_stream_key) =
            previous_stream_key.filter(|previous| *previous != pipeline.stream_key.as_str())
        {
            clear_stream_key_file_ingests(
                &pipeline_store,
                &ingest_store,
                engine,
                previous_stream_key,
            )
            .await
            .map_err(|_| ApiError::internal("clear stream key file ingests (previous)"))?;
        }

        if let Some(payload) = payload {
            clear_stream_key_file_ingests(
                &pipeline_store,
                &ingest_store,
                engine,
                &pipeline.stream_key,
            )
            .await
            .map_err(|_| ApiError::internal("clear stream key file ingests (current)"))?;

            match payload {
                Some(input) => {
                    let _ = persist_pipeline_file_ingest(
                        &ingest_store,
                        &ingest_store,
                        &pipeline_store,
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
                    .await;
                }
                None => {
                    remove_pipeline_file_ingest(
                        &ingest_store,
                        &ingest_store,
                        &pipeline_store,
                        pipeline,
                    )
                    .await
                    .map_err(|_| ApiError::internal("remove pipeline file ingest"))?;
                }
            }
        }

        load_pipeline_file_ingest_state(&ingest_store, engine, pipeline)
            .await
            .map_err(|_| ApiError::internal("load pipeline file ingest state"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            .await;

        assert!(!engine.is_file_ingest_running("ing-delete").await);
        assert!(
            crate::db::get_ingest(&pool, "ing-delete")
                .await
                .unwrap()
                .is_none()
        );
    }
}
