use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::events::EventLog;
use crate::stage_lifecycle::{StageBackendKind, StageLifecycle, StageLifecycleGuard, StagePhase};
use crate::stage_metrics::StageMetrics;
use restream_core::metadata::AudioMeta;
use restream_core::{MediaPacket, MediaType};
use restream_domain::domain::audio_routing::AudioRouting;
use restream_domain::domain::stage::StageKey;
use restream_sync::{MEDIA_PULL_BURST_PACKETS, Reader, RingBuffer, VideoConfigFollower};

/// The stage an audio router runs as: its key, cancellation, lifecycle and
/// metrics (the stage runtime's handle) and the event log its
/// `StageStopped` goes to.
pub(crate) struct AudioRouterStage {
    pub(crate) key: StageKey,
    pub(crate) cancel: CancellationToken,
    pub(crate) lifecycle: Arc<StageLifecycle>,
    pub(crate) metrics: Arc<StageMetrics>,
    pub(crate) events: Arc<EventLog>,
}

/// Lightweight audio routing stage with no FFmpeg or MPEG-TS round-trip.
///
/// `SelectTracks` filters and reindexes reference-counted packets in a tight
/// async loop. `Remap` and `Downmix` require DSP and run on the external FFmpeg
/// backend.
pub(crate) async fn start_audio_router(
    stage: AudioRouterStage,
    routing: AudioRouting,
    input_buffer: Arc<RingBuffer>,
    output_buffer: Arc<RingBuffer>,
) {
    let AudioRouterStage {
        key: stage_key,
        cancel,
        lifecycle,
        metrics: stage_metrics,
        events,
    } = stage;
    let pipeline_id = stage_key.pipeline.to_string();
    let _lifecycle_guard = StageLifecycleGuard::new(lifecycle.clone());
    lifecycle.transition(StagePhase::BackendSpawned {
        backend: StageBackendKind::AudioRouter,
        pid: None,
    });

    // Live inputs may publish track metadata after output wiring, so seed it
    // here and refresh it in the packet loop when it arrives later.
    seed_output_metadata(&routing, &input_buffer, &output_buffer);
    log_router_start(&pipeline_id, &routing, &input_buffer, &output_buffer);

    let mut reader = Reader::new_stage_input(
        format!(
            "audio-router:{}:{:?}",
            pipeline_id,
            std::mem::discriminant(&routing)
        ),
        input_buffer,
        0,
    );
    let mut _pushed_count: u64 = 0;
    let mut first_push_logged = false;
    let mut first_input_recorded = false;
    let mut first_output_recorded = false;
    // Reuse both vectors across bursts so steady-state routing allocates no
    // packet payloads and does not grow batch storage.
    let mut out_batch: Vec<MediaPacket> = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
    let mut packets: Vec<Arc<MediaPacket>> = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
    let mut video_config = VideoConfigFollower::default();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = reader.wait_for_data() => {
                if reader.is_caught_up_to_end_of_stream() {
                    break;
                }
                if reader.pull_burst(&mut packets, MEDIA_PULL_BURST_PACKETS).is_err() {
                    continue;
                }
                // The burst is a contiguous run ending at the read position.
                let first_sequence = reader.next_sequence() - packets.len();
                if !first_input_recorded {
                    first_input_recorded = true;
                    lifecycle.record_first_input();
                }
                for (index, packet) in packets.drain(..).enumerate() {
                    stage_metrics.record_in(packet.payload.len() as u64);
                    if packet.media_type == MediaType::Video {
                        reader.follow_video_config(
                            &mut video_config,
                            first_sequence + index,
                            &output_buffer,
                            &mut out_batch,
                        );
                    }
                    if output_buffer.audio_tracks().is_none()
                        && let Some(input_tracks) = reader.audio_tracks()
                    {
                        output_buffer
                            .set_audio_tracks(apply_audio_routing(&routing, &input_tracks));
                    }
                    if let Some(output) = route_audio_packet(&routing, &packet) {
                        stage_metrics.record_out(output.payload.len() as u64);
                        if !first_push_logged {
                            log_first_push(&pipeline_id, &output, &output_buffer);
                            first_push_logged = true;
                        }
                        out_batch.push(output);
                        _pushed_count += 1;
                    }
                }
                // One write-index store and one notification for the burst.
                if !out_batch.is_empty() {
                    if !first_output_recorded {
                        first_output_recorded = true;
                        lifecycle.record_first_output();
                    }
                    output_buffer.push_drained_batch_capped(&mut out_batch);
                    lifecycle.record_producing();
                }
            }
        }
    }

    output_buffer.mark_end_of_stream();
    // A cancelled entry is never reused; the consumers' lease drops remove
    // it, and only if no newer stage took its key.
    cancel.cancel();
    events.emit(crate::events::EventKind::StageStopped {
        pipeline_id,
        encoding: stage_key.kind.to_string(),
    });
}

/// Copy codec, parameter-set and track metadata from input to output before
/// the packet loop starts. Live inputs may publish tracks later; the loop
/// refreshes them.
fn seed_output_metadata(routing: &AudioRouting, input: &RingBuffer, output: &RingBuffer) {
    let hint = input.codec_hint_str();
    if !hint.is_empty() {
        output.set_codec_hint(hint);
    }
    if let Some(parameter_sets) = input.video_parameter_sets() {
        output.set_video_parameter_sets(parameter_sets);
    }
    if let Some(input_tracks) = input.audio_tracks() {
        output.set_audio_tracks(apply_audio_routing(routing, &input_tracks));
    }
}

fn routing_mode(routing: &AudioRouting) -> &'static str {
    match routing {
        AudioRouting::Passthrough => "all",
        AudioRouting::SelectTracks { .. } => "subset",
        AudioRouting::Remap { .. } => "remap",
        AudioRouting::Downmix { .. } => "downmix",
    }
}

#[cold]
fn log_router_start(
    pipeline_id: &str,
    routing: &AudioRouting,
    input: &RingBuffer,
    output: &RingBuffer,
) {
    info!(
        "[audio-router] start pipeline={} mode={} input_codec='{}' output_codec='{}'",
        pipeline_id,
        routing_mode(routing),
        input.codec_hint_str(),
        output.codec_hint_str(),
    );
}

#[cold]
fn log_first_push(pipeline_id: &str, output: &MediaPacket, output_buffer: &RingBuffer) {
    info!(
        "[audio-router] first push pipeline={} type={:?} track={} codec_out='{}'",
        pipeline_id,
        output.media_type,
        output.track_index,
        output_buffer.codec_hint_str()
    );
}

/// Projects input track metadata through the configured routing operation.
pub fn apply_audio_routing(routing: &AudioRouting, input_tracks: &[AudioMeta]) -> Vec<AudioMeta> {
    match routing {
        AudioRouting::Passthrough => input_tracks.to_vec(),
        AudioRouting::SelectTracks { tracks } => {
            let mut output = Vec::new();
            let mut output_index = 0;
            for (input_index, track) in input_tracks.iter().enumerate() {
                if tracks.contains(&input_index) {
                    let mut output_track = track.clone();
                    output_track.track_index = output_index;
                    output.push(output_track);
                    output_index += 1;
                }
            }
            output
        }
        AudioRouting::Remap { track, .. } => {
            if let Some(input_track) = input_tracks.get(*track) {
                let mut output_track = input_track.clone();
                output_track.track_index = 0;
                vec![output_track]
            } else {
                Vec::new()
            }
        }
        AudioRouting::Downmix { track } => {
            if let Some(input_track) = input_tracks.get(*track) {
                let mut output_track = input_track.clone();
                output_track.track_index = 0;
                output_track.channels = 2;
                output_track.channel_layout = Some("stereo".to_string());
                vec![output_track]
            } else {
                Vec::new()
            }
        }
    }
}

fn route_audio_packet(routing: &AudioRouting, packet: &MediaPacket) -> Option<MediaPacket> {
    match routing {
        AudioRouting::Passthrough => Some(packet.clone()),
        AudioRouting::SelectTracks { tracks } => match packet.media_type {
            MediaType::Video => Some(packet.clone()),
            MediaType::Audio => tracks
                .iter()
                .position(|&track| track == packet.track_index as usize)
                .map(|output_index| {
                    let mut output = packet.clone();
                    output.track_index = output_index as u32;
                    output
                }),
        },
        AudioRouting::Remap { left, right, track } => match packet.media_type {
            MediaType::Video => Some(packet.clone()),
            MediaType::Audio if packet.track_index as usize == *track => {
                let _ = (left, right);
                let mut output = packet.clone();
                output.track_index = 0;
                Some(output)
            }
            MediaType::Audio => None,
        },
        AudioRouting::Downmix { .. } => Some(packet.clone()),
    }
}

#[cfg(test)]
mod tests;
