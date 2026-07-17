//! File-backed ingest pipeline that reads stored media through FFmpeg and
//! feeds the live ring-buffer graph as if it were an active publisher.

use crate::media::avio::{CustomOutput, MemoryQueue};
use crate::media::engine::{AudioMeta, IngestRegistration, MediaEngine, StageMetrics, VideoMeta};
use crate::media::mpegts::TsDemuxer;
use crate::media::ring_buffer::{MediaPacket, MediaType, RingBuffer};
use ffmpeg_next::{codec, encoder, format, media};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::slice;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;
use tracing::error;

type KeyframeTimes = Arc<std::sync::Mutex<Vec<i64>>>;
type IngestRuntime = (
    Arc<AtomicU64>,
    Arc<StageMetrics>,
    Arc<AtomicU64>,
    KeyframeTimes,
);

#[derive(Default)]
struct LoopTimestampState {
    offset_ms: i64,
    pass_base_timestamp_ms: Option<i64>,
    pass_max_timestamp_ms: Option<i64>,
    pass_packet_count: usize,
}

impl LoopTimestampState {
    fn packet_timestamp_ms(packet: &MediaPacket) -> i64 {
        if packet.dts >= 0 {
            packet.dts
        } else {
            packet.pts
        }
    }

    fn begin_pass(&mut self) {
        self.pass_base_timestamp_ms = None;
        self.pass_max_timestamp_ms = None;
        self.pass_packet_count = 0;
    }

    fn apply(&mut self, packet: &mut MediaPacket) {
        let pass_base_timestamp_ms = *self
            .pass_base_timestamp_ms
            .get_or_insert_with(|| Self::packet_timestamp_ms(packet));
        packet.pts = packet
            .pts
            .saturating_sub(pass_base_timestamp_ms)
            .saturating_add(self.offset_ms);
        packet.dts = packet
            .dts
            .saturating_sub(pass_base_timestamp_ms)
            .saturating_add(self.offset_ms);
        self.pass_packet_count += 1;
        let packet_max = packet.pts.max(packet.dts);
        self.pass_max_timestamp_ms = Some(
            self.pass_max_timestamp_ms
                .map_or(packet_max, |current| current.max(packet_max)),
        );
    }

    fn finish_pass(&mut self) {
        if let Some(max_timestamp_ms) = self.pass_max_timestamp_ms {
            self.offset_ms = max_timestamp_ms.saturating_add(1);
        }
    }

    fn pass_packet_count(&self) -> usize {
        self.pass_packet_count
    }
}

#[derive(Debug, Clone, Copy)]
struct LoopStartupGate {
    waiting_for_video_startup: bool,
}

impl LoopStartupGate {
    fn new(has_video: bool) -> Self {
        Self {
            waiting_for_video_startup: has_video,
        }
    }

    fn filter_packet(&mut self, packet: &MediaPacket, ring_buffer: &Arc<RingBuffer>) -> bool {
        if !self.waiting_for_video_startup {
            return true;
        }

        match packet.media_type {
            MediaType::Audio => false,
            MediaType::Video => {
                if let Some(parameter_sets) =
                    crate::media::codec::annexb_parameter_sets(&packet.payload)
                {
                    ring_buffer.set_video_parameter_sets(parameter_sets);
                }
                if packet.is_keyframe {
                    self.waiting_for_video_startup = false;
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct ContinuousTimestampState {
    offset_ms: i64,
    last_timestamp_ms_by_stream: HashMap<u64, i64>,
}

impl ContinuousTimestampState {
    fn stream_key(packet: &MediaPacket) -> u64 {
        ((packet.media_type as u64) << 32) | u64::from(packet.track_index)
    }

    fn continuity_timestamp_ms(packet: &MediaPacket) -> i64 {
        if packet.dts >= 0 {
            packet.dts
        } else {
            packet.pts
        }
    }

    pub(crate) fn apply(&mut self, packet: &mut MediaPacket) {
        let stream_key = Self::stream_key(packet);
        let raw_timestamp_ms = Self::continuity_timestamp_ms(packet);
        if let Some(last_timestamp_ms) = self.last_timestamp_ms_by_stream.get(&stream_key).copied()
        {
            let adjusted_timestamp_ms = raw_timestamp_ms.saturating_add(self.offset_ms);
            if adjusted_timestamp_ms <= last_timestamp_ms {
                self.offset_ms = last_timestamp_ms
                    .saturating_add(1)
                    .saturating_sub(raw_timestamp_ms);
            }
        }

        packet.pts = packet.pts.saturating_add(self.offset_ms);
        packet.dts = packet.dts.saturating_add(self.offset_ms);
        let adjusted_timestamp_ms = Self::continuity_timestamp_ms(packet);
        self.last_timestamp_ms_by_stream.insert(
            stream_key,
            self.last_timestamp_ms_by_stream
                .get(&stream_key)
                .copied()
                .map_or(adjusted_timestamp_ms, |current| {
                    current.max(adjusted_timestamp_ms)
                }),
        );
    }
}

pub fn use_internal_file_ingest(config: &crate::AppConfig) -> bool {
    config.use_internal_file_ingest
}

fn seconds_to_ms(seconds: f64) -> Result<Option<i64>, String> {
    let ms = seconds * 1000.0;
    if !ms.is_finite() || ms > i64::MAX as f64 || ms < i64::MIN as f64 {
        return Err("start_time is out of range".to_string());
    }
    Ok(Some(ms.round() as i64))
}

pub fn parse_start_time_ms(input: &str) -> Result<Option<i64>, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if let Ok(seconds) = trimmed.parse::<f64>() {
        if !seconds.is_finite() {
            return Err("start_time must be a finite number".to_string());
        }
        if seconds < 0.0 {
            return Err("start_time must be non-negative".to_string());
        }
        return seconds_to_ms(seconds);
    }

    let parts: Vec<&str> = trimmed.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return Err("start_time must be seconds or MM:SS(.mmm) or HH:MM:SS(.mmm)".to_string());
    }

    let seconds = parts[parts.len() - 1]
        .parse::<f64>()
        .map_err(|_| "invalid seconds component in start_time".to_string())?;
    if !seconds.is_finite() {
        return Err("start_time must be a finite number".to_string());
    }
    if seconds < 0.0 {
        return Err("start_time must be non-negative".to_string());
    }

    let minutes = parts[parts.len() - 2]
        .parse::<i64>()
        .map_err(|_| "invalid minutes component in start_time".to_string())?;
    if minutes < 0 {
        return Err("start_time must be non-negative".to_string());
    }

    let hours = if parts.len() == 3 {
        let value = parts[0]
            .parse::<i64>()
            .map_err(|_| "invalid hours component in start_time".to_string())?;
        if value < 0 {
            return Err("start_time must be non-negative".to_string());
        }
        value
    } else {
        0
    };

    let hours_secs = hours
        .checked_mul(3600)
        .ok_or_else(|| "start_time is out of range".to_string())?;
    let minutes_secs = minutes
        .checked_mul(60)
        .ok_or_else(|| "start_time is out of range".to_string())?;
    let total_secs_int = hours_secs
        .checked_add(minutes_secs)
        .ok_or_else(|| "start_time is out of range".to_string())?;

    seconds_to_ms(total_secs_int as f64 + seconds)
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_internal_file_ingest(
    engine: Arc<MediaEngine>,
    runtime_handle: Handle,
    ingest_id: String,
    pipeline_id: String,
    file_path: PathBuf,
    start_time: String,
    loop_enabled: bool,
    ring_buffer: Arc<RingBuffer>,
    registration: IngestRegistration,
) -> Result<(), String> {
    let cancel = registration.cancel_token.clone();
    let seek_ms = parse_start_time_ms(&start_time)?;
    let engine_for_thread = engine.clone();
    let runtime_for_thread = runtime_handle.clone();
    let ingest_id_for_thread = ingest_id.clone();
    let pipeline_id_for_thread = pipeline_id.clone();
    let handle = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_internal_file_ingest_loop(
                engine_for_thread.clone(),
                runtime_for_thread.clone(),
                &pipeline_id_for_thread,
                &file_path,
                seek_ms,
                loop_enabled,
                ring_buffer,
                cancel.clone(),
            )
        }));
        let mut disconnect_phase: Option<String> = None;
        let mut disconnect_reason: Option<String> = None;
        let mut disconnect_had_error = false;

        match result {
            Ok(Err(err)) if !cancel.is_cancelled() => {
                error!(
                    "[file-ingest] internal ingest failed ({}): {}",
                    ingest_id_for_thread, err
                );
                disconnect_phase = Some("decode".to_string());
                disconnect_reason = Some(err);
                disconnect_had_error = true;
            }
            Err(_) if !cancel.is_cancelled() => {
                error!(
                    "[file-ingest] internal ingest panicked ({})",
                    ingest_id_for_thread
                );
                disconnect_phase = Some("panic".to_string());
                disconnect_reason = Some("internal file ingest panicked".to_string());
                disconnect_had_error = true;
            }
            Ok(Ok(())) if !cancel.is_cancelled() && !loop_enabled => {
                disconnect_phase = Some("eof".to_string());
                disconnect_reason = Some("file ingest reached end of input".to_string());
            }
            _ => {}
        }

        runtime_for_thread.block_on(async {
            engine_for_thread
                .record_ingest_disconnect_if_current(
                    &pipeline_id_for_thread,
                    &registration,
                    disconnect_phase.as_deref(),
                    disconnect_reason,
                    disconnect_had_error,
                )
                .await;
            engine_for_thread
                .clear_file_ingest_running(&ingest_id_for_thread)
                .await;
            engine_for_thread
                .unregister_ingest_if_current(&pipeline_id_for_thread, &registration)
                .await;
        });
    });
    engine.register_os_thread(handle);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_internal_file_ingest_loop(
    engine: Arc<MediaEngine>,
    runtime_handle: Handle,
    pipeline_id: &str,
    file_path: &Path,
    seek_ms: Option<i64>,
    loop_enabled: bool,
    ring_buffer: Arc<RingBuffer>,
    cancel: CancellationToken,
) -> Result<(), String> {
    let (bytes_received, ingest_metrics, last_progress_ms, cached_keyframe_times) =
        load_ingest_runtime(&engine, &runtime_handle, pipeline_id)?;
    let mut timestamps = LoopTimestampState::default();

    loop {
        if cancel.is_cancelled() {
            break;
        }

        timestamps.begin_pass();
        run_internal_file_ingest_once(
            &engine,
            &runtime_handle,
            pipeline_id,
            file_path,
            seek_ms,
            &ring_buffer,
            &cancel,
            &bytes_received,
            &ingest_metrics,
            &last_progress_ms,
            &cached_keyframe_times,
            &mut timestamps,
        )?;
        timestamps.finish_pass();

        if !cancel.is_cancelled() && !loop_enabled {
            ring_buffer.mark_end_of_stream();
        }

        if cancel.is_cancelled() || !loop_enabled {
            break;
        }

        if timestamps.pass_packet_count() == 0 {
            return Err(
                "Looped file ingest produced no packets; stopping to avoid a tight loop"
                    .to_string(),
            );
        }
    }

    Ok(())
}

fn load_ingest_runtime(
    engine: &Arc<MediaEngine>,
    runtime_handle: &Handle,
    pipeline_id: &str,
) -> Result<IngestRuntime, String> {
    runtime_handle.block_on(async {
        let ingests = engine.ingests.active.read().await;
        let ingest = ingests
            .get(pipeline_id)
            .ok_or_else(|| format!("Active ingest missing for pipeline {pipeline_id}"))?;
        Ok((
            ingest.bytes_received.clone(),
            ingest.metrics.clone(),
            ingest.last_progress_ms.clone(),
            ingest.keyframe_times.clone(),
        ))
    })
}

fn h264_startup_state_from_stream(
    stream: &ffmpeg_next::Stream<'_>,
) -> Option<(Vec<u8>, bytes::Bytes)> {
    if stream.parameters().id() != codec::Id::H264 {
        return None;
    }

    let extradata = unsafe {
        let params = stream.parameters().as_ptr();
        if params.is_null() || (*params).extradata.is_null() || (*params).extradata_size <= 0 {
            return None;
        }
        slice::from_raw_parts(
            (*params).extradata.cast::<u8>(),
            (*params).extradata_size as usize,
        )
    };

    let annexb = if extradata.starts_with(&[0x00, 0x00, 0x01])
        || extradata.starts_with(&[0x00, 0x00, 0x00, 0x01])
    {
        extradata.to_vec()
    } else if extradata.first() == Some(&0x01) {
        let (_, annexb) = crate::media::codec::parse_avcc_config(extradata);
        annexb
    } else {
        Vec::new()
    };

    if annexb.is_empty() {
        return None;
    }

    let sequence_header = crate::media::codec::build_avcc_sequence_header(&annexb)?;
    Some((annexb, sequence_header))
}

fn ingest_codec_name(id: ffmpeg_next::codec::Id) -> String {
    match id {
        ffmpeg_next::codec::Id::H264 => "h264",
        ffmpeg_next::codec::Id::HEVC => "hevc",
        ffmpeg_next::codec::Id::AAC => "aac",
        other => return format!("{other:?}").to_ascii_lowercase(),
    }
    .to_string()
}

fn prime_input_video_startup_state(
    engine: &Arc<MediaEngine>,
    runtime_handle: &Handle,
    pipeline_id: &str,
    ring_buffer: &Arc<RingBuffer>,
    ictx: &format::context::Input,
) {
    let Some(video_stream) = ictx
        .streams()
        .find(|stream| stream.parameters().medium() == media::Type::Video)
    else {
        return;
    };

    let Some((parameter_sets, sequence_header)) = h264_startup_state_from_stream(&video_stream)
    else {
        return;
    };

    ring_buffer.set_video_parameter_sets(parameter_sets);
    runtime_handle.block_on(async {
        engine
            .cache_sequence_header(pipeline_id, true, sequence_header)
            .await;
    });
}

// The internal TS-remux probe (see `maybe_publish_probe`) only resolves once
// every stream has emitted a real-time-paced packet, so `ingest.video` /
// `ingest.audio_tracks` otherwise stay empty until playback reaches whichever
// track's first frame sits latest in the file's own timeline. Container
// stream headers carry width/height/sample_rate/channel-count directly, so
// read those up front to unblock `wait_for_stage_metadata` immediately.
fn prime_input_container_metadata(
    engine: &Arc<MediaEngine>,
    runtime_handle: &Handle,
    pipeline_id: &str,
    ictx: &format::context::Input,
) {
    let mut video_meta = None;
    let mut audio_tracks = Vec::new();
    let mut track_index = 0u32;

    for stream in ictx.streams() {
        let params = stream.parameters();
        match params.medium() {
            media::Type::Video if video_meta.is_none() => unsafe {
                let ptr = params.as_ptr();
                if ptr.is_null() {
                    continue;
                }
                let width = (*ptr).width.max(0) as u32;
                let height = (*ptr).height.max(0) as u32;
                if width > 0 && height > 0 {
                    video_meta = Some(VideoMeta {
                        codec: ingest_codec_name(params.id()),
                        width,
                        height,
                        fps: 0.0,
                        bw: None,
                        pid: None,
                        language: None,
                        title: None,
                        profile: None,
                        level: None,
                        pixel_format: None,
                    });
                }
            },
            media::Type::Audio => {
                let (sample_rate, channels) = unsafe {
                    let ptr = params.as_ptr();
                    if ptr.is_null() {
                        (0, 0)
                    } else {
                        (
                            (*ptr).sample_rate.max(0) as u32,
                            (*ptr).ch_layout.nb_channels.max(0) as u32,
                        )
                    }
                };
                if sample_rate > 0 && channels > 0 {
                    audio_tracks.push(AudioMeta {
                        codec: ingest_codec_name(params.id()),
                        sample_rate,
                        channels,
                        track_index,
                        ..Default::default()
                    });
                }
                track_index += 1;
            }
            _ => {}
        }
    }

    if video_meta.is_none() && audio_tracks.is_empty() {
        return;
    }

    let first_audio = audio_tracks.first().cloned();
    runtime_handle.block_on(async {
        engine
            .update_ingest_meta(pipeline_id, video_meta, first_audio, None)
            .await;
        if !audio_tracks.is_empty() {
            engine
                .update_ingest_audio_tracks(pipeline_id, audio_tracks)
                .await;
        }
    });
}

// `build_avcc_sequence_header` only understands H.264 NAL types, so it
// returns `None` for HEVC parameter sets. The FLV/AVCC sequence header cache
// is an H.264-only optimization (raw RTMP passthrough); `video_parameter_sets`
// on the ring buffer must still be set for any codec so
// `wait_for_stage_metadata` can unblock transcoder stages waiting on it.
fn prime_input_video_startup_state_from_packet(
    engine: &Arc<MediaEngine>,
    runtime_handle: &Handle,
    pipeline_id: &str,
    ring_buffer: &Arc<RingBuffer>,
    payload: &[u8],
) -> bool {
    let Some(parameter_sets) = crate::media::codec::annexb_parameter_sets(payload) else {
        return false;
    };

    let sequence_header = crate::media::codec::build_avcc_sequence_header(&parameter_sets);
    ring_buffer.set_video_parameter_sets(parameter_sets);
    if let Some(sequence_header) = sequence_header {
        runtime_handle.block_on(async {
            engine
                .cache_sequence_header(pipeline_id, true, sequence_header)
                .await;
        });
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn run_internal_file_ingest_once(
    engine: &Arc<MediaEngine>,
    runtime_handle: &Handle,
    pipeline_id: &str,
    file_path: &Path,
    seek_ms: Option<i64>,
    ring_buffer: &Arc<RingBuffer>,
    cancel: &CancellationToken,
    bytes_received: &Arc<AtomicU64>,
    ingest_metrics: &Arc<StageMetrics>,
    last_progress_ms: &Arc<AtomicU64>,
    cached_keyframe_times: &KeyframeTimes,
    timestamps: &mut LoopTimestampState,
) -> Result<(), String> {
    let mut ictx = format::input_with_interrupt(&file_path, || cancel.is_cancelled())
        .map_err(|e| format!("Failed to open input file: {e}"))?;
    let has_video_stream = ictx
        .streams()
        .any(|stream| stream.parameters().medium() == media::Type::Video);

    if let Some(seek_ms) = seek_ms {
        ictx.seek(seek_ms.saturating_mul(1000), ..)
            .map_err(|e| format!("Failed to seek input file: {e}"))?;
    }

    let queue = MemoryQueue::new_with_capacity(engine.config.avio_capacity);
    let mut custom_output =
        CustomOutput::new(&queue, "mpegts").map_err(|e| format!("TS mux setup failed: {e}"))?;
    let octx = custom_output
        .output
        .as_mut()
        .ok_or_else(|| "Failed to acquire TS output context".to_string())?;

    let mut startup_video_state_primed = false;
    prime_input_video_startup_state(engine, runtime_handle, pipeline_id, ring_buffer, &ictx);
    prime_input_container_metadata(engine, runtime_handle, pipeline_id, &ictx);

    let mut stream_mapping = vec![-1i32; ictx.nb_streams() as usize];
    let mut ist_time_bases = vec![ffmpeg_next::Rational(0, 1); ictx.nb_streams() as usize];
    let mut ost_index = 0i32;

    for (ist_index, ist) in ictx.streams().enumerate() {
        let medium = ist.parameters().medium();
        if medium != media::Type::Audio && medium != media::Type::Video {
            continue;
        }

        stream_mapping[ist_index] = ost_index;
        ist_time_bases[ist_index] = ist.time_base();
        ost_index += 1;

        let mut ost = octx
            .add_stream(encoder::find(codec::Id::None))
            .map_err(|e| format!("Failed to add TS stream: {e}"))?;
        ost.set_parameters(ist.parameters());
        unsafe {
            (*ost.parameters().as_mut_ptr()).codec_tag = 0;
        }
    }

    octx.set_metadata(ictx.metadata().to_owned());
    octx.write_header()
        .map_err(|e| format!("Failed to write TS header: {e}"))?;

    let mut demuxer = TsDemuxer::new();
    let mut packets = Vec::with_capacity(16);
    let mut probe_sent = false;
    let mut startup_gate = LoopStartupGate::new(has_video_stream);
    drain_remuxed_ts(
        engine,
        runtime_handle,
        pipeline_id,
        &queue,
        &mut demuxer,
        &mut packets,
        ring_buffer,
        bytes_received,
        ingest_metrics,
        last_progress_ms,
        cached_keyframe_times,
        timestamps,
        &mut startup_gate,
        &mut probe_sent,
    );

    let mut pace_anchor = None;
    for (stream, mut packet) in ictx.packets() {
        if cancel.is_cancelled() {
            break;
        }

        let ist_index = stream.index();
        if !startup_video_state_primed
            && stream.parameters().medium() == media::Type::Video
            && let Some(payload) = packet.data()
        {
            startup_video_state_primed = prime_input_video_startup_state_from_packet(
                engine,
                runtime_handle,
                pipeline_id,
                ring_buffer,
                payload,
            );
        }
        let mapped_index = stream_mapping.get(ist_index).copied().unwrap_or(-1);
        if mapped_index < 0 {
            continue;
        }

        if let Some(packet_ts_ms) = packet_timestamp_ms(&stream, &packet) {
            pace_packet(cancel, &mut pace_anchor, packet_ts_ms);
            if cancel.is_cancelled() {
                break;
            }
        }

        let ost = octx
            .stream(mapped_index as usize)
            .ok_or_else(|| format!("Missing TS output stream {}", mapped_index))?;
        packet.rescale_ts(ist_time_bases[ist_index], ost.time_base());
        packet.set_position(-1);
        packet.set_stream(mapped_index as usize);
        packet
            .write_interleaved(octx)
            .map_err(|e| format!("Failed to mux TS packet: {e}"))?;

        drain_remuxed_ts(
            engine,
            runtime_handle,
            pipeline_id,
            &queue,
            &mut demuxer,
            &mut packets,
            ring_buffer,
            bytes_received,
            ingest_metrics,
            last_progress_ms,
            cached_keyframe_times,
            timestamps,
            &mut startup_gate,
            &mut probe_sent,
        );
    }

    octx.write_trailer()
        .map_err(|e| format!("Failed to finalize TS mux: {e}"))?;
    drain_remuxed_ts(
        engine,
        runtime_handle,
        pipeline_id,
        &queue,
        &mut demuxer,
        &mut packets,
        ring_buffer,
        bytes_received,
        ingest_metrics,
        last_progress_ms,
        cached_keyframe_times,
        timestamps,
        &mut startup_gate,
        &mut probe_sent,
    );

    demuxer.flush();
    push_demuxed_packets(
        &mut demuxer,
        &mut packets,
        ring_buffer,
        cached_keyframe_times,
        timestamps,
        &mut startup_gate,
    );
    maybe_publish_probe(
        engine,
        runtime_handle,
        pipeline_id,
        &mut demuxer,
        &mut probe_sent,
    );

    Ok(())
}

fn packet_timestamp_ms(
    stream: &ffmpeg_next::Stream<'_>,
    packet: &ffmpeg_next::Packet,
) -> Option<i64> {
    let ts = packet.dts().or_else(|| packet.pts())?;
    let tb = stream.time_base();
    if tb.1 == 0 {
        return Some(ts);
    }
    Some((ts as i128 * tb.0 as i128 * 1000 / tb.1 as i128) as i64)
}

fn pace_packet(cancel: &CancellationToken, anchor: &mut Option<(i64, Instant)>, packet_ts_ms: i64) {
    if packet_ts_ms < 0 {
        return;
    }

    if anchor.is_none() {
        *anchor = Some((packet_ts_ms, Instant::now()));
        return;
    }

    let (base_ts_ms, start_instant) = anchor.expect("anchor initialized above");
    // Interleaved streams can deliver a packet timestamped slightly before the
    // anchor (e.g. audio that starts earlier than the first video packet in
    // mux order). A negative delta must clamp to zero — casting it straight
    // to u64 would wrap into a near-infinite sleep and hang the ingest.
    let desired_ms = packet_ts_ms.saturating_sub(base_ts_ms).max(0) as u64;
    let desired = Duration::from_millis(desired_ms);
    let elapsed = start_instant.elapsed();
    if elapsed >= desired {
        return;
    }

    let mut remaining = desired - elapsed;
    while remaining > Duration::ZERO && !cancel.is_cancelled() {
        let slice = remaining.min(Duration::from_millis(25));
        std::thread::sleep(slice);
        remaining = desired.saturating_sub(start_instant.elapsed());
    }
}

#[allow(clippy::too_many_arguments)]
fn drain_remuxed_ts(
    engine: &Arc<MediaEngine>,
    runtime_handle: &Handle,
    pipeline_id: &str,
    queue: &MemoryQueue,
    demuxer: &mut TsDemuxer,
    packets: &mut Vec<MediaPacket>,
    ring_buffer: &Arc<RingBuffer>,
    bytes_received: &Arc<AtomicU64>,
    ingest_metrics: &Arc<StageMetrics>,
    last_progress_ms: &Arc<AtomicU64>,
    cached_keyframe_times: &KeyframeTimes,
    timestamps: &mut LoopTimestampState,
    startup_gate: &mut LoopStartupGate,
    probe_sent: &mut bool,
) {
    let mut buf = [0u8; 64 * 1024];

    loop {
        let read = queue.read_nonblocking(&mut buf);
        if read == 0 {
            break;
        }

        demuxer.feed(&buf[..read]);
        push_demuxed_packets(
            demuxer,
            packets,
            ring_buffer,
            cached_keyframe_times,
            timestamps,
            startup_gate,
        );
        maybe_publish_probe(engine, runtime_handle, pipeline_id, demuxer, probe_sent);
        bytes_received.fetch_add(read as u64, Ordering::Relaxed);
        ingest_metrics.record_in(read as u64);
        last_progress_ms.store(MediaEngine::now_epoch_ms(), Ordering::Relaxed);
    }
}

fn push_demuxed_packets(
    demuxer: &mut TsDemuxer,
    packets: &mut Vec<MediaPacket>,
    ring_buffer: &Arc<RingBuffer>,
    cached_keyframe_times: &KeyframeTimes,
    timestamps: &mut LoopTimestampState,
    startup_gate: &mut LoopStartupGate,
) {
    if demuxer.drain_into(packets) == 0 {
        return;
    }

    packets.retain(|pkt| startup_gate.filter_packet(pkt, ring_buffer));
    if packets.is_empty() {
        return;
    }

    for pkt in packets.iter_mut() {
        timestamps.apply(pkt);
    }

    for pkt in packets.iter() {
        if pkt.media_type == MediaType::Video
            && let Some(parameter_sets) = crate::media::codec::annexb_parameter_sets(&pkt.payload)
        {
            ring_buffer.set_video_parameter_sets(parameter_sets);
        }
        if pkt.media_type == MediaType::Video && pkt.is_keyframe {
            let mut times = cached_keyframe_times
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            times.push(pkt.pts);
            if times.len() > 30 {
                times.remove(0);
            }
        }
    }

    ring_buffer.push_drained_batch_capped(packets);
}

fn maybe_publish_probe(
    engine: &Arc<MediaEngine>,
    runtime_handle: &Handle,
    pipeline_id: &str,
    demuxer: &mut TsDemuxer,
    probe_sent: &mut bool,
) {
    if *probe_sent {
        return;
    }

    let Some(probe) = demuxer.take_probe() else {
        return;
    };
    *probe_sent = true;
    let first_audio = probe.audio_tracks.first().cloned();
    let video_sequence_header = probe.video_sequence_header.clone();
    let selected_video_track_index = probe.video.as_ref().map(|_| 0);
    runtime_handle.block_on(async {
        engine
            .update_ingest_meta(pipeline_id, probe.video, first_audio, None)
            .await;
        if let Some(sequence_header) = video_sequence_header {
            engine
                .cache_sequence_header(pipeline_id, true, sequence_header)
                .await;
        }
        engine
            .update_ingest_video_track_selection(
                pipeline_id,
                probe.video_track_count,
                selected_video_track_index,
            )
            .await;
        if !probe.audio_tracks.is_empty() {
            engine
                .update_ingest_audio_tracks(pipeline_id, probe.audio_tracks)
                .await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::maybe_publish_probe;
    use super::parse_start_time_ms;
    use super::prime_input_container_metadata;
    use super::prime_input_video_startup_state_from_packet;
    use super::spawn_internal_file_ingest;
    use super::{ContinuousTimestampState, LoopStartupGate, LoopTimestampState};
    use crate::media::engine::MediaEngine;
    use crate::media::mpegts::TsDemuxer;
    use crate::media::ring_buffer::{MediaPacket, MediaType, PayloadFormat, RingBuffer};
    use bytes::Bytes;
    use ffmpeg_next::format;
    use std::sync::Arc;
    use tokio::time::{Duration, sleep};

    #[test]
    fn internal_file_ingest_flag_uses_typed_config() {
        let disabled = crate::AppConfig {
            use_internal_file_ingest: false,
            ..crate::AppConfig::default()
        };
        let enabled = crate::AppConfig {
            use_internal_file_ingest: true,
            ..crate::AppConfig::default()
        };

        assert!(!super::use_internal_file_ingest(&disabled));
        assert!(super::use_internal_file_ingest(&enabled));
    }

    #[test]
    fn empty_start_time_is_none() {
        assert_eq!(parse_start_time_ms("").unwrap(), None);
        assert_eq!(parse_start_time_ms("   ").unwrap(), None);
    }

    #[test]
    fn parses_seconds_start_time() {
        assert_eq!(parse_start_time_ms("5").unwrap(), Some(5_000));
        assert_eq!(parse_start_time_ms("1.25").unwrap(), Some(1_250));
    }

    #[test]
    fn parses_colon_delimited_start_time() {
        assert_eq!(parse_start_time_ms("00:00:05").unwrap(), Some(5_000));
        assert_eq!(parse_start_time_ms("01:02:03.5").unwrap(), Some(3_723_500));
        assert_eq!(parse_start_time_ms("02:03.25").unwrap(), Some(123_250));
    }

    #[test]
    fn rejects_invalid_start_time() {
        assert!(parse_start_time_ms("-1").is_err());
        assert!(parse_start_time_ms("1:two").is_err());
        assert!(parse_start_time_ms("1:2:3:4").is_err());
    }

    #[test]
    fn rejects_non_finite_plain_seconds() {
        assert!(parse_start_time_ms("NaN").is_err());
        assert!(parse_start_time_ms("nan").is_err());
        assert!(parse_start_time_ms("inf").is_err());
        assert!(parse_start_time_ms("infinity").is_err());
        assert!(parse_start_time_ms("-inf").is_err());
    }

    #[test]
    fn rejects_non_finite_colon_delimited_seconds_component() {
        assert!(parse_start_time_ms("00:nan").is_err());
        assert!(parse_start_time_ms("00:00:inf").is_err());
    }

    #[test]
    fn rejects_float_to_millisecond_overflow() {
        assert!(parse_start_time_ms("1e30").is_err());
        assert!(parse_start_time_ms("00:00:1e30").is_err());
    }

    #[test]
    fn rejects_colon_delimited_integer_overflow() {
        // Individually parseable i64 components whose hours*3600 or
        // minutes*60 scaling overflows i64 before any float arithmetic runs.
        assert!(parse_start_time_ms("9223372036854775807:00:00").is_err());
        assert!(parse_start_time_ms("00:9223372036854775807:00").is_err());
        assert!(parse_start_time_ms("9223372036854775807:9223372036854775807:00").is_err());
    }

    #[test]
    fn pace_packet_does_not_sleep_for_timestamps_behind_the_anchor() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut anchor = None;
        super::pace_packet(&cancel, &mut anchor, 1_433);

        let start = std::time::Instant::now();
        super::pace_packet(&cancel, &mut anchor, 1_400);
        assert!(
            start.elapsed() < std::time::Duration::from_millis(200),
            "packet behind the pace anchor must pass through without sleeping"
        );
    }

    #[test]
    fn loop_timestamp_state_keeps_replayed_packets_monotonic() {
        let mut timestamps = LoopTimestampState::default();

        timestamps.begin_pass();
        let mut first = test_packet(MediaType::Video, 0, 0, 0);
        let mut second = test_packet(MediaType::Video, 0, 33, 33);
        timestamps.apply(&mut first);
        timestamps.apply(&mut second);
        timestamps.finish_pass();

        assert_eq!(first.pts, 0);
        assert_eq!(second.pts, 33);
        assert_eq!(timestamps.pass_packet_count(), 2);

        timestamps.begin_pass();
        let mut looped_first = test_packet(MediaType::Video, 0, 0, 0);
        let mut looped_second = test_packet(MediaType::Video, 0, 33, 33);
        timestamps.apply(&mut looped_first);
        timestamps.apply(&mut looped_second);
        timestamps.finish_pass();

        assert_eq!(looped_first.pts, 34);
        assert_eq!(looped_first.dts, 34);
        assert_eq!(looped_second.pts, 67);
        assert_eq!(looped_second.dts, 67);
        assert_eq!(timestamps.pass_packet_count(), 2);
    }

    #[test]
    fn loop_timestamp_state_reports_empty_passes() {
        let mut timestamps = LoopTimestampState::default();
        timestamps.begin_pass();
        timestamps.finish_pass();

        assert_eq!(timestamps.pass_packet_count(), 0);
    }

    #[test]
    fn loop_timestamp_state_normalizes_nonzero_file_offsets() {
        let mut timestamps = LoopTimestampState::default();

        timestamps.begin_pass();
        let mut first = test_packet(MediaType::Video, 0, 1_467, 1_400);
        let mut second = test_packet(MediaType::Audio, 0, 1_445, 1_445);
        let mut third = test_packet(MediaType::Video, 0, 1_500, 1_433);
        timestamps.apply(&mut first);
        timestamps.apply(&mut second);
        timestamps.apply(&mut third);
        timestamps.finish_pass();

        assert_eq!(first.pts, 67);
        assert_eq!(first.dts, 0);
        assert_eq!(second.pts, 45);
        assert_eq!(second.dts, 45);
        assert_eq!(third.pts, 100);
        assert_eq!(third.dts, 33);

        timestamps.begin_pass();
        let mut replayed = test_packet(MediaType::Video, 0, 1_467, 1_400);
        timestamps.apply(&mut replayed);
        timestamps.finish_pass();

        assert_eq!(replayed.dts, 101);
    }

    #[test]
    fn continuous_timestamp_state_offsets_replayed_subprocess_packets() {
        let mut timestamps = ContinuousTimestampState::default();

        let mut first = test_packet(MediaType::Video, 0, 0, 0);
        let mut second = test_packet(MediaType::Video, 0, 40, 40);
        timestamps.apply(&mut first);
        timestamps.apply(&mut second);

        let mut replayed_first = test_packet(MediaType::Video, 0, 0, 0);
        let mut replayed_second = test_packet(MediaType::Video, 0, 40, 40);
        timestamps.apply(&mut replayed_first);
        timestamps.apply(&mut replayed_second);

        assert_eq!(first.pts, 0);
        assert_eq!(second.pts, 40);
        assert_eq!(replayed_first.pts, 41);
        assert_eq!(replayed_first.dts, 41);
        assert_eq!(replayed_second.pts, 81);
        assert_eq!(replayed_second.dts, 81);
    }

    #[test]
    fn continuous_timestamp_state_preserves_interleaved_audio_video_timestamps() {
        let mut timestamps = ContinuousTimestampState::default();

        let mut video0 = test_packet(MediaType::Video, 0, 0, 0);
        let mut audio0 = test_packet(MediaType::Audio, 0, 0, 0);
        let mut audio1 = test_packet(MediaType::Audio, 0, 21, 21);
        let mut video1 = test_packet(MediaType::Video, 0, 33, 33);

        timestamps.apply(&mut video0);
        timestamps.apply(&mut audio0);
        timestamps.apply(&mut audio1);
        timestamps.apply(&mut video1);

        assert_eq!(video0.pts, 0);
        assert_eq!(audio0.pts, 0);
        assert_eq!(audio1.pts, 21);
        assert_eq!(video1.pts, 33);
    }

    #[test]
    fn continuous_timestamp_state_uses_dts_for_reordered_video_packets() {
        let mut timestamps = ContinuousTimestampState::default();

        let mut anchor = test_packet(MediaType::Video, 0, 0, 0);
        let mut reordered_p = test_packet(MediaType::Video, 0, 100, 33);
        let mut reordered_b = test_packet(MediaType::Video, 0, 66, 66);

        timestamps.apply(&mut anchor);
        timestamps.apply(&mut reordered_p);
        timestamps.apply(&mut reordered_b);

        assert_eq!(anchor.pts, 0);
        assert_eq!(reordered_p.pts, 100);
        assert_eq!(reordered_p.dts, 33);
        assert_eq!(reordered_b.pts, 66);
        assert_eq!(reordered_b.dts, 66);
    }

    #[test]
    fn loop_startup_gate_waits_for_keyframe_before_releasing_packets() {
        let ring = Arc::new(RingBuffer::new(64));
        let mut gate = LoopStartupGate::new(true);
        let delta_video = MediaPacket {
            media_type: MediaType::Video,
            format: PayloadFormat::Raw,
            is_keyframe: false,
            track_index: 0,
            pts: 0,
            dts: 0,
            payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0xDD]),
        };
        let mut keyframe_video = test_packet(MediaType::Video, 0, 33, 33);
        keyframe_video.is_keyframe = true;
        let audio = test_packet(MediaType::Audio, 0, 10, 10);

        assert!(
            !gate.filter_packet(&audio, &ring),
            "audio must stay gated until the loop reaches a clean video boundary"
        );
        assert!(
            !gate.filter_packet(&delta_video, &ring),
            "delta video must not start a fresh file-ingest loop"
        );
        assert!(gate.filter_packet(&keyframe_video, &ring));
        assert!(
            gate.filter_packet(&audio, &ring),
            "once a loop starts on a keyframe, audio may flow again"
        );
    }

    fn test_packet(media_type: MediaType, track_index: u32, pts: i64, dts: i64) -> MediaPacket {
        MediaPacket {
            media_type,
            format: PayloadFormat::Raw,
            is_keyframe: false,
            track_index,
            pts,
            dts,
            payload: Bytes::from_static(b"packet"),
        }
    }

    #[tokio::test]
    async fn internal_file_ingest_pushes_packets_and_stays_registered() {
        let engine = Arc::new(MediaEngine::new());
        let pipeline_id = "pipe-file-ingest-test";
        let ingest_id = "ing-file-ingest-test";
        let stream_key = "file-ingest-test-key";
        let ring_buffer = engine.get_or_create_pipeline(pipeline_id).await;
        let registration = engine
            .try_register_ingest_attempt(pipeline_id, stream_key, "file")
            .await
            .expect("register ingest");

        engine.mark_file_ingest_running(ingest_id).await;
        spawn_internal_file_ingest(
            engine.clone(),
            tokio::runtime::Handle::current(),
            ingest_id.to_string(),
            pipeline_id.to_string(),
            crate::test_fixtures::canonical_h264_ts_fixture()
                .expect("checked-in transport fixture"),
            String::new(),
            false,
            ring_buffer.clone(),
            registration.clone(),
        )
        .expect("spawn internal ingest");

        sleep(Duration::from_secs(2)).await;

        assert!(
            engine.ingests.active.read().await.contains_key(pipeline_id),
            "internal ingest should still be registered while streaming"
        );
        assert!(
            ring_buffer.get_write_idx() > 0,
            "internal ingest should have produced media packets after startup"
        );

        registration.cancel_token.cancel();
        sleep(Duration::from_millis(250)).await;
    }

    #[tokio::test]
    async fn internal_bf0_file_ingest_caches_video_startup_state() {
        let engine = Arc::new(MediaEngine::new());
        let pipeline_id = "pipe-file-ingest-bf0-state";
        let ingest_id = "ing-file-ingest-bf0-state";
        let stream_key = "file-ingest-bf0-state-key";
        let ring_buffer = engine.get_or_create_pipeline(pipeline_id).await;
        let registration = engine
            .try_register_ingest_attempt(pipeline_id, stream_key, "file")
            .await
            .expect("register ingest");

        engine.mark_file_ingest_running(ingest_id).await;
        spawn_internal_file_ingest(
            engine.clone(),
            tokio::runtime::Handle::current(),
            ingest_id.to_string(),
            pipeline_id.to_string(),
            crate::test_fixtures::av_marker_transport_fixture_for_bframes(
                "h264",
                false,
                crate::test_fixtures::AvMarkerBframeMode::Bf0,
            )
            .expect("checked-in bf0 transport fixture"),
            String::new(),
            false,
            ring_buffer.clone(),
            registration.clone(),
        )
        .expect("spawn internal ingest");

        sleep(Duration::from_secs(2)).await;

        let (cached_video, _) = engine.get_sequence_headers(pipeline_id).await;
        let ring_parameter_sets = ring_buffer.video_parameter_sets();
        let ring_sequence_header = ring_parameter_sets
            .as_deref()
            .and_then(crate::media::codec::build_avcc_sequence_header);
        assert!(
            cached_video.is_some(),
            "internal BF0 file ingest should cache a startup video sequence header (ring parameter sets present: {}, ring startup header present: {})",
            ring_parameter_sets.is_some(),
            ring_sequence_header.is_some(),
        );

        registration.cancel_token.cancel();
        sleep(Duration::from_millis(250)).await;
    }

    #[test]
    fn prime_input_video_startup_state_from_packet_sets_ring_parameter_sets_for_hevc() {
        // HEVC has no AVCC/FLV sequence header (build_avcc_sequence_header
        // only understands H.264 NALs, so it returns None for HEVC VPS/SPS/
        // PPS), but wait_for_stage_metadata's eager-parameter-sets gate for
        // VideoPreset transcoder stages only needs
        // `ring_buffer.video_parameter_sets()` — that must still get set from
        // the first video packet even when no AVCC header can be built.
        let runtime = tokio::runtime::Runtime::new().expect("create runtime");
        let engine = Arc::new(MediaEngine::new());
        let pipeline_id = "pipe-file-hevc-paramsets-direct";
        runtime
            .block_on(engine.try_register_ingest_attempt(pipeline_id, "hevc-key", "file"))
            .expect("register ingest");
        let ring_buffer = runtime.block_on(engine.get_or_create_pipeline(pipeline_id));

        let video_payload = [
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
            0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC, 0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xDD,
        ];

        assert!(
            crate::media::codec::build_avcc_sequence_header(
                &crate::media::codec::annexb_parameter_sets(&video_payload)
                    .expect("hevc payload should carry VPS/SPS/PPS")
            )
            .is_none(),
            "sanity check: HEVC parameter sets must not build an AVCC header"
        );

        let primed = prime_input_video_startup_state_from_packet(
            &engine,
            runtime.handle(),
            pipeline_id,
            &ring_buffer,
            &video_payload,
        );

        assert!(primed, "priming should report success for HEVC packets");
        assert!(
            ring_buffer.video_parameter_sets().is_some(),
            "HEVC video packet should prime ring buffer parameter sets even without an AVCC header"
        );
    }

    #[test]
    fn prime_input_container_metadata_populates_ingest_before_any_packet_read() {
        let runtime = tokio::runtime::Runtime::new().expect("create runtime");
        let engine = Arc::new(MediaEngine::new());
        let pipeline_id = "pipe-file-ingest-eager-meta";
        runtime
            .block_on(engine.try_register_ingest_attempt(pipeline_id, "eager-meta-key", "file"))
            .expect("register ingest");

        let fixture = crate::test_fixtures::av_marker_transport_fixture("h265", true)
            .expect("checked-in 2-audio-track transport fixture");
        let ictx = format::input(&fixture).expect("open fixture container");

        // No packets have been read or paced yet: this proves metadata comes
        // from container stream headers, not from the packet-paced probe.
        prime_input_container_metadata(&engine, runtime.handle(), pipeline_id, &ictx);

        runtime.block_on(async {
            let ingests = engine.ingests.active.read().await;
            let ingest = ingests.get(pipeline_id).expect("ingest registered");
            let video = ingest.video.as_ref().expect("video meta primed eagerly");
            assert_eq!(video.codec, "hevc");
            assert!(video.width > 0 && video.height > 0);

            let audio_tracks = ingest
                .audio_tracks
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            assert_eq!(audio_tracks.len(), 2, "both audio tracks should be primed");
            for track in audio_tracks.iter() {
                assert!(track.sample_rate > 0 && track.channels > 0);
            }
        });
    }

    #[test]
    fn maybe_publish_probe_caches_h264_sequence_header() {
        let runtime = tokio::runtime::Runtime::new().expect("create runtime");
        let engine = Arc::new(MediaEngine::new());
        let pipeline_id = "pipe-file-probe-seqhdr";
        runtime
            .block_on(engine.try_register_ingest_attempt(pipeline_id, "stream-key", "file"))
            .expect("register ingest");

        let mut demuxer = TsDemuxer::new();
        let fixture = crate::test_fixtures::canonical_h264_ts_fixture()
            .expect("checked-in transport fixture");
        let fixture_bytes = std::fs::read(fixture).expect("read checked-in transport fixture");
        demuxer.feed(&fixture_bytes);
        demuxer.flush();

        let mut probe_sent = false;
        maybe_publish_probe(
            &engine,
            runtime.handle(),
            pipeline_id,
            &mut demuxer,
            &mut probe_sent,
        );

        let (cached_video, _) = runtime.block_on(engine.get_sequence_headers(pipeline_id));
        let cached_video = cached_video.expect("probe should cache an H.264 startup header");
        assert_eq!(cached_video[0], 0x17);
        assert_eq!(cached_video[1], 0x00);
    }
}
