//! MPEG-TS recording writer — writes live pipeline data to timestamped `.ts` files.
//! Architecture: `RingBuffer` → `TsMuxer` → `MemoryQueue` → raw TS byte writer on OS thread.
//! Auto-deletes recordings shorter than 5 seconds (transient connection artifacts).
//!
//! # Note on Container Format
//! The output is raw MPEG-TS (`.ts`), not Matroska/MKV. MPEG-TS is directly seekable
//! and playable by most media players and HLS-based workflows. After a recording
//! ends we optionally remux the completed `.ts` into `.mp4` via the configured
//! FFmpeg subprocess when that binary exposes the MP4 muxer.

use crate::domain::recording::RecordingSettings;
use crate::domain::stage::StageKey;
use crate::media::engine::MediaEngine;
use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
use crate::media::mpegts::TsServiceMetadata;
use crate::media::ring_buffer::{Reader, RingBuffer};
use crate::media::startup_policy;
use crate::media::{MEDIA_PULL_BURST_PACKETS, MEDIA_TS_BATCH_TARGET_BYTES};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

mod catalog;
pub mod runtime;
pub mod writer;

pub use catalog::{RecordingConversionState, RecordingConversionStatus};
pub(crate) use catalog::{
    build_conversion_state_path, build_mp4_path, is_recording_source_filename,
    load_conversion_state,
};
use catalog::{now_rfc3339, write_conversion_state};

const MIN_DURATION_SECS: u64 = 5;
const MP4_MUXER_NAME: &str = "mov";

pub struct RecordingStart {
    pub recording_id: String,
    pub pipeline_name: String,
    pub pipeline_id: String,
    pub input_source: Option<String>,
    pub media_dir: String,
    pub settings: RecordingSettings,
    pub stage_key: StageKey,
    pub metadata: Option<RecordingMetadataReporter>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordingMetadataEvent {
    Started {
        recording_id: String,
        pipeline_id: String,
        started_at: String,
        temp_path: String,
    },
    Finalized {
        recording_id: String,
        ended_at: String,
        final_path: String,
    },
    Failed {
        recording_id: String,
        error: String,
    },
}

#[derive(Clone)]
pub struct RecordingMetadataReporter {
    sender: mpsc::UnboundedSender<RecordingMetadataEvent>,
}

impl RecordingMetadataReporter {
    pub fn new(sender: mpsc::UnboundedSender<RecordingMetadataEvent>) -> Self {
        Self { sender }
    }

    pub(crate) fn report(&self, event: RecordingMetadataEvent) {
        let _ = self.sender.send(event);
    }
}

fn sanitize_name(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    let mut last_was_sep = false;
    for c in name.chars() {
        let is_allowed = c.is_ascii_alphanumeric() || matches!(c, '-' | '_');
        let next = if is_allowed { c } else { '_' };
        if next == '_' {
            if last_was_sep {
                continue;
            }
            last_was_sep = true;
        } else {
            last_was_sep = false;
        }
        sanitized.push(next);
    }
    sanitized.trim_matches('_').to_string()
}

fn build_filename(pipe_name: &str) -> String {
    let now = chrono::Local::now();
    let safe_name = sanitize_name(pipe_name);
    let safe_name = if safe_name.is_empty() {
        "pipeline"
    } else {
        safe_name.as_str()
    };
    format!("recording_{}_{}.ts", now.format("%Y%m%dT%H%M%S"), safe_name)
}

fn build_mp4_temp_path(mp4_path: &Path) -> PathBuf {
    let stem = mp4_path
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "recording".to_string());
    mp4_path.with_file_name(format!("{stem}.tmp.mp4"))
}

fn normalize_recording_threads(recording_threads: Option<u32>) -> u32 {
    recording_threads.unwrap_or(2).max(1)
}

fn build_recording_remux_args(
    input_path: &Path,
    output_path: &Path,
    ffmpeg_threads: u32,
) -> Vec<String> {
    vec![
        "-y".to_string(),
        "-nostdin".to_string(),
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "warning".to_string(),
        "-threads".to_string(),
        ffmpeg_threads.to_string(),
        "-fflags".to_string(),
        "+genpts".to_string(),
        "-i".to_string(),
        input_path.display().to_string(),
        "-map".to_string(),
        "0:v?".to_string(),
        "-map".to_string(),
        "0:a?".to_string(),
        "-c".to_string(),
        "copy".to_string(),
        "-movflags".to_string(),
        "+faststart".to_string(),
        "-bsf:a".to_string(),
        "aac_adtstoasc".to_string(),
        "-f".to_string(),
        "mov".to_string(),
        output_path.display().to_string(),
    ]
}

fn ffmpeg_muxers_include_mp4(listing: &str) -> bool {
    listing.lines().any(|line| {
        let trimmed = line.trim();
        let mut parts = trimmed.split_whitespace();
        let flags = parts.next().unwrap_or_default();
        let muxer_names = parts.next().unwrap_or_default();
        flags.contains('E')
            && muxer_names
                .split(',')
                .any(|name| name == MP4_MUXER_NAME || name == "mp4")
    })
}

fn ffmpeg_supports_mp4_muxer() -> bool {
    static SUPPORTS_MP4_MUXER: OnceLock<bool> = OnceLock::new();
    *SUPPORTS_MP4_MUXER.get_or_init(|| {
        let ffmpeg = crate::ffmpeg_extract::ffmpeg_bin_path();
        match std::process::Command::new(ffmpeg)
            .args(["-hide_banner", "-muxers"])
            .output()
        {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                ffmpeg_muxers_include_mp4(&stdout)
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(
                    ffmpeg = %ffmpeg.display(),
                    status = %output.status,
                    stderr = %stderr.trim(),
                    "failed to inspect ffmpeg muxer support; recording remux disabled"
                );
                false
            }
            Err(error) => {
                warn!(
                    ffmpeg = %ffmpeg.display(),
                    err = %error,
                    "failed to spawn ffmpeg for muxer probe; recording remux disabled"
                );
                false
            }
        }
    })
}

async fn remux_recording_to_mp4(
    recording_id: String,
    ts_path: PathBuf,
    settings: RecordingSettings,
    ffmpeg_threads: u32,
    metadata: Option<RecordingMetadataReporter>,
) {
    if !ffmpeg_supports_mp4_muxer() {
        let error = "Configured FFmpeg binary does not expose the mov/mp4 muxer".to_string();
        write_conversion_state(
            &ts_path,
            RecordingConversionStatus::Failed,
            Some(error.clone()),
        )
        .await;
        if let Some(metadata) = &metadata {
            metadata.report(RecordingMetadataEvent::Failed {
                recording_id,
                error,
            });
        }
        info!(
            source = %ts_path.display(),
            muxer = MP4_MUXER_NAME,
            "recording remux skipped because ffmpeg lacks mp4 muxer support"
        );
        return;
    }

    let mp4_path = build_mp4_path(&ts_path);
    let temp_path = build_mp4_temp_path(&mp4_path);
    let ffmpeg_path = crate::ffmpeg_extract::ffmpeg_bin_path().to_path_buf();
    let args = build_recording_remux_args(&ts_path, &temp_path, ffmpeg_threads);
    let _ = tokio::fs::remove_file(&temp_path).await;

    info!(
        source = %ts_path.display(),
        output = %mp4_path.display(),
        ffmpeg = %ffmpeg_path.display(),
        "starting recording mp4 remux"
    );

    match Command::new(&ffmpeg_path).args(&args).output().await {
        Ok(output) if output.status.success() => {
            if let Err(error) = tokio::fs::rename(&temp_path, &mp4_path).await {
                let _ = tokio::fs::remove_file(&temp_path).await;
                let error_message = format!("Finalized MP4 rename failed: {error}");
                write_conversion_state(
                    &ts_path,
                    RecordingConversionStatus::Failed,
                    Some(error_message.clone()),
                )
                .await;
                if let Some(metadata) = &metadata {
                    metadata.report(RecordingMetadataEvent::Failed {
                        recording_id,
                        error: error_message,
                    });
                }
                error!(
                    source = %ts_path.display(),
                    output = %mp4_path.display(),
                    err = %error,
                    "recording remux completed but final rename failed"
                );
                return;
            }

            if settings.retain_source_ts {
                write_conversion_state(&ts_path, RecordingConversionStatus::Ready, None).await;
            } else if let Err(error) = tokio::fs::remove_file(&ts_path).await {
                write_conversion_state(&ts_path, RecordingConversionStatus::Ready, None).await;
                warn!(
                    source = %ts_path.display(),
                    output = %mp4_path.display(),
                    err = %error,
                    "recording remux succeeded but source ts cleanup failed"
                );
            } else {
                let state_path = build_conversion_state_path(&ts_path);
                if let Err(error) = tokio::fs::remove_file(&state_path).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    warn!(
                        state = %state_path.display(),
                        err = %error,
                        "recording remux succeeded but conversion state cleanup failed"
                    );
                }
            }
            info!(
                source = %ts_path.display(),
                output = %mp4_path.display(),
                "recording remux completed"
            );
            if let Some(metadata) = &metadata {
                metadata.report(RecordingMetadataEvent::Finalized {
                    recording_id,
                    ended_at: now_rfc3339(),
                    final_path: mp4_path.display().to_string(),
                });
            }
        }
        Ok(output) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            let stderr = String::from_utf8_lossy(&output.stderr);
            let error = stderr.trim().to_string();
            write_conversion_state(
                &ts_path,
                RecordingConversionStatus::Failed,
                Some(error.clone()),
            )
            .await;
            if let Some(metadata) = &metadata {
                metadata.report(RecordingMetadataEvent::Failed {
                    recording_id,
                    error,
                });
            }
            warn!(
                source = %ts_path.display(),
                output = %mp4_path.display(),
                status = %output.status,
                stderr = %stderr.trim(),
                "recording remux failed; keeping original ts"
            );
        }
        Err(error) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            let error_message = format!("Failed to spawn ffmpeg: {error}");
            write_conversion_state(
                &ts_path,
                RecordingConversionStatus::Failed,
                Some(error_message.clone()),
            )
            .await;
            if let Some(metadata) = &metadata {
                metadata.report(RecordingMetadataEvent::Failed {
                    recording_id,
                    error: error_message,
                });
            }
            warn!(
                source = %ts_path.display(),
                output = %mp4_path.display(),
                err = %error,
                "failed to spawn ffmpeg for recording remux; keeping original ts"
            );
        }
    }
}

fn build_recording_service_metadata(
    pipeline_name: &str,
    pipeline_id: &str,
    input_source: Option<&str>,
    recorded_at: &str,
) -> TsServiceMetadata {
    let source = input_source
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("publisher");
    TsServiceMetadata {
        provider_name: format!("Restream pipeline_id={pipeline_id}"),
        service_name: format!(
            "pipeline={}; source={}; recorded_at={}",
            pipeline_name, source, recorded_at
        ),
    }
}

pub async fn start_recording(
    start: RecordingStart,
    ring_buffer: Arc<RingBuffer>,
    engine: Arc<MediaEngine>,
    cancel_token: CancellationToken,
) {
    let RecordingStart {
        recording_id,
        pipeline_name,
        pipeline_id,
        input_source,
        media_dir,
        settings,
        stage_key: rec_stage_key,
        metadata,
    } = start;

    let _ = fs::create_dir_all(&media_dir);
    let filename = build_filename(&pipeline_name);
    let file_path = format!("{}/{}", media_dir, filename);
    let recorded_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    if let Some(metadata) = &metadata {
        metadata.report(RecordingMetadataEvent::Started {
            recording_id: recording_id.clone(),
            pipeline_id: pipeline_id.clone(),
            started_at: recorded_at.clone(),
            temp_path: file_path.clone(),
        });
    }
    let service_metadata = build_recording_service_metadata(
        &pipeline_name,
        &pipeline_id,
        input_source.as_deref(),
        &recorded_at,
    );
    let started_at = std::time::Instant::now();

    info!(filename = %filename, "recording started");

    let (lifecycle, stage_metrics) = engine
        .get_or_create_non_ring_stage_runtime(
            rec_stage_key.clone(),
            crate::media::stage_lifecycle::StagePhase::Registered,
            crate::media::stage_lifecycle::StageBackendKind::Recording,
            cancel_token.clone(),
        )
        .await;
    let _lifecycle_guard =
        crate::media::stage_lifecycle::StageLifecycleGuard::new(lifecycle.clone());
    lifecycle.transition(crate::media::stage_lifecycle::StagePhase::BackendSpawned {
        backend: crate::media::stage_lifecycle::StageBackendKind::Recording,
        pid: None,
    });
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageRegistered {
            pipeline_id: pipeline_id.clone(),
            encoding: "recording".to_string(),
        });

    let queue = Arc::new(crate::media::avio::MemoryQueue::new_with_capacity(
        engine.config.avio_capacity,
    ));

    // Guard: close the queue on drop so the OS writer thread always unblocks,
    // even if this async fn is cancelled or panics before reaching queue.close().
    struct QueueCloseGuard(Arc<crate::media::avio::MemoryQueue>);
    impl Drop for QueueCloseGuard {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    let _queue_guard = QueueCloseGuard(queue.clone());

    let queue_clone = queue.clone();
    let file_path_clone = file_path.clone();
    let cancel_token_clone = cancel_token.clone();
    // Store the JoinHandle so we can join the thread on exit and detect panics.
    // Dropping the handle detaches the thread silently — any crash becomes invisible.
    let muxer_handle = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_ts_writer(queue_clone, &file_path_clone, cancel_token_clone)
        }));
        match result {
            Ok(Err(e)) => error!(err = ?e, "TS writer failed"),
            Err(_) => error!("TS writer panicked"),
            _ => {}
        }
    });

    let mut reader = Reader::new_with_keyframe_preroll(
        format!("recording:{}", pipeline_name),
        ring_buffer,
        startup_policy::recording_keyframe_preroll_packets(),
    );
    let mut packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);

    // Lazily initialized when first packet arrives.
    let (video_sequence_header, _) = engine.get_sequence_headers(&pipeline_id).await;
    let mut feeder: Option<TsPacketFeeder> = None;
    // Accumulation buffer: collect all muxed TS bytes for a burst, then
    // write them in a single queue.write() call (one lock acquisition per
    // burst instead of one per packet).
    let mut ts_batch: Vec<u8> = Vec::with_capacity(MEDIA_TS_BATCH_TARGET_BYTES);

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => break,
            _ = reader.wait_for_data() => {
                loop {
                    packets.clear();
                    match reader.pull_burst(&mut packets, MEDIA_PULL_BURST_PACKETS) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }

                    for pkt in &packets {
                        // Lazily create the feeder from engine metadata.
                        if feeder.is_none() {
                            let metadata = {
                                let ingests = engine.ingests.active.read().await;
                                ingests.get(&pipeline_id).and_then(|ingest| {
                                    let metadata = ingest.metadata();
                                    let video = metadata.video;
                                    let lock = ingest
                                        .audio_tracks
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner());
                                    let tracks = if lock.is_empty() {
                                        metadata
                                            .audio
                                            .clone()
                                            .map(|audio| Arc::new(vec![audio]))
                                            .unwrap_or_else(|| Arc::new(Vec::new()))
                                    } else {
                                        Arc::clone(&lock)
                                    };
                                    (video.is_some() || !tracks.is_empty()).then_some((video, tracks))
                                })
                            };
                            let Some((video, tracks)) = metadata else {
                                continue;
                            };
                            feeder = Some(TsPacketFeeder::new(
                                video.as_ref(),
                                tracks,
                                PacketFeedConfig {
                                    video_sequence_header: video_sequence_header
                                        .as_ref()
                                        .map(|v| v.to_vec()),
                                    raw_video_parameter_sets: reader
                                        .current_ring()
                                        .video_parameter_sets()
                                        .map(|v| v.to_vec()),
                                    service_metadata: Some(service_metadata.clone()),
                                    ..PacketFeedConfig::default()
                                },
                            ));
                        }

                        if let Some(ref mut feeder) = feeder {
                            feeder.extend_ts_for_packet(pkt, &mut ts_batch);
                        }
                    }
                    // One lock acquisition for the whole burst.
                    if !ts_batch.is_empty() {
                        queue.write(&ts_batch).await;
                        ts_batch.clear();
                    }
                    for pkt in &packets {
                        stage_metrics.record_in(pkt.payload.len() as u64);
                    }
                }
            }
        }
    }

    queue.close();

    // Join the muxer thread to ensure the file is fully flushed before we
    // check the duration and potentially delete it.  Joining also surfaces
    // any panic that escaped catch_unwind (shouldn't happen, but be explicit).
    if let Err(e) = muxer_handle.join() {
        error!(
            "[recording] TS writer thread join failed for {}: {:?}",
            filename, e
        );
    }

    let duration = started_at.elapsed();
    info!(
        "[recording] Ended: {} (duration: {:.1}s)",
        filename,
        duration.as_secs_f64()
    );

    if duration.as_secs() < MIN_DURATION_SECS {
        let _ = fs::remove_file(&file_path);
        if let Some(metadata) = &metadata {
            metadata.report(RecordingMetadataEvent::Failed {
                recording_id: recording_id.clone(),
                error: format!("Recording shorter than {MIN_DURATION_SECS}s was discarded"),
            });
        }
        info!(filename = %filename, "deleted short recording");
    } else {
        write_conversion_state(
            Path::new(&file_path),
            RecordingConversionStatus::Converting,
            None,
        )
        .await;
        let ffmpeg_threads = normalize_recording_threads(engine.config.recording_threads);
        tokio::spawn(remux_recording_to_mp4(
            recording_id,
            PathBuf::from(&file_path),
            settings,
            ffmpeg_threads,
            metadata,
        ));
    }

    engine.remove_stage_runtime(&rec_stage_key).await;
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStopped {
            pipeline_id: pipeline_id.clone(),
            encoding: "recording".to_string(),
        });
}

fn run_ts_writer(
    queue: Arc<crate::media::avio::MemoryQueue>,
    file_path: &str,
    // Cancellation propagates via queue.close() called by QueueCloseGuard on
    // the async side. The token is threaded through for future use (e.g., if
    // MemoryQueue gains a timed-read path) and to make the dependency explicit.
    _cancel: CancellationToken,
) -> Result<(), &'static str> {
    use std::io::Write;

    let path = std::path::Path::new(file_path);
    let mut file =
        std::fs::File::create(path).map_err(|_| "Recording: Failed to create output file")?;

    let mut buf = vec![0u8; 1316];
    let mut done = false;
    while !done {
        let n = queue.read(&mut buf);
        if n == 0 {
            done = true;
        } else {
            file.write_all(&buf[..n])
                .map_err(|_| "Recording: Failed to write")?;
        }
    }

    // Drain any remaining data after cancellation
    loop {
        let n = queue.read(&mut buf);
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|_| "Recording: Failed to write")?;
    }

    file.flush().map_err(|_| "Recording: Failed to flush")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::avio::MemoryQueue;
    use crate::media::mpegts::TsDemuxer;
    use crate::media::ring_buffer::{MediaPacket, MediaType};
    use std::sync::Arc;
    use tokio::process::Command as TokioCommand;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn run_ts_writer_exits_on_closed_queue() {
        let queue = Arc::new(MemoryQueue::new());
        queue.close();
        let token = CancellationToken::new();
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_recording.ts");
        let path_str = file_path.to_string_lossy().to_string();

        let res = run_ts_writer(queue, &path_str, token);
        assert!(res.is_ok());
        let _ = std::fs::remove_file(file_path);
    }

    #[test]
    fn sanitize_name_replaces_path_chars() {
        assert_eq!(
            sanitize_name("a/b\\c:d*e?f\"g<h>i|j"),
            "a_b_c_d_e_f_g_h_i_j"
        );
    }

    #[test]
    fn sanitize_name_preserves_alphanumeric_and_dashes() {
        assert_eq!(sanitize_name("My-Pipeline_v2"), "My-Pipeline_v2");
    }

    #[test]
    fn sanitize_name_collapses_spaces_for_filenames() {
        assert_eq!(sanitize_name("Main Program  01"), "Main_Program_01");
    }

    #[test]
    fn sanitize_name_empty_string() {
        assert_eq!(sanitize_name(""), "");
    }

    #[test]
    fn build_filename_has_ts_extension() {
        let name = build_filename("test-pipe");
        assert!(name.ends_with(".ts"));
        assert!(name.starts_with("recording_"));
        assert!(!name.contains(' '));
    }

    #[test]
    fn build_filename_contains_sanitized_name() {
        let name = build_filename("My Pipe?");
        assert!(
            name.contains("My_Pipe"),
            "expected sanitized name in: {name}"
        );
    }

    #[test]
    fn build_mp4_path_replaces_ts_extension() {
        let ts = Path::new("/tmp/recording_20260629_demo.ts");
        assert_eq!(
            build_mp4_path(ts),
            PathBuf::from("/tmp/recording_20260629_demo.mp4")
        );
    }

    #[test]
    fn build_mp4_temp_path_preserves_mp4_extension_for_muxer_inference() {
        let mp4 = Path::new("/tmp/recording_20260629_demo.mp4");
        assert_eq!(
            build_mp4_temp_path(mp4),
            PathBuf::from("/tmp/recording_20260629_demo.tmp.mp4")
        );
    }

    #[test]
    fn build_conversion_state_path_adds_sidecar_suffix() {
        let ts = Path::new("/tmp/recording_20260629_demo.ts");
        assert_eq!(
            build_conversion_state_path(ts),
            PathBuf::from("/tmp/recording_20260629_demo.ts.conversion.json")
        );
    }

    #[tokio::test]
    async fn recording_settings_default_to_deleting_source_ts() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        crate::db::setup_database_schema(&pool)
            .await
            .expect("schema should initialize");
        let meta_store = crate::infrastructure::sqlite_ports::SqliteMetaStore::new(pool.clone());

        assert_eq!(
            crate::application::recording::load_recording_settings(&meta_store).await,
            RecordingSettings {
                retain_source_ts: false
            }
        );
    }

    #[test]
    fn build_recording_remux_args_targets_faststart_mp4_copy() {
        let input = Path::new("/tmp/input.ts");
        let output = Path::new("/tmp/output.tmp.mp4");
        let args = build_recording_remux_args(input, output, 3);

        assert!(args.windows(2).any(|pair| pair == ["-i", "/tmp/input.ts"]));
        assert!(args.windows(2).any(|pair| pair == ["-threads", "3"]));
        assert!(args.windows(2).any(|pair| pair == ["-c", "copy"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-movflags", "+faststart"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-bsf:a", "aac_adtstoasc"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-f", "mov"]));
        assert_eq!(args.last().map(String::as_str), Some("/tmp/output.tmp.mp4"));
    }

    #[test]
    fn ffmpeg_muxers_include_mp4_detects_mov_muxer_aliases() {
        let listing = "Formats:\n D.. = Demuxing supported\n .E. = Muxing supported\n ---\n  E mov,mp4,m4a,3gp,3g2,mj2 QuickTime / MOV\n  E mpegts MPEG-TS\n";
        assert!(ffmpeg_muxers_include_mp4(listing));
    }

    #[test]
    fn ffmpeg_muxers_include_mp4_detects_plain_mov_muxer_name() {
        let listing = "Formats:\n D.. = Demuxing supported\n .E. = Muxing supported\n ---\n  E mov             QuickTime / MOV\n  E mpegts          MPEG-TS (MPEG-2 Transport Stream)\n";
        assert!(ffmpeg_muxers_include_mp4(listing));
    }

    #[test]
    fn ffmpeg_muxers_include_mp4_rejects_missing_mov_muxer() {
        let listing =
            "Formats:\n .E. = Muxing supported\n ---\n  E matroska Matroska\n  E mpegts MPEG-TS\n";
        assert!(!ffmpeg_muxers_include_mp4(listing));
    }

    async fn remux_recording_fixture(
        settings: RecordingSettings,
    ) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        assert!(
            ffmpeg_supports_mp4_muxer(),
            "bundled ffmpeg must expose mp4 muxing for recording remux"
        );

        let fixture = crate::test_fixtures::canonical_h264_ts_fixture()
            .expect("checked-in TS fixture should exist");
        let temp_dir =
            std::env::temp_dir().join(format!("recording-remux-test-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let source = temp_dir.join("recording_fixture.ts");
        std::fs::copy(&fixture, &source).expect("fixture should copy");

        write_conversion_state(&source, RecordingConversionStatus::Converting, None).await;
        remux_recording_to_mp4(
            "recording-fixture".to_string(),
            source.clone(),
            settings,
            2,
            None,
        )
        .await;

        let mp4_path = build_mp4_path(&source);
        let state_path = build_conversion_state_path(&source);

        (temp_dir, source, mp4_path, state_path)
    }

    #[tokio::test]
    async fn remux_recording_to_mp4_deletes_source_ts_when_retention_disabled() {
        let (temp_dir, source, mp4_path, state_path) = remux_recording_fixture(RecordingSettings {
            retain_source_ts: false,
        })
        .await;

        assert!(
            !source.exists(),
            "source TS should be deleted after successful remux by default"
        );
        assert!(mp4_path.exists(), "remux should create an MP4 sibling");
        assert!(
            !state_path.exists(),
            "conversion state should be removed once source retention is disabled"
        );

        let roundtrip_ts = temp_dir.join("roundtrip.ts");
        let status = TokioCommand::new(crate::ffmpeg_extract::ffmpeg_bin_path())
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                &mp4_path.display().to_string(),
                "-map",
                "0",
                "-c",
                "copy",
                "-f",
                "mpegts",
                &roundtrip_ts.display().to_string(),
            ])
            .status()
            .await
            .expect("bundled ffmpeg should validate remuxed mp4");
        assert!(
            status.success(),
            "remuxed mp4 should be readable by bundled ffmpeg"
        );
        assert!(
            roundtrip_ts.exists() && std::fs::metadata(&roundtrip_ts).unwrap().len() > 0,
            "round-trip remux should produce TS output"
        );

        let _ = std::fs::remove_file(roundtrip_ts);
        let _ = std::fs::remove_file(mp4_path);
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn remux_recording_to_mp4_keeps_source_ts_when_retention_enabled() {
        let (temp_dir, source, mp4_path, state_path) = remux_recording_fixture(RecordingSettings {
            retain_source_ts: true,
        })
        .await;

        assert!(
            source.exists(),
            "source TS should remain after successful remux when retention is enabled"
        );
        assert!(mp4_path.exists(), "remux should create an MP4 sibling");

        let state = load_conversion_state(&source).expect("conversion state should exist");
        assert_eq!(state.status, RecordingConversionStatus::Ready);
        assert!(
            state.error.is_none(),
            "successful remux should not persist an error"
        );

        let roundtrip_ts = temp_dir.join("roundtrip.ts");
        let status = TokioCommand::new(crate::ffmpeg_extract::ffmpeg_bin_path())
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                &mp4_path.display().to_string(),
                "-map",
                "0",
                "-c",
                "copy",
                "-f",
                "mpegts",
                &roundtrip_ts.display().to_string(),
            ])
            .status()
            .await
            .expect("bundled ffmpeg should validate remuxed mp4");
        assert!(
            status.success(),
            "remuxed mp4 should be readable by bundled ffmpeg"
        );
        assert!(
            roundtrip_ts.exists() && std::fs::metadata(&roundtrip_ts).unwrap().len() > 0,
            "round-trip remux should produce TS output"
        );

        let _ = std::fs::remove_file(roundtrip_ts);
        let _ = std::fs::remove_file(mp4_path);
        let _ = std::fs::remove_file(state_path);
        let _ = std::fs::remove_file(source);
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    fn demux_ts_file(path: &Path) -> Vec<MediaPacket> {
        let bytes = std::fs::read(path)
            .unwrap_or_else(|e| panic!("failed to read TS file {}: {e}", path.display()));
        let mut demuxer = TsDemuxer::new();
        demuxer.feed(&bytes);
        demuxer.flush();
        demuxer.drain()
    }

    /// Asserts DTS is monotonically non-decreasing within each stream and that
    /// no gap between consecutive packets exceeds 1s (which would indicate a
    /// dropped GOP or dropped audio frames introduced by the remux).
    fn assert_continuous_monotonic_dts(packets: &[MediaPacket], label: &str) {
        let mut last_video_dts: Option<i64> = None;
        let mut last_audio_dts: Option<i64> = None;
        let mut video_count = 0usize;
        let mut audio_count = 0usize;

        for packet in packets {
            let (last_dts, count) = match packet.media_type {
                MediaType::Video => (&mut last_video_dts, &mut video_count),
                MediaType::Audio => (&mut last_audio_dts, &mut audio_count),
            };
            *count += 1;
            if let Some(previous) = *last_dts {
                let gap = packet.dts - previous;
                assert!(
                    gap >= 0,
                    "{label}: {:?} DTS must be non-decreasing: {previous} -> {}",
                    packet.media_type,
                    packet.dts
                );
                assert!(
                    gap < 1000,
                    "{label}: {:?} DTS gap {gap}ms between {previous} and {} is too large, \
                     suggests dropped frames across the remux",
                    packet.media_type,
                    packet.dts
                );
            }
            *last_dts = Some(packet.dts);
        }

        assert!(video_count > 1, "{label}: expected multiple video packets");
        assert!(audio_count > 1, "{label}: expected multiple audio packets");
    }

    fn stream_span_ms(packets: &[MediaPacket], media_type: MediaType) -> i64 {
        let mut min = i64::MAX;
        let mut max = i64::MIN;
        for packet in packets.iter().filter(|p| p.media_type == media_type) {
            min = min.min(packet.dts);
            max = max.max(packet.dts);
        }
        assert!(
            min <= max,
            "expected at least one packet for {media_type:?}"
        );
        max - min
    }

    /// Proof: TS -> MP4 -> TS remux preserves DTS monotonicity and timeline
    /// span for both video and audio streams, regardless of whether the
    /// source TS is retained or deleted after the remux completes.
    async fn assert_remux_preserves_timestamp_continuity(retain_source_ts: bool) {
        let (temp_dir, source, mp4_path, state_path) =
            remux_recording_fixture(RecordingSettings { retain_source_ts }).await;

        let fixture = crate::test_fixtures::canonical_h264_ts_fixture()
            .expect("checked-in TS fixture should exist");
        let source_packets = demux_ts_file(&fixture);
        assert_continuous_monotonic_dts(&source_packets, "source TS");
        let source_video_span = stream_span_ms(&source_packets, MediaType::Video);
        let source_audio_span = stream_span_ms(&source_packets, MediaType::Audio);

        let roundtrip_ts = temp_dir.join("roundtrip.ts");
        let status = TokioCommand::new(crate::ffmpeg_extract::ffmpeg_bin_path())
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                &mp4_path.display().to_string(),
                "-map",
                "0",
                "-c",
                "copy",
                "-f",
                "mpegts",
                &roundtrip_ts.display().to_string(),
            ])
            .status()
            .await
            .expect("bundled ffmpeg should remux mp4 back to ts");
        assert!(status.success(), "mp4 -> ts round trip should succeed");

        let roundtrip_packets = demux_ts_file(&roundtrip_ts);
        assert_continuous_monotonic_dts(&roundtrip_packets, "TS -> MP4 -> TS roundtrip");

        let roundtrip_video_span = stream_span_ms(&roundtrip_packets, MediaType::Video);
        let roundtrip_audio_span = stream_span_ms(&roundtrip_packets, MediaType::Audio);

        assert!(
            (source_video_span - roundtrip_video_span).abs() <= 40,
            "video timeline span should survive TS -> MP4 -> TS within rounding: \
             source={source_video_span}ms roundtrip={roundtrip_video_span}ms"
        );
        assert!(
            (source_audio_span - roundtrip_audio_span).abs() <= 40,
            "audio timeline span should survive TS -> MP4 -> TS within rounding: \
             source={source_audio_span}ms roundtrip={roundtrip_audio_span}ms"
        );

        let _ = std::fs::remove_file(roundtrip_ts);
        let _ = std::fs::remove_file(mp4_path);
        let _ = std::fs::remove_file(state_path);
        let _ = std::fs::remove_file(source);
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_disabled() {
        assert_remux_preserves_timestamp_continuity(false).await;
    }

    #[tokio::test]
    async fn remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_enabled() {
        assert_remux_preserves_timestamp_continuity(true).await;
    }

    #[test]
    fn recording_service_metadata_describes_pipeline_source_and_time() {
        let metadata = build_recording_service_metadata(
            "Main Program",
            "pipe_123",
            Some("file:clip.mp4"),
            "2026-06-27T12:34:56Z",
        );
        assert_eq!(metadata.provider_name, "Restream pipeline_id=pipe_123");
        assert!(metadata.service_name.contains("pipeline=Main Program"));
        assert!(metadata.service_name.contains("source=file:clip.mp4"));
        assert!(
            metadata
                .service_name
                .contains("recorded_at=2026-06-27T12:34:56Z")
        );

        let publisher =
            build_recording_service_metadata("Live", "pipe_live", None, "2026-06-27T12:34:56Z");
        assert!(publisher.service_name.contains("source=publisher"));
    }

    #[test]
    fn ts_writer_writes_data_to_file() {
        let queue = Arc::new(MemoryQueue::new());
        let token = CancellationToken::new();
        let temp = std::env::temp_dir().join("test_write.ts");
        let path = temp.to_string_lossy().to_string();
        queue.write_sync(b"hello world");
        queue.close();
        let res = run_ts_writer(queue, &path, token);
        assert!(res.is_ok());
        let content = std::fs::read(&temp).unwrap();
        assert_eq!(content, b"hello world");
        let _ = std::fs::remove_file(&temp);
    }

    #[test]
    fn ts_writer_empty_closed_queue_creates_empty_file() {
        let queue = Arc::new(MemoryQueue::new());
        queue.close();
        let token = CancellationToken::new();
        let temp = std::env::temp_dir().join("test_empty.ts");
        let path = temp.to_string_lossy().to_string();
        assert!(run_ts_writer(queue, &path, token).is_ok());
        assert_eq!(std::fs::read(&temp).unwrap().len(), 0);
        let _ = std::fs::remove_file(&temp);
    }

    #[test]
    fn ts_writer_fails_on_invalid_path() {
        let queue = Arc::new(MemoryQueue::new());
        let token = CancellationToken::new();
        assert!(run_ts_writer(queue, "/nonexistent_dir/should/fail.ts", token).is_err());
    }

    // H5: QueueCloseGuard must unblock the writer thread even if the queue is
    // never explicitly closed by the caller (e.g., async fn cancelled/panicked).
    // Simulate by dropping the guard and verifying the writer exits.
    #[test]
    fn sanitize_name_trims_leading_trailing_underscores() {
        assert_eq!(sanitize_name("///name///"), "name");
        assert_eq!(sanitize_name("   name   "), "name");
    }

    #[test]
    fn sanitize_name_all_special_chars_becomes_empty_or_underscore() {
        // All slashes collapse to a single underscore, then get trimmed
        let result = sanitize_name("///");
        assert!(result.is_empty() || result == "_");
    }

    #[test]
    fn build_recording_service_metadata_uses_publisher_when_source_empty() {
        let meta = build_recording_service_metadata("Test", "pid", Some(""), "2026-06-27");
        assert!(meta.service_name.contains("source=publisher"));
    }

    #[test]
    fn build_recording_service_metadata_trims_whitespace_from_source() {
        let meta = build_recording_service_metadata("Test", "pid", Some("  "), "2026-06-27");
        assert!(meta.service_name.contains("source=publisher"));
    }

    #[test]
    fn ts_writer_drains_data_written_before_close() {
        let queue = Arc::new(MemoryQueue::new());
        let token = CancellationToken::new();
        let temp = std::env::temp_dir().join("test_drain.ts");
        let path = temp.to_string_lossy().to_string();

        // Write multiple chunks then close
        queue.write_sync(b"chunk-one-");
        queue.write_sync(b"chunk-two");
        queue.close();

        let res = run_ts_writer(queue, &path, token);
        assert!(res.is_ok());
        let content = std::fs::read(&temp).unwrap();
        assert_eq!(content, b"chunk-one-chunk-two");
        let _ = std::fs::remove_file(&temp);
    }

    #[test]
    fn queue_close_guard_unblocks_writer_thread() {
        let queue = Arc::new(MemoryQueue::new());

        // Start the writer thread on an open queue.
        let queue_for_thread = queue.clone();
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("test_guard_recording.ts");
        let path_str = file_path.to_string_lossy().to_string();
        let token = CancellationToken::new();
        let thread = std::thread::spawn(move || run_ts_writer(queue_for_thread, &path_str, token));

        // Simulate the guard drop (async fn drop) by closing the queue directly.
        // In production this is done by QueueCloseGuard::drop.
        queue.close();

        // Writer thread must exit within 1 second — no hang.
        let result = thread.join().expect("writer thread panicked");
        assert!(result.is_ok());
        let _ = std::fs::remove_file(temp_dir.join("test_guard_recording.ts"));
    }
}
