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
/// async loop. `Remap` and `Downmix` require DSP and run on the external FFmpeg
/// backend.
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
mod tests {
    use super::*;
    use crate::media::packet::PayloadFormat;
    use proptest::prelude::*;

    // --- apply_audio_routing tests ---

    #[test]
    fn apply_routing_passthrough_preserves_all_tracks() {
        let tracks = vec![
            AudioMeta {
                codec: "aac".into(),
                sample_rate: 48000,
                channels: 2,
                channel_layout: None,
                track_index: 0,
                pid: Some(0x101),
                language: Some("eng".to_string()),
                title: None,
                profile: None,
            },
            AudioMeta {
                codec: "aac".into(),
                sample_rate: 44100,
                channels: 1,
                channel_layout: None,
                track_index: 1,
                pid: Some(0x102),
                language: Some("spa".to_string()),
                title: None,
                profile: None,
            },
        ];
        let result = apply_audio_routing(&AudioRouting::Passthrough, &tracks);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].track_index, 0);
        assert_eq!(result[1].track_index, 1);
    }

    #[test]
    fn apply_routing_select_tracks_filters_and_reindexes() {
        let tracks = vec![
            AudioMeta {
                codec: "aac".into(),
                sample_rate: 48000,
                channels: 2,
                channel_layout: None,
                track_index: 0,
                pid: Some(0x101),
                language: Some("eng".to_string()),
                title: None,
                profile: None,
            },
            AudioMeta {
                codec: "aac".into(),
                sample_rate: 44100,
                channels: 1,
                channel_layout: None,
                track_index: 1,
                pid: Some(0x102),
                language: Some("spa".to_string()),
                title: None,
                profile: None,
            },
            AudioMeta {
                codec: "aac".into(),
                sample_rate: 32000,
                channels: 1,
                channel_layout: None,
                track_index: 2,
                pid: Some(0x103),
                language: None,
                title: None,
                profile: None,
            },
        ];
        // Select tracks 0 and 2
        let routing = AudioRouting::SelectTracks { tracks: vec![0, 2] };
        let result = apply_audio_routing(&routing, &tracks);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].track_index, 0); // re-indexed: track 0 → index 0
        assert_eq!(result[1].track_index, 1); // re-indexed: track 2 → index 1
        assert_eq!(result[0].sample_rate, 48000);
        assert_eq!(result[1].sample_rate, 32000);
    }

    /// Verify that stage keys for different video presets with the same audio
    /// routing produce different cache keys, preventing cross-contamination.
    /// See docs/media-pipeline.md "Audio Stage Cache Concern".
    #[test]
    fn stage_keys_isolate_video_presets() {
        use crate::planner::EncodingStagePlan;

        let plan_720 = EncodingStagePlan::from_encoding("pipe1", "720p+atrack:0");
        let plan_1080 = EncodingStagePlan::from_encoding("pipe1", "1080p+atrack:0");

        let audio_720 = plan_720.audio_stage().unwrap();
        let audio_1080 = plan_1080.audio_stage().unwrap();
        assert_ne!(
            audio_720, audio_1080,
            "audio stages with different video upstreams must have different keys"
        );

        let plan_720_dup = EncodingStagePlan::from_encoding("pipe1", "720p+atrack:0");
        assert_eq!(audio_720, plan_720_dup.audio_stage().unwrap());
    }

    /// Verify video stage keys are shared across outputs with different audio routing.
    #[test]
    fn video_stage_shared_across_audio_variants() {
        use crate::domain::stage::StageKind;
        use crate::planner::EncodingStagePlan;
        let expected = StageKind::video_preset("720p");
        for encoding in &["720p", "720p+atrack:0", "720p+remap:0:1"] {
            let plan = EncodingStagePlan::from_encoding("pipe1", encoding);
            let video = plan.video_stage().unwrap();
            assert_eq!(video.kind, expected, "encoding={}", encoding);
        }
    }

    #[test]
    fn test_apply_audio_routing_reindexes() {
        let input_tracks = vec![
            AudioMeta {
                codec: "aac".to_string(),
                channels: 2,
                sample_rate: 48000,
                track_index: 0,
                channel_layout: None,
                pid: Some(0x101),
                language: Some("eng".to_string()),
                title: None,
                profile: None,
            },
            AudioMeta {
                codec: "aac".to_string(),
                channels: 2,
                sample_rate: 48000,
                track_index: 1,
                channel_layout: None,
                pid: Some(0x102),
                language: Some("spa".to_string()),
                title: None,
                profile: None,
            },
            AudioMeta {
                codec: "aac".to_string(),
                channels: 2,
                sample_rate: 48000,
                track_index: 2,
                channel_layout: None,
                pid: Some(0x103),
                language: None,
                title: None,
                profile: None,
            },
        ];

        let routing = AudioRouting::SelectTracks { tracks: vec![2] };
        let output_tracks = apply_audio_routing(&routing, &input_tracks);
        assert_eq!(output_tracks.len(), 1);
        assert_eq!(output_tracks[0].track_index, 0); // re-indexed from 2 to 0
    }

    #[test]
    fn apply_routing_remap_selects_and_zeroes_track_index() {
        let tracks = vec![generated_audio_track(0), generated_audio_track(1)];
        let routing = AudioRouting::Remap {
            track: 1,
            left: 0,
            right: 1,
        };
        let result = apply_audio_routing(&routing, &tracks);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].track_index, 0);
        assert_eq!(result[0].pid, Some(0x101));
    }

    #[test]
    fn apply_routing_remap_out_of_range_track_yields_no_tracks() {
        let tracks = vec![generated_audio_track(0)];
        let routing = AudioRouting::Remap {
            track: 5,
            left: 0,
            right: 1,
        };
        assert!(apply_audio_routing(&routing, &tracks).is_empty());
    }

    #[test]
    fn apply_routing_downmix_selects_track_and_forces_stereo() {
        let mut mono_track = generated_audio_track(0);
        mono_track.channels = 1;
        mono_track.channel_layout = Some("mono".to_string());
        let tracks = vec![mono_track, generated_audio_track(1)];
        let routing = AudioRouting::Downmix { track: 1 };
        let result = apply_audio_routing(&routing, &tracks);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].track_index, 0);
        assert_eq!(result[0].channels, 2);
        assert_eq!(result[0].channel_layout, Some("stereo".to_string()));
    }

    #[test]
    fn apply_routing_downmix_out_of_range_track_yields_no_tracks() {
        let tracks = vec![generated_audio_track(0)];
        let routing = AudioRouting::Downmix { track: 3 };
        assert!(apply_audio_routing(&routing, &tracks).is_empty());
    }

    #[test]
    fn route_audio_packet_remap_matches_configured_track_and_zeroes_index() {
        let routing = AudioRouting::Remap {
            track: 2,
            left: 0,
            right: 1,
        };
        let pkt = generated_router_packet(false, 2, 42);
        let routed = route_audio_packet(&routing, &pkt).expect("matching track must route");
        assert_eq!(routed.track_index, 0);
        assert_eq!(routed.pts, 42);
    }

    #[test]
    fn route_audio_packet_remap_drops_non_matching_audio_track() {
        let routing = AudioRouting::Remap {
            track: 2,
            left: 0,
            right: 1,
        };
        let pkt = generated_router_packet(false, 0, 42);
        assert!(route_audio_packet(&routing, &pkt).is_none());
    }

    #[test]
    fn route_audio_packet_remap_passes_video_through_untouched() {
        let routing = AudioRouting::Remap {
            track: 2,
            left: 0,
            right: 1,
        };
        let pkt = generated_router_packet(true, 7, 42);
        let routed = route_audio_packet(&routing, &pkt).expect("video always passes through");
        assert_eq!(routed.track_index, 7);
    }

    #[test]
    fn route_audio_packet_downmix_always_passes_the_packet_through() {
        let routing = AudioRouting::Downmix { track: 1 };
        let audio_pkt = generated_router_packet(false, 1, 10);
        let video_pkt = generated_router_packet(true, 0, 10);
        assert!(route_audio_packet(&routing, &audio_pkt).is_some());
        assert!(route_audio_packet(&routing, &video_pkt).is_some());
    }

    fn generated_audio_track(index: usize) -> AudioMeta {
        AudioMeta {
            codec: "aac".to_string(),
            channels: 2,
            sample_rate: 48_000,
            track_index: index as u32,
            channel_layout: None,
            pid: Some(0x100 + index as u16),
            language: None,
            title: None,
            profile: None,
        }
    }

    fn generated_router_packet(is_video: bool, track_index: u32, pts: i64) -> MediaPacket {
        MediaPacket {
            media_type: if is_video {
                MediaType::Video
            } else {
                MediaType::Audio
            },
            track_index,
            pts,
            dts: pts,
            is_keyframe: is_video,
            format: PayloadFormat::Raw,
            payload: bytes::Bytes::from_static(&[0x01, 0x02, 0x03]),
        }
    }

    proptest! {
        #[test]
        fn audio_router_select_tracks_preserves_video_and_reindexes_selected_audio(
            selected_mask in prop::array::uniform4(any::<bool>()),
            packets in prop::collection::vec((any::<bool>(), 0_u32..4, 0_i64..10_000), 1..64),
        ) {
            let selected_tracks = selected_mask
                .iter()
                .enumerate()
                .filter_map(|(index, selected)| selected.then_some(index))
                .collect::<Vec<_>>();
            let routing = AudioRouting::SelectTracks {
                tracks: selected_tracks.clone(),
            };
            let input_tracks = (0..4).map(generated_audio_track).collect::<Vec<_>>();
            let output_tracks = apply_audio_routing(&routing, &input_tracks);

            prop_assert_eq!(output_tracks.len(), selected_tracks.len());
            for (output_index, source_index) in selected_tracks.iter().enumerate() {
                prop_assert_eq!(output_tracks[output_index].track_index, output_index as u32);
                prop_assert_eq!(output_tracks[output_index].pid, Some(0x100 + *source_index as u16));
            }

            let mut routed_packets = Vec::new();
            for (is_video, track_index, pts) in packets {
                let input = generated_router_packet(is_video, track_index, pts);
                if let Some(output) = route_audio_packet(&routing, &input) {
                    if is_video {
                        prop_assert_eq!(output.media_type, MediaType::Video);
                        prop_assert_eq!(output.track_index, track_index);
                    } else {
                        let expected_index = selected_tracks
                            .iter()
                            .position(|track| *track == track_index as usize)
                            .expect("only selected audio packets should be routed");
                        prop_assert_eq!(output.media_type, MediaType::Audio);
                        prop_assert_eq!(output.track_index, expected_index as u32);
                    }
                    prop_assert_eq!(output.pts, input.pts);
                    prop_assert_eq!(output.dts, input.dts);
                    prop_assert_eq!(&output.payload, &input.payload);
                    routed_packets.push(output);
                } else {
                    prop_assert!(!is_video, "video packets must always pass through");
                    prop_assert!(
                        !selected_tracks.contains(&(track_index as usize)),
                        "selected audio track {track_index} was dropped"
                    );
                }
            }

            prop_assert!(
                routed_packets.iter().all(|packet| {
                    packet.media_type == MediaType::Video
                        || (packet.track_index as usize) < selected_tracks.len()
                }),
                "routed audio packets must be re-indexed into the selected output track range"
            );
        }
    }
}
