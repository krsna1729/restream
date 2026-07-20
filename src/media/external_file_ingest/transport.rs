use std::sync::atomic::Ordering;

use tokio::io::AsyncReadExt;
use tokio::process::ChildStdout;

use crate::media::file_ingest::ContinuousTimestampState;
use crate::media::input_gate::{InputPacketBoundary, InputTimestampMapper};
use crate::media::mpegts::TsDemuxer;
use crate::media::packet::MediaType;
use crate::media::ring_buffer::MEDIA_PRODUCER_BATCH_PACKETS;

use super::ExternalFileIngestRuntime;

#[derive(Default)]
pub(super) struct FileIngestTimestamps {
    continuous: ContinuousTimestampState,
    promotion: InputTimestampMapper,
}

pub(super) async fn pump_stdout(
    runtime: &ExternalFileIngestRuntime,
    mut stdout: ChildStdout,
    timestamps: &mut FileIngestTimestamps,
) -> Result<(), String> {
    let (bytes_received, ingest_metrics, last_progress_ms, cached_keyframe_times) = runtime
        .engine
        .with_ingest_session(&runtime.registration, |ingest| {
            (
                ingest.bytes_received.clone(),
                ingest.metrics.clone(),
                ingest.last_progress_ms.clone(),
                ingest.keyframe_times.clone(),
            )
        })
        .await
        .ok_or_else(|| format!("Active ingest missing for pipeline {}", runtime.pipeline_id))?;

    let mut demuxer = TsDemuxer::new();
    let mut packets = Vec::with_capacity(MEDIA_PRODUCER_BATCH_PACKETS);
    let mut probe_sent = false;
    let mut buf = vec![0u8; 64 * 1024];

    loop {
        let read = tokio::select! {
            _ = runtime.registration.cancel_token.cancelled() => break,
            result = stdout.read(&mut buf) => result,
        }
        .map_err(|error| format!("Failed to read ffmpeg stdout: {error}"))?;

        if read == 0 {
            break;
        }

        demuxer.feed(&buf[..read]);
        if demuxer.drain_into(&mut packets) > 0 {
            for packet in &mut packets {
                timestamps.continuous.apply(packet);
            }
            if let Some(preview_ring) = runtime.registration.preview_ring.load_full() {
                preview_ring.push_batch(packets.iter().cloned());
            }

            let first_keyframe = packets
                .iter()
                .position(|packet| packet.media_type == MediaType::Video && packet.is_keyframe);
            let boundary = if first_keyframe.is_some() {
                InputPacketBoundary::VideoKeyframe
            } else {
                InputPacketBoundary::Other
            };
            if let Some(lease) = runtime.registration.gate.try_enter(boundary) {
                if lease.activated()
                    && let Some(first_keyframe) = first_keyframe
                {
                    packets.drain(..first_keyframe);
                }
                for packet in &mut packets {
                    timestamps.promotion.map_packet(
                        packet,
                        lease.activated(),
                        &runtime.registration.last_forwarded_dts,
                    );
                }
                for packet in &packets {
                    if packet.media_type == MediaType::Video
                        && let Some(parameter_sets) =
                            crate::media::codec::annexb_parameter_sets(&packet.payload)
                    {
                        runtime.ring_buffer.set_video_parameter_sets(parameter_sets);
                    }
                    if packet.media_type == MediaType::Video && packet.is_keyframe {
                        let mut times = cached_keyframe_times
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        times.push(packet.pts);
                        if times.len() > 30 {
                            times.remove(0);
                        }
                    }
                }
                if let Some(last) = packets.iter().max_by_key(|packet| packet.dts) {
                    InputTimestampMapper::record_forwarded(
                        last,
                        &runtime.registration.last_forwarded_dts,
                    );
                }
                runtime.ring_buffer.push_drained_batch_capped(&mut packets);
            } else {
                packets.clear();
            }
        }

        if !probe_sent && let Some(probe) = demuxer.take_probe() {
            probe_sent = true;
            let first_audio = probe.audio_tracks.first().cloned();
            let video_sequence_header = probe.video_sequence_header.clone();
            let selected_video_track_index = probe.video.as_ref().map(|_| 0);
            runtime
                .engine
                .update_ingest_session_meta(
                    &runtime.pipeline_id,
                    &runtime.registration,
                    probe.video,
                    first_audio,
                    None,
                )
                .await;
            if let Some(sequence_header) = video_sequence_header {
                runtime
                    .engine
                    .cache_ingest_session_sequence_header(
                        &runtime.registration,
                        true,
                        sequence_header,
                    )
                    .await;
            }
            runtime
                .engine
                .update_ingest_session_video_track_selection(
                    &runtime.registration,
                    probe.video_track_count,
                    selected_video_track_index,
                )
                .await;
            if !probe.audio_tracks.is_empty() {
                runtime
                    .engine
                    .update_ingest_session_audio_tracks(
                        &runtime.pipeline_id,
                        &runtime.registration,
                        probe.audio_tracks,
                    )
                    .await;
            }
        }

        bytes_received.fetch_add(read as u64, Ordering::Relaxed);
        ingest_metrics.record_in(read as u64);
        last_progress_ms.store(
            crate::media::engine::MediaEngine::now_epoch_ms(),
            Ordering::Relaxed,
        );
    }

    Ok(())
}
