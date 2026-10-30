use super::*;
use proptest::prelude::*;
use restream_core::PayloadFormat;
use restream_core::metadata::AudioMeta;
use restream_sync::Reader;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

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

#[tokio::test]
async fn test_audio_router_reindexes_packets() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "pipe-id",
        StageKind::audio_route("atrack:2", StageKind::source()),
    );

    // Start audio router
    let routing = AudioRouting::SelectTracks { tracks: vec![2] };
    let handle = tokio::spawn(start_audio_router(
        "pipe-id".to_string(),
        routing,
        source_ring.clone(),
        out_ring.clone(),
        engine,
        cancel.clone(),
        stage_key,
    ));

    // Push some source packets
    source_ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[1, 2, 3]),
    });
    source_ring.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index: 2, // track 2
        pts: 10,
        dts: 10,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[4, 5, 6]),
    });

    // Let the router process
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    cancel.cancel();
    let _ = handle.await;

    // Verify output packets
    let mut reader = Reader::new("test_router".to_string(), out_ring);
    let mut out_pkts = Vec::new();
    while let Ok(Some(pkt)) = reader.pull() {
        out_pkts.push(pkt);
    }

    // Should contain video packet and audio packet
    assert_eq!(out_pkts.len(), 2);
    assert_eq!(out_pkts[0].media_type, MediaType::Video);
    assert_eq!(out_pkts[1].media_type, MediaType::Audio);
    assert_eq!(out_pkts[1].track_index, 0); // re-indexed to 0
}

#[tokio::test]
async fn audio_router_stage_is_not_blocking_after_output() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "audio-router-producing",
        StageKind::audio_route("atrack:0", StageKind::source()),
    );

    let handle = tokio::spawn(start_audio_router(
        "audio-router-producing".to_string(),
        AudioRouting::SelectTracks { tracks: vec![0] },
        source_ring.clone(),
        out_ring.clone(),
        engine.clone(),
        cancel.clone(),
        stage_key.clone(),
    ));

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while source_ring.active_reader_count() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("audio router input reader should attach");

    source_ring.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[4, 5, 6]),
    });

    let mut output_reader = Reader::new("audio-router-producing-output".to_string(), out_ring);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if matches!(output_reader.pull(), Ok(Some(_))) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("audio router should emit output");

    assert!(
        engine
            .egress_blocked_by_stage_snapshot(&stage_key)
            .await
            .is_none(),
        "a stage that has emitted output must not surface as blockedBy"
    );

    cancel.cancel();
    handle.await.expect("audio router task should not panic");
}

#[tokio::test]
async fn audio_router_replays_prebuffered_upstream_stage() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "prebuffered-audio-route",
        StageKind::audio_route(
            "atrack:1",
            StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("720p")),
        ),
    );

    source_ring.set_codec_hint("h264");
    source_ring.set_audio_tracks(vec![
        AudioMeta {
            codec: "aac".to_string(),
            channels: 2,
            sample_rate: 48000,
            track_index: 0,
            channel_layout: None,
            pid: Some(0x101),
            language: None,
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
            language: None,
            title: None,
            profile: None,
        },
    ]);
    source_ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[1, 2, 3]),
    });
    source_ring.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts: 10,
        dts: 10,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[4, 5, 6]),
    });
    source_ring.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index: 1,
        pts: 20,
        dts: 20,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[7, 8, 9]),
    });
    source_ring.mark_end_of_stream();

    let handle = tokio::spawn(start_audio_router(
        "prebuffered-audio-route".to_string(),
        AudioRouting::SelectTracks { tracks: vec![1] },
        source_ring,
        out_ring.clone(),
        engine,
        cancel,
        stage_key,
    ));
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("audio router should finish after upstream EOS")
        .expect("audio router task should not panic");

    let mut reader = Reader::new("prebuffered-router-output".to_string(), out_ring);
    let mut out_pkts = Vec::new();
    while let Ok(Some(pkt)) = reader.pull() {
        out_pkts.push(pkt);
    }

    assert!(
        out_pkts
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "late audio router should replay prebuffered video"
    );
    assert!(
        out_pkts
            .iter()
            .any(|packet| { packet.media_type == MediaType::Audio && packet.track_index == 0 }),
        "late audio router should replay and re-index the selected audio track"
    );
}

#[tokio::test]
async fn audio_router_propagates_late_arriving_audio_tracks() {
    // Simulates SRT multi-audio: source ring has no audio_tracks when the
    // audio_router stage starts, then they arrive mid-stream.
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "late-tracks",
        StageKind::audio_route("atrack:0,1", StageKind::source()),
    );

    source_ring.set_codec_hint("h264");
    // NOTE: no set_audio_tracks() yet — simulates live SRT before probe

    let handle = tokio::spawn(start_audio_router(
        "late-tracks".to_string(),
        AudioRouting::SelectTracks { tracks: vec![0, 1] },
        source_ring.clone(),
        out_ring.clone(),
        engine,
        cancel.clone(),
        stage_key,
    ));

    // Output ring has no audio_tracks yet (source not probed)
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(
        out_ring.audio_tracks().is_none(),
        "output ring should not have tracks before source has them"
    );

    // SRT ingest probe completes — audio_tracks become available
    source_ring.set_audio_tracks(vec![
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: Some("stereo".to_string()),
            track_index: 0,
            pid: Some(0x100),
            language: Some("eng".to_string()),
            title: None,
            profile: None,
        },
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: Some("stereo".to_string()),
            track_index: 1,
            pid: Some(0x101),
            language: Some("fra".to_string()),
            title: None,
            profile: None,
        },
    ]);

    // Push an audio packet — router burst loop should propagate audio_tracks
    source_ring.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: false,
        payload: bytes::Bytes::from_static(&[0x01]),
        format: restream_core::PayloadFormat::Raw,
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let tracks = out_ring
        .audio_tracks()
        .expect("output ring should have audio_tracks after source ring received them");
    assert_eq!(
        tracks.len(),
        2,
        "SelectTracks [0,1] should propagate both tracks"
    );
    assert_eq!(tracks[0].track_index, 0);
    assert_eq!(tracks[1].track_index, 1);

    cancel.cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn audio_tracks_ring_supports_reconnect_update() {
    // Verifies that ArcSwapOption allows updating audio_tracks across
    // publisher reconnects — a reconnected stream may have different tracks.
    let ring = Arc::new(RingBuffer::new(8));

    // First publisher: 2 audio tracks
    ring.set_audio_tracks(vec![
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: 0,
            pid: None,
            language: None,
            title: None,
            profile: None,
        },
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: 1,
            pid: None,
            language: None,
            title: None,
            profile: None,
        },
    ]);
    assert_eq!(
        ring.audio_tracks().unwrap().len(),
        2,
        "first publisher: 2 tracks"
    );

    // Publisher reconnects — RTMP clears with empty vec, then re-probes
    ring.set_audio_tracks(Vec::new());
    assert!(
        ring.audio_tracks().is_none(),
        "empty set_audio_tracks should clear metadata (not a no-op)"
    );

    // Re-probe with new single-track configuration
    ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 44100,
        channels: 1,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    }]);
    let tracks = ring.audio_tracks().unwrap();
    assert_eq!(tracks.len(), 1, "reconnected publisher: 1 track");
    assert_eq!(
        tracks[0].sample_rate, 44100,
        "reconnected publisher track data"
    );
}

#[tokio::test]
async fn audio_router_preserves_upstream_video_parameter_sets() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "pipe-video-params",
        StageKind::audio_route("atrack:0", StageKind::source()),
    );

    source_ring.set_codec_hint("h264");
    source_ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68, 0xCE,
        0x38, 0x80,
    ]);

    let handle = tokio::spawn(start_audio_router(
        "pipe-video-params".to_string(),
        AudioRouting::SelectTracks { tracks: vec![0] },
        source_ring,
        out_ring.clone(),
        engine,
        cancel.clone(),
        stage_key,
    ));

    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert_eq!(out_ring.codec_hint_str(), "h264");
    assert_eq!(
        out_ring.video_parameter_sets(),
        Some(vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
            0xCE, 0x38, 0x80,
        ])
    );

    cancel.cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn audio_router_learns_video_parameter_sets_from_live_packets() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "pipe-video-params-live",
        StageKind::audio_route("atrack:0", StageKind::source()),
    );

    source_ring.set_codec_hint("h264");

    let handle = tokio::spawn(start_audio_router(
        "pipe-video-params-live".to_string(),
        AudioRouting::SelectTracks { tracks: vec![0] },
        source_ring.clone(),
        out_ring.clone(),
        engine,
        cancel.clone(),
        stage_key,
    ));

    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(
        out_ring.video_parameter_sets().is_none(),
        "router should start without cached parameter sets when the upstream ring does not have them yet"
    );

    // The producer contract: the upstream producer records the in-band
    // parameter sets it publishes.
    source_ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x40, 0x28, 0x02, 0xDD, 0x80, 0x00,
        0x00, 0x00, 0x01, 0x68, 0xCE, 0x38, 0x80,
    ]);
    source_ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x40, 0x28, 0x02, 0xDD, 0x80,
            0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x38, 0x80, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88,
            0x84, 0x00,
        ]),
    });

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        if out_ring.video_parameter_sets().is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "router should cache parameter sets from the first live video packet"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    cancel.cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn audio_router_copies_late_upstream_parameter_sets_without_inband_headers() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "pipe-video-params-cache",
        StageKind::audio_route("atrack:0", StageKind::source()),
    );

    source_ring.set_codec_hint("h264");

    let handle = tokio::spawn(start_audio_router(
        "pipe-video-params-cache".to_string(),
        AudioRouting::SelectTracks { tracks: vec![0] },
        source_ring.clone(),
        out_ring.clone(),
        engine,
        cancel.clone(),
        stage_key,
    ));

    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    source_ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68, 0xCE,
        0x38, 0x80,
    ]);
    source_ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x00]),
    });

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        if out_ring.video_parameter_sets().is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "router should copy late upstream parameter sets even when the live packet payload lacks them"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    cancel.cancel();
    let _ = handle.await;
}

/// A reader of the router's output ring sees each video packet with the
/// configuration the source published it with, also when the source changes
/// it in the middle of one burst.
#[tokio::test]
async fn audio_router_copies_a_configuration_change_inside_one_burst() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    const SPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80];
    const PPS_A: &[u8] = &[0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2];
    const PPS_B: &[u8] = &[0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80];
    const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88];
    const P_SLICE: &[u8] = &[0, 0, 0, 1, 0x41, 0x9a];
    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(start_audio_router(
        "pipe-video-config-burst".to_string(),
        AudioRouting::SelectTracks { tracks: vec![0] },
        source_ring.clone(),
        out_ring.clone(),
        Arc::new(MediaEngine::new()),
        cancel.clone(),
        StageKey::new(
            "pipe-video-config-burst",
            StageKind::audio_route("atrack:0", StageKind::source()),
        ),
    ));
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;

    let video = |dts, is_keyframe, parts: &[&[u8]]| MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: dts,
        dts,
        is_keyframe,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from(parts.concat()),
    };
    // One publication (one write-index store), so the router pulls all four
    // in one burst; the third packet starts configuration B.
    source_ring.push_drained_batch_recording_config(&mut vec![
        video(0, true, &[SPS, PPS_A, IDR]),
        video(40, false, &[P_SLICE]),
        video(80, true, &[SPS, PPS_B, IDR]),
        video(120, false, &[P_SLICE]),
    ]);

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    while out_ring.get_write_idx() < 4 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "router did not forward"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    cancel.cancel();
    let _ = handle.await;

    let configs: Vec<Vec<u8>> = (0..4)
        .map(|sequence| {
            out_ring
                .track_config_at(sequence, MediaType::Video, 0)
                .expect("retained")
                .expect("configured")
                .bytes
                .to_vec()
        })
        .collect();
    let a = [SPS, PPS_A].concat();
    let b = [SPS, PPS_B].concat();
    assert_eq!(configs, [a.clone(), a, b.clone(), b]);
}
