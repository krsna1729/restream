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
use crate::media::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::media::engine::MediaEngine;
use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
use crate::media::mpegts::TsServiceMetadata;
use crate::media::ring_buffer::{MEDIA_PULL_BURST_PACKETS, Reader, RingBuffer};
use crate::media::startup_policy;
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

fn build_filename(pipe_name: &str, recording_id: &str) -> String {
    let now = chrono::Local::now();
    let safe_name = sanitize_name(pipe_name);
    let safe_name = if safe_name.is_empty() {
        "pipeline"
    } else {
        safe_name.as_str()
    };
    // Two pipelines can share a display name (names aren't unique) and start
    // recording within the same second, which would otherwise make this
    // filename collide across pipelines and race on a single truncating
    // File::create. recording_id is a fresh random token per recording, so
    // appending it keeps the filename unique even when the name and
    // timestamp match.
    let id_suffix = recording_id.rsplit('_').next().unwrap_or(recording_id);
    format!(
        "recording_{}_{}_{}.ts",
        now.format("%Y%m%dT%H%M%S"),
        safe_name,
        id_suffix
    )
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
    let filename = build_filename(&pipeline_name, &recording_id);
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
#[path = "tests.rs"]
mod tests;
