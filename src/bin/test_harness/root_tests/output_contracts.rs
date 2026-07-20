use super::super::*;

#[test]
fn output_retry_fault_phase_accepts_retry_error_or_cleanup() {
    let retrying_with_error = OutputRetryObservation {
        status_visible: true,
        has_error: true,
        ..Default::default()
    };
    let cleaned_up = OutputRetryObservation {
        cleaned_up: true,
        ..Default::default()
    };
    let retrying_without_error = OutputRetryObservation {
        status_visible: true,
        ..Default::default()
    };

    assert!(output_retry_or_cleanup_phase_ok(&retrying_with_error));
    assert!(output_retry_or_cleanup_phase_ok(&cleaned_up));
    assert!(!output_retry_or_cleanup_phase_ok(&retrying_without_error));
}

#[test]
fn ramp_json_dsl_carries_current_config_contract() {
    let configs = ramp_configs();
    assert_eq!(configs.len(), 8);
    assert_eq!(configs[0].name, "rtmp-rtmp-src");
    assert_eq!(configs[7].name, "srt-srt-720p");
}

#[test]
fn resource_egress_scenario_table_carries_branch_contract() {
    assert_eq!(resource_egress_scenarios().len(), 10);
    assert_eq!(
        resource_egress_scenarios()
            .iter()
            .filter(|scenario| scenario.branch_order.is_some())
            .count(),
        5
    );
    assert_eq!(
        resource_egress_scenario("egress-growth-hevc-bridge")
            .unwrap()
            .config_index,
        2
    );
    assert_eq!(
        resource_egress_scenario("egress-growth-source-srt")
            .unwrap()
            .output_kinds,
        vec![SweepOutputKind::SrtSource]
    );
    assert_eq!(
        resource_egress_scenario("egress-growth-transcode-srt")
            .unwrap()
            .output_kinds,
        vec![SweepOutputKind::Srt720p]
    );
    assert_eq!(
        resource_egress_scenario("egress-growth-source-plus-transcode-dual-mixed")
            .unwrap()
            .output_kinds,
        vec![
            SweepOutputKind::RtmpSource,
            SweepOutputKind::SrtSource,
            SweepOutputKind::Rtmp720p,
            SweepOutputKind::Srt720p,
            SweepOutputKind::Rtmp1080p,
            SweepOutputKind::Srt1080p,
        ]
    );
    assert_eq!(
        resource_egress_scenario("egress-growth-transcode-mixed")
            .unwrap()
            .branch_label(),
        "one transcode family (720p)"
    );
}

#[test]
fn sweep_output_kind_centralizes_urls_and_multi_audio_encoding() {
    assert_eq!(
        SweepOutputKind::Rtmp720p.publish_url(1936, 8891, "out"),
        "rtmp://127.0.0.1:1936/live/out"
    );
    assert_eq!(
        SweepOutputKind::Srt720p.publish_url(1936, 8891, "out"),
        "srt://127.0.0.1:8891?streamid=publish:out"
    );
    assert_eq!(
        SweepOutputKind::Srt720p.read_url(1936, 8891, "out"),
        "srt://127.0.0.1:8891?streamid=read:out&timeout=30000000"
    );
    assert_eq!(SweepOutputKind::Rtmp720p.encoding(true), "720p+atrack:0");
    assert_eq!(SweepOutputKind::Srt720p.encoding(true), "720p+atrack:0,1");
    assert_eq!(
        SweepOutputKind::RtmpSource.encoding(true),
        "source+atrack:0"
    );
    assert_eq!(
        SweepOutputKind::SrtSource.encoding(true),
        "source+atrack:0,1"
    );
    assert_eq!(
        SweepOutputKind::RtmpSourceDownmix.encoding(true),
        "source+downmix:0"
    );
    assert_eq!(
        SweepOutputKind::SrtSourceDownmix.encoding(true),
        "source+downmix:0"
    );
    assert_eq!(
        SweepOutputKind::RtmpSource.rtmp_mode(),
        RtmpOutputMode::Enhanced
    );
    assert_eq!(
        SweepOutputKind::RtmpSourceDownmix.rtmp_mode(),
        RtmpOutputMode::Enhanced
    );
    assert_eq!(
        SweepOutputKind::Rtmp720p.rtmp_mode(),
        RtmpOutputMode::Legacy
    );
}

#[test]
fn internal_backend_smoke_filters_hevc_codec_edge_output_groups() {
    let source = include_str!("../../../../scripts/harness/rollouts/internal-backend-smoke.sh");

    assert!(source.contains("RESTREAM_INTERNAL_HEVC_TO_H264=1"));
    assert!(source.contains("N_PER_GROUP=1"));
    assert!(source.contains("MIXED_OUTPUT_GROUPS=rtmp.720p.a0"));
    assert!(source.contains("ONLY_CHECKS=load,ffprobe,stage-sharing"));
    assert!(source.contains("MIXED_OUTPUT_GROUPS=rtmp.720p.a0,rtmp.720p.a1"));
}

#[test]
fn mixed_signal_skips_rtmp_hevc_publish_probe_rows() {
    let source = include_str!("../mixed_checks.rs");

    assert!(source.contains(
        "let mediamtx_publish_probe = matches!(case.protocol(), MixedOutputProtocol::Rtmp)"
    ));
    assert!(source.contains("env.check_selected(\"audio-route\")"));
    assert!(source.contains("env.check_selected(\"decode-scan\")"));
    assert!(source.contains("&& !mediamtx_publish_probe"));
}

#[test]
fn backend_policy_matrix_is_separate_from_symmetric_mixed_matrix() {
    let matrix_spec = mode_spec("mixed.matrix").expect("mixed.matrix must be listed");
    let policy_spec =
        mode_spec("backend-policy-matrix").expect("backend-policy-matrix must be listed");

    assert!(matrix_spec.suite_default);
    assert!(!policy_spec.suite_default);
    assert!(policy_spec.requires_bench_profile);
}

#[test]
fn backend_policy_matrix_default_variants_cover_four_internal_stage_families() {
    let variants = selected_backend_policy_variants().expect("default variants should parse");
    let names: Vec<_> = variants.iter().map(|variant| variant.name()).collect();

    assert_eq!(
        names,
        vec![
            "external-all",
            "internal-video-presets",
            "internal-hevc-to-h264",
            "internal-hls-preview",
            "internal-complex-audio",
            "internal-all",
        ]
    );
}

#[test]
fn resource_output_progress_timeout_scales_and_caps() {
    assert_eq!(
        scaled_output_progress_timeout(1, 30, 4, 240),
        Duration::from_secs(30)
    );
    assert_eq!(
        scaled_output_progress_timeout(20, 30, 4, 240),
        Duration::from_secs(106)
    );
    assert_eq!(
        scaled_output_progress_timeout(60, 30, 4, 240),
        Duration::from_secs(240)
    );
    assert_eq!(
        scaled_output_progress_timeout(0, 30, 4, 240),
        Duration::from_secs(30)
    );
}

#[test]
fn mixed_hevc_output_progress_timeout_gets_codec_edge_budget() {
    let h264_case = mixed_input_cases()
        .iter()
        .copied()
        .find(|case| matches!(case.codec(), MixedVideoCodec::H264))
        .expect("h264 mixed case");
    let h265_case = mixed_input_cases()
        .iter()
        .copied()
        .find(|case| matches!(case.codec(), MixedVideoCodec::H265))
        .expect("h265 mixed case");

    assert_eq!(
        mixed_output_progress_timeout_for_case(h264_case, 15),
        Duration::from_secs(102)
    );
    assert_eq!(
        mixed_output_progress_timeout_for_case(h265_case, 15),
        Duration::from_secs(192)
    );
}

#[test]
fn mixed_adaptive_ring_gate_bounds_telemetry_requests() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/test_harness/mixed_telemetry.rs"
    ));

    assert!(
        source.contains("ADAPTIVE_RING_CHECK_TIMEOUT"),
        "adaptive ring checks should keep an explicit whole-gate deadline"
    );
    assert!(
        source.contains("ADAPTIVE_RING_TELEMETRY_REQUEST_TIMEOUT"),
        "adaptive ring checks should bound each telemetry poll"
    );
    assert!(
        source.contains("tokio::time::timeout("),
        "a hung telemetry request must not stall the shard until the outer CI timeout"
    );
}

#[test]
fn mixed_matrix_hevc_rows_do_not_share_capacity_wave() {
    let h264_case = mixed_input_cases()
        .iter()
        .copied()
        .find(|case| matches!(case.codec(), MixedVideoCodec::H264))
        .expect("h264 mixed case");
    let h265_case = mixed_input_cases()
        .iter()
        .copied()
        .find(|case| matches!(case.codec(), MixedVideoCodec::H265))
        .expect("h265 mixed case");

    assert!(mixed_matrix_cases_can_share_wave(h264_case, h264_case));
    assert!(!mixed_matrix_cases_can_share_wave(h264_case, h265_case));
    assert!(!mixed_matrix_cases_can_share_wave(h265_case, h264_case));
    assert!(!mixed_matrix_cases_can_share_wave(h265_case, h265_case));
}

#[test]
fn mixed_input_planning_shares_stages_across_duplicate_outputs() {
    for case in mixed_input_cases() {
        let single = planned_mixed_stage_count(*case, 1);
        let duplicated = planned_mixed_stage_count(*case, 2);
        let expected = expected_mixed_stage_count(*case);

        assert_eq!(
            single,
            expected,
            "{} should plan the expected unique stage set",
            case.scenario_id()
        );
        assert_eq!(
            duplicated,
            single,
            "{} should not add unique processing stages when N_PER_GROUP grows",
            case.scenario_id()
        );
    }
}

#[test]
fn mixed_input_suite_default_runs_aggregate_not_duplicate_rows() {
    let matrix_spec = mode_spec("mixed.matrix").expect("mixed.matrix must be listed");
    assert!(matrix_spec.suite_default);
    let signal_spec = mode_spec(MIXED_SIGNAL_MODE).expect("mixed.signal must be listed");
    assert!(!signal_spec.suite_default);
    let fast_spec = mode_spec(MIXED_FAST_BREADTH_MODE).expect("mixed.fast-breadth must be listed");
    assert!(!fast_spec.suite_default);
    for case in mixed_input_cases() {
        let mode = mixed_input_mode_name(*case);
        let spec = mode_spec(&mode).unwrap_or_else(|| panic!("{mode} must be listed"));
        assert!(
            !spec.suite_default,
            "{mode} is covered by mixed.matrix and should not duplicate default suite work"
        );
    }
}

#[test]
fn mixed_input_modes_share_one_bench_profile_policy() {
    for case in mixed_input_cases() {
        let mode = mixed_input_mode_name(*case);
        let spec = mode_spec(&mode).unwrap_or_else(|| panic!("{mode} must be listed"));
        assert!(
            spec.requires_bench_profile,
            "{mode} should inherit the mixed harness bench-profile requirement"
        );
    }
}

#[test]
fn mixed_input_fixture_selection_tracks_reorder_signal() {
    for case in mixed_input_cases() {
        let fixture = mixed_input_fixture(*case).unwrap_or_else(|error| {
            panic!(
                "{} should resolve a checked-in fixture: {error}",
                case.scenario_id()
            )
        });
        let file_name = fixture.file_name().unwrap().to_string_lossy();
        match case.reorder() {
            MixedInputReorder::Bf0 => assert!(
                file_name.contains("-bf0"),
                "{} should use a bf0 fixture, got {}",
                case.scenario_id(),
                file_name
            ),
            MixedInputReorder::Bf2 => assert!(
                !file_name.contains("-bf0"),
                "{} should use the reordered bf2 fixture family, got {}",
                case.scenario_id(),
                file_name
            ),
        }
    }
}

#[test]
fn single_track_output_matrix_exercises_all_protocol_encoding_pairs() {
    let pairs: Vec<_> = single_track_mixed_output_cases()
        .iter()
        .map(|case| (mixed_output_protocol_name(case.protocol()), case.encoding()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("rtmp", "source"),
            ("rtmp", "720p"),
            ("rtmp", "720p"),
            ("rtmp", "1080p"),
            ("rtmp", "1080p"),
            ("srt", "source"),
            ("srt", "720p"),
            ("srt", "1080p"),
        ]
    );
    assert!(
        single_track_mixed_output_cases()
            .iter()
            .all(|case| case.expected_audio_tracks() == 1)
    );
    let rtmp_source = single_track_mixed_output_cases()
        .iter()
        .find(|case| case.id() == "rtmp.src.a0")
        .expect("single-track RTMP source row should exist");
    assert_eq!(rtmp_source.rtmp_mode(), RtmpOutputMode::Enhanced);
    assert_eq!(
        rtmp_source.expected_video_codec_for_input(
            mixed_input_case_for_command("mixed.live.srt.h265.a1.bf2").unwrap()
        ),
        "hevc"
    );
}

#[test]
fn single_track_output_matrix_reports_same_rows_it_executes() {
    let rows = mixed_output_matrix_json(single_track_mixed_output_cases());
    let groups: Vec<_> = rows.iter().map(|row| row["id"].as_str().unwrap()).collect();
    assert_eq!(
        groups,
        vec![
            "rtmp.src.a0",
            "rtmp.720p.a0",
            "rtmp.720p-enh.a0",
            "rtmp.1080p.a0",
            "rtmp.1080p-enh.a0",
            "srt.src.a0",
            "srt.720p.a0",
            "srt.1080p.a0",
        ]
    );
}

#[test]
fn multi_track_output_matrix_exercises_rtmp_subsets_and_srt_all_plus_subsets() {
    let groups: Vec<_> = multi_track_mixed_output_cases()
        .iter()
        .map(|case| case.id())
        .collect();
    assert_eq!(
        groups,
        vec![
            "rtmp.src.a0",
            "rtmp.src.a1",
            "rtmp.720p.a0",
            "rtmp.720p-enh.a0",
            "rtmp.720p.a1",
            "rtmp.720p-enh.a1",
            "rtmp.1080p.a0",
            "rtmp.1080p-enh.a0",
            "rtmp.1080p.a1",
            "rtmp.1080p-enh.a1",
            "srt.src.all",
            "srt.src.a0",
            "srt.src.a1",
            "srt.720p.all",
            "srt.720p.a0",
            "srt.720p.a1",
            "srt.1080p.all",
            "srt.1080p.a0",
            "srt.1080p.a1",
        ]
    );
    let rtmp_cases: Vec<_> = multi_track_mixed_output_cases()
        .iter()
        .filter(|case| case.protocol() == MixedOutputProtocol::Rtmp)
        .collect();
    assert_eq!(rtmp_cases.len(), 10);
    assert!(
        rtmp_cases
            .iter()
            .all(|case| case.expected_audio_tracks() == 1)
    );
    assert!(
        rtmp_cases
            .iter()
            .all(|case| case.selected_audio_track().is_some())
    );
    assert!(
        rtmp_cases
            .iter()
            .filter(|case| case.encoding().starts_with("source+"))
            .all(|case| case.rtmp_mode() == RtmpOutputMode::Enhanced),
        "multi-track RTMP source rows should exercise Enhanced RTMP"
    );
    assert!(
        rtmp_cases
            .iter()
            .filter(|case| !case.encoding().starts_with("source+"))
            .any(|case| case.rtmp_mode() == RtmpOutputMode::Enhanced),
        "multi-track RTMP scaled rows should exercise Enhanced RTMP"
    );

    let srt_all_cases: Vec<_> = multi_track_mixed_output_cases()
        .iter()
        .filter(|case| {
            case.protocol() == MixedOutputProtocol::Srt && case.selected_audio_track().is_none()
        })
        .collect();
    assert_eq!(srt_all_cases.len(), 3);
    assert!(
        srt_all_cases
            .iter()
            .all(|case| case.expected_audio_tracks() == 2)
    );
}
