use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::domain::audio_routing::AudioRouting;
use crate::domain::stage::StageKey;
use crate::media::metadata::AudioMeta;
use crate::media::packet::{MediaPacket, MediaType};
use crate::media::ring_buffer::{MEDIA_PULL_BURST_PACKETS, Reader, RingBuffer};

/// Lightweight audio routing stage with no FFmpeg or MPEG-TS round-trip.
///
/// `SelectTracks` filters and reindexes reference-counted packets in a tight
/// async loop. `Remap` and `Downmix` require DSP and are assigned to the FFmpeg
/// backend by `BackendPolicy`.
pub async fn start_audio_router(
    pipeline_id: String,
    routing: AudioRouting,
    input_buffer: Arc<RingBuffer>,
    output_buffer: Arc<RingBuffer>,
    engine: Arc<crate::media::engine::MediaEngine>,
    cancel: CancellationToken,
    stage_key: StageKey,
) {
    let stage_metrics = engine.get_or_create_stage_metrics(stage_key.clone()).await;
    let lifecycle = engine
        .get_or_create_stage_lifecycle(
            stage_key.clone(),
            crate::media::stage_lifecycle::StagePhase::Registered,
        )
        .await;
    let _lifecycle_guard =
        crate::media::stage_lifecycle::StageLifecycleGuard::new(lifecycle.clone());
    lifecycle.transition(crate::media::stage_lifecycle::StagePhase::BackendSpawned {
        backend: crate::media::stage_lifecycle::StageBackendKind::AudioRouter,
        pid: None,
    });

    let hint = input_buffer.codec_hint_str();
    if !hint.is_empty() {
        output_buffer.set_codec_hint(hint);
    }
    if let Some(parameter_sets) = input_buffer.video_parameter_sets() {
        output_buffer.set_video_parameter_sets(parameter_sets);
    }
    // Live inputs may publish track metadata after output wiring, so seed it
    // here and refresh it in the packet loop when it arrives later.
    if let Some(input_tracks) = input_buffer.audio_tracks() {
        output_buffer.set_audio_tracks(apply_audio_routing(&routing, &input_tracks));
    }

    let routing_mode = match &routing {
        AudioRouting::Passthrough => "all",
        AudioRouting::SelectTracks { .. } => "subset",
        AudioRouting::Remap { .. } => "remap",
        AudioRouting::Downmix { .. } => "downmix",
    };
    info!(
        "[audio-router] start pipeline={} mode={} input_codec='{}' output_codec='{}'",
        pipeline_id,
        routing_mode,
        input_buffer.codec_hint_str(),
        output_buffer.codec_hint_str(),
    );

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
                if !first_input_recorded {
                    first_input_recorded = true;
                    lifecycle.record_first_input();
                }
                for packet in packets.drain(..) {
                    stage_metrics.record_in(packet.payload.len() as u64);
                    if packet.media_type == MediaType::Video
                        && output_buffer.video_parameter_sets().is_none()
                    {
                        if let Some(parameter_sets) = reader.current_ring().video_parameter_sets() {
                            output_buffer.set_video_parameter_sets(parameter_sets);
                        } else if let Some(parameter_sets) =
                            crate::media::codec::annexb_parameter_sets(&packet.payload)
                        {
                            output_buffer.set_video_parameter_sets(parameter_sets);
                        }
                    }
                    if output_buffer.audio_tracks().is_none()
                        && let Some(input_tracks) = reader.current_ring().audio_tracks()
                    {
                        output_buffer
                            .set_audio_tracks(apply_audio_routing(&routing, &input_tracks));
                    }
                    if let Some(output) = route_audio_packet(&routing, &packet) {
                        stage_metrics.record_out(output.payload.len() as u64);
                        if !first_push_logged {
                            info!(
                                "[audio-router] first push pipeline={} type={:?} track={} codec_out='{}'",
                                pipeline_id,
                                output.media_type,
                                output.track_index,
                                output_buffer.codec_hint_str()
                            );
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
    engine.remove_stage_metrics(&stage_key).await;
    engine.remove_stage_lifecycle(&stage_key).await;
    engine.remove_stage_runtime(&stage_key).await;
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStopped {
            pipeline_id,
            encoding: stage_key.kind.to_string(),
        });
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

pub(super) fn route_audio_packet(
    routing: &AudioRouting,
    packet: &MediaPacket,
) -> Option<MediaPacket> {
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
