#[test]
fn external_stage_arg_preset_uses_preview_preset_not_stage_key_display() {
    let key = StageKey::new(
        "pipe-preview",
        StageKind::preview("720p", StageKind::source()),
    );
    let plan = build_ffmpeg_stage_plan(&key, None, Vec::new(), None, false)
        .expect("preview stage should produce an FFmpeg plan");

    assert_eq!(
        external_stage_arg_preset(&plan, &key.kind.to_string()),
        "720p"
    );
}

#[test]
fn external_output_stream_idx_routes_known_tracks_without_aliasing() {
    let audio_tracks = vec![
        test_audio_track(7),
        test_audio_track(2),
        test_audio_track(11),
    ];

    assert_eq!(
        external_output_stream_idx(MediaType::Video, 0, &audio_tracks, true),
        Some(0)
    );
    assert_eq!(
        external_output_stream_idx(MediaType::Audio, 7, &audio_tracks, true),
        Some(1)
    );
    assert_eq!(
        external_output_stream_idx(MediaType::Audio, 2, &audio_tracks, true),
        Some(2)
    );
    assert_eq!(
        external_output_stream_idx(MediaType::Audio, 11, &audio_tracks, true),
        Some(3)
    );
    assert_eq!(
        external_output_stream_idx(MediaType::Audio, 99, &audio_tracks, true),
        None
    );
    assert_eq!(
        external_output_stream_idx(MediaType::Audio, 7, &audio_tracks, false),
        None
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn proptest_external_output_dts_routing_preserves_per_stream_monotonicity(
        track_set in proptest::collection::btree_set(0u32..64, 1..=6),
        events in proptest::collection::vec((0u8..4, 0usize..16, -10i64..40, -10i64..40), 1..160),
    ) {
        let audio_tracks = track_set
            .into_iter()
            .map(test_audio_track)
            .collect::<Vec<_>>();
        let mut enforcer = DtsEnforcer::new(1 + audio_tracks.len());
        let mut previous_by_stream = vec![None; 1 + audio_tracks.len()];

        for (kind, index_seed, pts, dts) in events {
            let (media_type, track_index, should_route) = match kind {
                0 => (MediaType::Video, 0, true),
                1 | 2 => {
                    let track = audio_tracks[index_seed % audio_tracks.len()].track_index;
                    (MediaType::Audio, track, true)
                }
                _ => (MediaType::Audio, 10_000 + index_seed as u32, false),
            };

            let stream_idx = external_output_stream_idx(
                media_type,
                track_index,
                &audio_tracks,
                true,
            );
            prop_assert_eq!(stream_idx.is_some(), should_route);

            if let Some(stream_idx) = stream_idx {
                let (out_pts, out_dts) = enforcer.enforce(stream_idx, pts, dts);
                prop_assert!(out_pts >= out_dts);
                if let Some(previous) = previous_by_stream[stream_idx] {
                    prop_assert!(out_dts > previous);
                }
                previous_by_stream[stream_idx] = Some(out_dts);
            }
        }
    }
}

