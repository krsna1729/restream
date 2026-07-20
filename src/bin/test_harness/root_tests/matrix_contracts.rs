use super::super::*;

#[test]
fn mixed_input_matrix_keeps_rtmp_ingest_single_h264_only() {
    let rtmp_cases: Vec<_> = mixed_input_cases()
        .iter()
        .filter(|case| case.protocol() == MixedInputProtocol::Rtmp)
        .collect();
    assert_eq!(rtmp_cases.len(), 2);
    assert_eq!(rtmp_cases[0].scenario_id(), "mixed.live.rtmp.h264.a1.bf0");
    assert_eq!(rtmp_cases[1].scenario_id(), "mixed.live.rtmp.h264.a1.bf2");
    assert!(
        rtmp_cases
            .iter()
            .all(|case| matches!(case.codec(), MixedVideoCodec::H264))
    );
    assert!(rtmp_cases.iter().all(|case| !case.is_multi_track()));
    assert!(
        rtmp_cases
            .iter()
            .any(|case| matches!(case.reorder(), MixedInputReorder::Bf0))
    );
    assert!(
        rtmp_cases
            .iter()
            .any(|case| matches!(case.reorder(), MixedInputReorder::Bf2))
    );
}

#[test]
fn mixed_input_matrix_covers_bf0_and_bf2_for_every_supported_shape() {
    let mut grouped = HashMap::new();
    for case in mixed_input_cases() {
        grouped
            .entry((case.protocol(), case.codec(), case.audio_layout()))
            .or_insert_with(Vec::new)
            .push(case.reorder());
    }

    for ((protocol, codec, audio_layout), reorders) in grouped {
        assert!(
            reorders.contains(&MixedInputReorder::Bf0),
            "missing bf0 row for {:?}/{:?}/{:?}",
            protocol,
            codec,
            audio_layout
        );
        assert!(
            reorders.contains(&MixedInputReorder::Bf2),
            "missing bf2 row for {:?}/{:?}/{:?}",
            protocol,
            codec,
            audio_layout
        );
    }
}

#[test]
fn mixed_hls_preview_expectations_match_current_hevc_preview_contract() {
    for case in mixed_input_cases() {
        let expected = case.hls_preview_expected_dimensions();
        if matches!(case.codec(), MixedVideoCodec::H265) {
            assert_eq!(
                expected,
                "1280x720",
                "{} should assert HEVC preview transcode dimensions",
                case.scenario_id()
            );
        } else {
            assert_eq!(
                expected,
                "1920x1080",
                "{} should assert source-size H.264 preview",
                case.scenario_id()
            );
        }
    }
}

#[test]
fn mixed_input_recording_expectations_follow_source_tracks() {
    for case in mixed_input_cases() {
        assert_eq!(
            case.expected_audio_tracks(),
            if case.is_multi_track() { 2 } else { 1 },
            "{} should record one assertion row with the source audio-track count",
            case.scenario_id()
        );
        assert_eq!(
            case.expected_video_codec(),
            if matches!(case.codec(), MixedVideoCodec::H265) {
                "hevc"
            } else {
                "h264"
            },
            "{} should record the source video codec",
            case.scenario_id()
        );
    }
}

#[test]
fn mixed_input_rows_select_their_output_matrix() {
    for case in mixed_input_cases() {
        let plan = MixedScenarioPlan::for_input(*case);
        let cases = plan.outputs;
        assert_eq!(plan.source.adapter, MixedSourceAdapter::for_input(*case));
        assert_eq!(plan.expected_stages, expected_mixed_stage_count(*case));
        if case.is_multi_track() {
            assert_eq!(
                cases.len(),
                multi_track_mixed_output_cases().len(),
                "{} should exercise the multi-audio output matrix",
                case.scenario_id()
            );
            assert!(cases.iter().any(|case| case.expected_audio_tracks() == 2));
        } else {
            assert_eq!(
                cases.len(),
                single_track_mixed_output_cases().len(),
                "{} should exercise the single-track output matrix",
                case.scenario_id()
            );
            assert!(cases.iter().all(|case| case.expected_audio_tracks() == 1));
        }
    }
}

#[test]
fn mixed_scenario_plan_expands_without_signal_cost() {
    let plans: Vec<_> = mixed_input_cases()
        .iter()
        .copied()
        .map(MixedScenarioPlan::for_input)
        .collect();

    assert_eq!(plans.len(), 18);
    assert_eq!(
        plans.iter().map(|plan| plan.output_cells()).sum::<usize>(),
        232
    );
    assert_eq!(
        plans
            .iter()
            .filter(|plan| plan.source.adapter == MixedSourceAdapter::FileIngest)
            .count(),
        8
    );
    assert_eq!(
        plans
            .iter()
            .filter(|plan| plan.source.adapter == MixedSourceAdapter::RtmpPublisher)
            .count(),
        2
    );
    assert_eq!(
        plans
            .iter()
            .filter(|plan| plan.source.adapter == MixedSourceAdapter::SrtPublisher)
            .count(),
        8
    );

    let check_names: Vec<_> = mixed_default_checks()
        .iter()
        .map(|check| check.as_str())
        .collect();
    assert_eq!(
        check_names,
        vec![
            "ffprobe",
            "audio-route",
            "decode-scan",
            "runtime-log",
            "stage-sharing",
            "hls",
            "recording",
            "load",
            "smoke",
            "lifecycle",
            "sink-probe",
            "hls-put-probe",
            "burst-graph",
        ]
    );
}

#[test]
fn mixed_json_dsl_carries_current_matrix_contract() {
    let manifest = mixed_dsl_manifest().expect("mixed DSL manifest should parse");
    assert_eq!(manifest.version, 1);
    assert_eq!(manifest.input_cases().unwrap(), mixed_input_cases());

    let dsl_fast: Vec<_> = manifest
        .mixed
        .fast_breadth
        .iter()
        .map(|row| (row.id, row.rationale.as_str(), row.check_specs().unwrap()))
        .collect();
    let rust_fast: Vec<_> = mixed_fast_breadth_cases()
        .iter()
        .map(|row| {
            (
                row.case.scenario_id(),
                row.rationale.as_str(),
                row.checks.to_vec(),
            )
        })
        .collect();
    assert_eq!(dsl_fast, rust_fast);

    let dsl_batches: Vec<_> = manifest
        .mixed
        .fast_breadth_batches
        .iter()
        .map(|batch| {
            (
                MixedSharedBatchGroup::from_str(batch.group).unwrap(),
                batch
                    .cases
                    .iter()
                    .map(|id| mixed_input_case_for_command(id).unwrap())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let rust_batches: Vec<_> = mixed_fast_breadth_batches()
        .iter()
        .map(|batch| (batch.group, batch.cases.to_vec()))
        .collect();
    assert_eq!(dsl_batches, rust_batches);

    let dsl_signal: Vec<_> = manifest
        .mixed
        .signal_sentinels
        .iter()
        .map(|row| (row.id, row.rationale.as_str(), row.check_specs().unwrap()))
        .collect();
    let rust_signal: Vec<_> = mixed_signal_sentinels()
        .iter()
        .map(|row| {
            (
                row.case.scenario_id(),
                row.rationale.as_str(),
                row.checks.to_vec(),
            )
        })
        .collect();
    assert_eq!(dsl_signal, rust_signal);

    let dsl_signal_batches: Vec<_> = manifest
        .mixed
        .signal_batches
        .iter()
        .map(|batch| {
            (
                MixedSharedBatchGroup::from_str(batch.group).unwrap(),
                batch
                    .cases
                    .iter()
                    .map(|id| mixed_input_case_for_command(id).unwrap())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let rust_signal_batches: Vec<_> = mixed_signal_batches()
        .iter()
        .map(|batch| (batch.group, batch.cases.to_vec()))
        .collect();
    assert_eq!(dsl_signal_batches, rust_signal_batches);
}

#[test]
fn fault_json_dsl_carries_current_case_contract() {
    assert_eq!(
        publisher_disconnect_cases()
            .iter()
            .map(|case| case.test_name.as_str())
            .collect::<Vec<_>>(),
        ["rtmp-publisher-disconnect", "srt-publisher-disconnect"]
    );

    assert_eq!(
        retry_budget_cases()
            .iter()
            .map(|case| (
                case.test_name.as_str(),
                case.protocol.ffmpeg_format(),
                case.dead_sink_offset
            ))
            .collect::<Vec<_>>(),
        [
            ("rtmp-egress-retry-budget-exhausts", "flv", 77),
            ("srt-egress-retry-budget-exhausts", "mpegts", 78),
        ]
    );

    assert_eq!(
        recovery_transient_cases()
            .iter()
            .map(|case| (
                case.test_name.as_str(),
                case.protocol.ffmpeg_format(),
                case.wait_input_off_after_drop,
                case.require_media_ready_on_resume,
                case.second_reconnect_checks_flapping,
            ))
            .collect::<Vec<_>>(),
        [
            (
                "transient-rtmp-drop-preserves-egress",
                "flv",
                false,
                false,
                true,
            ),
            (
                "transient-srt-drop-preserves-egress",
                "mpegts",
                true,
                true,
                false,
            ),
        ]
    );

    for test_name in [
        "file-ingest-stop",
        "recording-stops-after-ingest-disconnect",
        "hls-preview-stops-after-ingest-disconnect",
        "file-ingest-eof-clears-and-restarts",
    ] {
        assert_eq!(
            ingest_lifecycle_case(test_name).unwrap().test_name,
            test_name
        );
    }
}
