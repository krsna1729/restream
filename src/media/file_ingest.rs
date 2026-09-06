//! File-backed ingest pipeline that reads stored media through FFmpeg and
//! feeds the live ring-buffer graph as if it were an active publisher.

mod start_time;
mod startup;
mod timeline;

use crate::media::avio::{CustomOutput, MemoryQueue};
use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::mpegts::TsDemuxer;
use crate::media::packet::{MediaPacket, MediaType};
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_metrics::StageMetrics;
use ffmpeg_next::{codec, encoder, format, media};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;
use tracing::error;

pub use start_time::parse_start_time_ms;
#[cfg(test)]
use start_time::seconds_to_ms;
pub(crate) use timeline::ContinuousTimestampState;
use timeline::{LoopTimestampState, pace_packet};

type KeyframeTimes = Arc<std::sync::Mutex<Vec<i64>>>;
type IngestRuntime = (
    Arc<AtomicU64>,
    Arc<StageMetrics>,
    Arc<AtomicU64>,
    KeyframeTimes,
);

struct FileIngestPass<'a> {
    engine: &'a Arc<MediaEngine>,
    runtime_handle: &'a Handle,
    pipeline_id: &'a str,
    ring_buffer: &'a Arc<RingBuffer>,
    registration: &'a IngestRegistration,
    cancel: &'a CancellationToken,
    bytes_received: &'a Arc<AtomicU64>,
    ingest_metrics: &'a Arc<StageMetrics>,
    last_progress_ms: &'a Arc<AtomicU64>,
    cached_keyframe_times: &'a KeyframeTimes,
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

    fn filter_packet(
        &mut self,
        packet: &MediaPacket,
        ring_buffer: &Arc<RingBuffer>,
        registration: &IngestRegistration,
    ) -> bool {
        if !self.waiting_for_video_startup {
            return true;
        }

        match packet.media_type {
            MediaType::Audio => false,
            MediaType::Video => {
                if let Some(parameter_sets) =
                    crate::media::codec::annexb_parameter_sets(&packet.payload)
                {
                    if let Some(preview_ring) = registration.preview_ring.load_full() {
                        preview_ring.set_video_parameter_sets(parameter_sets.clone());
                    }
                    if registration.gate.state()
                        == crate::media::input_gate::InputForwardState::Active
                    {
                        ring_buffer.set_video_parameter_sets(parameter_sets);
                    }
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

pub fn use_internal_file_ingest(config: &crate::AppConfig) -> bool {
    config.use_internal_file_ingest
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
                &registration,
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
    registration: &IngestRegistration,
    cancel: CancellationToken,
) -> Result<(), String> {
    let (bytes_received, ingest_metrics, last_progress_ms, cached_keyframe_times) =
        load_ingest_runtime(&engine, &runtime_handle, registration)?;
    let pass = FileIngestPass {
        engine: &engine,
        runtime_handle: &runtime_handle,
        pipeline_id,
        ring_buffer: &ring_buffer,
        registration,
        cancel: &cancel,
        bytes_received: &bytes_received,
        ingest_metrics: &ingest_metrics,
        last_progress_ms: &last_progress_ms,
        cached_keyframe_times: &cached_keyframe_times,
    };
    let mut timestamps = LoopTimestampState::default();
    let mut switch_timestamps = crate::media::input_gate::InputTimestampMapper::default();

    loop {
        if cancel.is_cancelled() {
            break;
        }

        timestamps.begin_pass();
        run_internal_file_ingest_once(
            &pass,
            file_path,
            seek_ms,
            &mut timestamps,
            &mut switch_timestamps,
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
    registration: &IngestRegistration,
) -> Result<IngestRuntime, String> {
    runtime_handle.block_on(async {
        engine
            .with_ingest_session(registration, |ingest| {
                (
                    ingest.bytes_received.clone(),
                    ingest.metrics.clone(),
                    ingest.last_progress_ms.clone(),
                    ingest.keyframe_times.clone(),
                )
            })
            .await
            .ok_or_else(|| "File ingest session missing during startup".to_string())
    })
}

fn run_internal_file_ingest_once(
    pass: &FileIngestPass<'_>,
    file_path: &Path,
    seek_ms: Option<i64>,
    timestamps: &mut LoopTimestampState,
    switch_timestamps: &mut crate::media::input_gate::InputTimestampMapper,
) -> Result<(), String> {
    // ffmpeg-next 9 boxes this callback onto the format context (`Send + 'static`).
    let cancel = pass.cancel.clone();
    let mut ictx = format::input_with_interrupt(file_path, move || cancel.is_cancelled())
        .map_err(|e| format!("Failed to open input file: {e}"))?;
    let has_video_stream = ictx
        .streams()
        .any(|stream| stream.parameters().medium() == media::Type::Video);

    if let Some(seek_ms) = seek_ms {
        ictx.seek(seek_ms.saturating_mul(1000), ..)
            .map_err(|e| format!("Failed to seek input file: {e}"))?;
    }

    let queue = MemoryQueue::new_with_capacity(pass.engine.config.avio_capacity);
    let mut custom_output =
        CustomOutput::new(&queue, "mpegts").map_err(|e| format!("TS mux setup failed: {e}"))?;
    let octx = custom_output
        .output
        .as_mut()
        .ok_or_else(|| "Failed to acquire TS output context".to_string())?;

    let mut startup_video_state_primed = false;
    startup::prime_video_from_stream(
        pass.engine,
        pass.runtime_handle,
        pass.ring_buffer,
        pass.registration,
        &ictx,
    );
    startup::prime_container_metadata(
        pass.engine,
        pass.runtime_handle,
        pass.pipeline_id,
        pass.registration,
        &ictx,
    );

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
    let mut demux_state = DemuxPassState {
        timestamps,
        switch_timestamps,
        startup_gate: &mut startup_gate,
        probe_sent: &mut probe_sent,
    };
    drain_remuxed_ts(pass, &queue, &mut demuxer, &mut packets, &mut demux_state);

    let mut pace_anchor = None;
    for (stream, mut packet) in ictx.packets() {
        if pass.cancel.is_cancelled() {
            break;
        }

        let ist_index = stream.index();
        if !startup_video_state_primed
            && stream.parameters().medium() == media::Type::Video
            && let Some(payload) = packet.data()
        {
            startup_video_state_primed = startup::prime_video_from_packet(
                pass.engine,
                pass.runtime_handle,
                pass.ring_buffer,
                pass.registration,
                payload,
            );
        }
        let mapped_index = stream_mapping.get(ist_index).copied().unwrap_or(-1);
        if mapped_index < 0 {
            continue;
        }

        if let Some(packet_ts_ms) = packet_timestamp_ms(&stream, &packet) {
            pace_packet(pass.cancel, &mut pace_anchor, packet_ts_ms);
            if pass.cancel.is_cancelled() {
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

        drain_remuxed_ts(pass, &queue, &mut demuxer, &mut packets, &mut demux_state);
    }

    octx.write_trailer()
        .map_err(|e| format!("Failed to finalize TS mux: {e}"))?;
    drain_remuxed_ts(pass, &queue, &mut demuxer, &mut packets, &mut demux_state);

    demuxer.flush();
    let mut packet_state = DemuxPacketState {
        timestamps: &mut *demux_state.timestamps,
        switch_timestamps: &mut *demux_state.switch_timestamps,
        startup_gate: &mut *demux_state.startup_gate,
    };
    push_demuxed_packets(
        &mut demuxer,
        &mut packets,
        pass.ring_buffer,
        pass.registration,
        pass.cached_keyframe_times,
        &mut packet_state,
    );
    maybe_publish_probe(
        pass.engine,
        pass.runtime_handle,
        pass.pipeline_id,
        pass.registration,
        &mut demuxer,
        &mut *demux_state.probe_sent,
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

fn drain_remuxed_ts(
    pass: &FileIngestPass<'_>,
    queue: &MemoryQueue,
    demuxer: &mut TsDemuxer,
    packets: &mut Vec<MediaPacket>,
    state: &mut DemuxPassState<'_>,
) {
    let mut buf = [0u8; 64 * 1024];

    loop {
        let read = queue.read_nonblocking(&mut buf);
        if read == 0 {
            break;
        }

        demuxer.feed(&buf[..read]);
        let mut packet_state = DemuxPacketState {
            timestamps: &mut *state.timestamps,
            switch_timestamps: &mut *state.switch_timestamps,
            startup_gate: &mut *state.startup_gate,
        };
        push_demuxed_packets(
            demuxer,
            packets,
            pass.ring_buffer,
            pass.registration,
            pass.cached_keyframe_times,
            &mut packet_state,
        );
        maybe_publish_probe(
            pass.engine,
            pass.runtime_handle,
            pass.pipeline_id,
            pass.registration,
            demuxer,
            &mut *state.probe_sent,
        );
        pass.bytes_received
            .fetch_add(read as u64, Ordering::Relaxed);
        pass.ingest_metrics.record_in(read as u64);
        pass.last_progress_ms
            .store(MediaEngine::now_epoch_ms(), Ordering::Relaxed);
    }
}

struct DemuxPassState<'a> {
    timestamps: &'a mut LoopTimestampState,
    switch_timestamps: &'a mut crate::media::input_gate::InputTimestampMapper,
    startup_gate: &'a mut LoopStartupGate,
    probe_sent: &'a mut bool,
}

struct DemuxPacketState<'a> {
    timestamps: &'a mut LoopTimestampState,
    switch_timestamps: &'a mut crate::media::input_gate::InputTimestampMapper,
    startup_gate: &'a mut LoopStartupGate,
}

fn push_demuxed_packets(
    demuxer: &mut TsDemuxer,
    packets: &mut Vec<MediaPacket>,
    ring_buffer: &Arc<RingBuffer>,
    registration: &IngestRegistration,
    cached_keyframe_times: &KeyframeTimes,
    state: &mut DemuxPacketState<'_>,
) {
    if demuxer.drain_into(packets) == 0 {
        return;
    }

    packets.retain(|pkt| {
        state
            .startup_gate
            .filter_packet(pkt, ring_buffer, registration)
    });
    if packets.is_empty() {
        return;
    }

    for pkt in packets.iter_mut() {
        state.timestamps.apply(pkt);
    }

    if let Some(preview_ring) = registration.preview_ring.load_full() {
        preview_ring.push_batch(packets.iter().cloned());
    }
    let first_keyframe = packets
        .iter()
        .position(|packet| packet.media_type == MediaType::Video && packet.is_keyframe);
    let boundary = if first_keyframe.is_some() {
        crate::media::input_gate::InputPacketBoundary::VideoKeyframe
    } else {
        crate::media::input_gate::InputPacketBoundary::Other
    };
    let Some(lease) = registration.gate.try_enter(boundary) else {
        packets.clear();
        return;
    };
    if lease.activated()
        && let Some(first_keyframe) = first_keyframe
    {
        packets.drain(..first_keyframe);
    }
    for packet in packets.iter_mut() {
        state.switch_timestamps.map_packet(
            packet,
            lease.activated(),
            &registration.last_forwarded_dts,
        );
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

    if let Some(last) = packets.iter().max_by_key(|packet| packet.dts) {
        crate::media::input_gate::InputTimestampMapper::record_forwarded(
            last,
            &registration.last_forwarded_dts,
        );
    }
    ring_buffer.push_drained_batch_capped(packets);
}

fn maybe_publish_probe(
    engine: &Arc<MediaEngine>,
    runtime_handle: &Handle,
    pipeline_id: &str,
    registration: &IngestRegistration,
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
            .update_ingest_session_meta(pipeline_id, registration, probe.video, first_audio, None)
            .await;
        if let Some(sequence_header) = video_sequence_header {
            engine
                .cache_ingest_session_sequence_header(registration, true, sequence_header)
                .await;
        }
        engine
            .update_ingest_session_video_track_selection(
                registration,
                probe.video_track_count,
                selected_video_track_index,
            )
            .await;
        if !probe.audio_tracks.is_empty() {
            engine
                .update_ingest_session_audio_tracks(pipeline_id, registration, probe.audio_tracks)
                .await;
        }
    });
}

#[cfg(test)]
mod tests;
