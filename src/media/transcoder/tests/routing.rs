// --- apply_audio_routing tests ---

#[test]
fn internal_video_stage_uses_plan_preset_for_codec_qualified_stage() {
    let stage_key = StageKey::new("pipe", StageKind::video_preset_with_codec("720p", "h264"));
    let plan = FfmpegStagePlan {
        stage_key: stage_key.clone(),
        pipeline_id: "pipe".to_string(),
        input: StageInputSpec {
            codec_hint: VideoCodecKind::H264,
            video_meta: None,
            audio_tracks: Vec::new(),
        },
        video: VideoStageOp::ScalePreset {
            preset: "720p".to_string(),
        },
        audio: crate::media::ffmpeg::stage_plan::AudioStageOp::Passthrough,
        output_codec: VideoCodecKind::H264,
        output_profile: None,
        include_audio: true,
        startup: Default::default(),
        timeline: Default::default(),
    };

    assert_eq!(
        internal_video_stage_preset_name(&plan, &stage_key.kind),
        "720p"
    );
}

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
