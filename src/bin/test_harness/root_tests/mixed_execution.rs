use super::super::*;
use super::mixed_runner_matrix_source;

#[test]
fn unknown_command_error_lists_every_supported_mode() {
    let message = unknown_command_error("nope-mode");
    assert!(message.contains("\"nope-mode\""));
    assert!(message.contains("suite"));
    assert!(message.contains("preflight"));
    for mode in supported_mode_names() {
        assert!(
            message.contains(mode.as_str()),
            "unknown-command help text is missing mode {mode}"
        );
    }
}

#[test]
fn every_mode_spec_has_dispatch_arm() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/test_harness.rs"
    ));
    for spec in all_mode_specs() {
        if spec.name == MIXED_MATRIX_MODE
            || spec.name == MIXED_SIGNAL_MODE
            || spec.name == MIXED_FAST_BREADTH_MODE
            || spec.name == "generic-sweeps"
            || mixed_input_case_for_command(&spec.name).is_some()
        {
            // "generic-sweeps" is a manifest-only suite-composition entry
            // (folded into "mixed"); it has no standalone Rust runner.
            continue;
        }
        let arm = format!("\"{}\" =>", spec.name);
        assert!(
            source.contains(&arm),
            "mode {} is missing a run() dispatch arm",
            spec.name
        );
    }
}

#[test]
fn mixed_input_matrix_names_are_explicit_and_supported() {
    let names: Vec<_> = mixed_input_cases()
        .iter()
        .map(|case| case.scenario_id())
        .collect();
    assert_eq!(
        names,
        vec![
            "mixed.asset.file.h264.a1.bf0",
            "mixed.asset.file.h264.a1.bf2",
            "mixed.asset.file.h264.a2.bf0",
            "mixed.asset.file.h264.a2.bf2",
            "mixed.asset.file.h265.a1.bf0",
            "mixed.asset.file.h265.a1.bf2",
            "mixed.asset.file.h265.a2.bf0",
            "mixed.asset.file.h265.a2.bf2",
            "mixed.live.rtmp.h264.a1.bf0",
            "mixed.live.rtmp.h264.a1.bf2",
            "mixed.live.srt.h264.a1.bf0",
            "mixed.live.srt.h264.a1.bf2",
            "mixed.live.srt.h264.a2.bf0",
            "mixed.live.srt.h264.a2.bf2",
            "mixed.live.srt.h265.a1.bf0",
            "mixed.live.srt.h265.a1.bf2",
            "mixed.live.srt.h265.a2.bf0",
            "mixed.live.srt.h265.a2.bf2",
        ]
    );
    for case in mixed_input_cases() {
        let mode = mixed_input_mode_name(*case);
        assert_eq!(mixed_input_case_for_command(&mode), Some(*case));
        assert!(
            mode_spec(&mode).is_some(),
            "{mode} must be listed in harness help/suite specs"
        );
    }
}

#[test]
fn mixed_fast_breadth_is_small_but_axis_rich() {
    let names: Vec<_> = mixed_fast_breadth_cases()
        .iter()
        .map(|selected| selected.case.scenario_id())
        .collect();
    assert_eq!(
        names,
        vec![
            "mixed.asset.file.h264.a1.bf0",
            "mixed.asset.file.h265.a2.bf2",
            "mixed.live.rtmp.h264.a1.bf0",
            "mixed.live.rtmp.h264.a1.bf2",
            "mixed.live.srt.h264.a2.bf0",
            "mixed.live.srt.h265.a2.bf2",
        ]
    );

    let cases: Vec<_> = mixed_fast_breadth_cases()
        .iter()
        .map(|selected| selected.case)
        .collect();
    for protocol in [
        MixedInputProtocol::File,
        MixedInputProtocol::Rtmp,
        MixedInputProtocol::Srt,
    ] {
        assert!(
            cases.iter().any(|case| case.protocol() == protocol),
            "fast breadth must cover input protocol {protocol:?}"
        );
    }
    for codec in [MixedVideoCodec::H264, MixedVideoCodec::H265] {
        assert!(
            cases.iter().any(|case| case.codec() == codec),
            "fast breadth must cover codec {codec:?}"
        );
    }
    for audio in [MixedInputAudioLayout::A1, MixedInputAudioLayout::A2] {
        assert!(
            cases.iter().any(|case| case.audio_layout() == audio),
            "fast breadth must cover audio layout {audio:?}"
        );
    }
    for reorder in [MixedInputReorder::Bf0, MixedInputReorder::Bf2] {
        assert!(
            cases.iter().any(|case| case.reorder() == reorder),
            "fast breadth must cover reorder mode {reorder:?}"
        );
    }
    for reorder in [MixedInputReorder::Bf0, MixedInputReorder::Bf2] {
        assert!(
            cases.iter().any(|case| {
                case.protocol() == MixedInputProtocol::Rtmp && case.reorder() == reorder
            }),
            "fast breadth must cover RTMP sender reorder mode {reorder:?}"
        );
    }
    for selected in mixed_fast_breadth_cases() {
        assert!(
            !selected.checks.contains(&MixedCheck::Recording)
                && !selected.checks.contains(&MixedCheck::Load),
            "{} should keep fast-breadth checks short; use env overrides for depth",
            selected.case.scenario_id()
        );
    }
    assert_eq!(
        mixed_fast_breadth_cases()
            .iter()
            .filter(|selected| selected.checks.contains(&MixedCheck::Signal))
            .count(),
        1,
        "signal quality should be sampled on exactly one sentinel fast-breadth row"
    );
    assert!(
        mixed_fast_breadth_cases().iter().any(|selected| {
            selected.case.scenario_id() == "mixed.live.rtmp.h264.a1.bf0"
                && selected.checks.contains(&MixedCheck::Signal)
        }),
        "RTMP H.264 BF0 should stay the signal-quality sentinel row"
    );
    assert_eq!(
        mixed_fast_breadth_cases()
            .iter()
            .filter(|selected| selected.checks.contains(&MixedCheck::Hls))
            .count(),
        2,
        "HLS is sampled on representative H.264 and HEVC rows, not every row"
    );

    let selected_cells: usize = cases
        .iter()
        .map(|case| mixed_output_cases_for_input(*case).len())
        .sum();
    let total_cells: usize = mixed_input_cases()
        .iter()
        .map(|case| mixed_output_cases_for_input(*case).len())
        .sum();
    assert_eq!(selected_cells, 81);
    assert_eq!(total_cells, 232);
    assert!(
        selected_cells < total_cells / 2,
        "fast breadth should stay quick enough to run before the exhaustive matrix"
    );
    assert!(
        cases.iter().any(|case| {
            case.codec() == MixedVideoCodec::H265
                && mixed_output_cases_for_input(*case).iter().any(|output| {
                    output.protocol() == MixedOutputProtocol::Rtmp
                        && output.rtmp_mode() == RtmpOutputMode::Enhanced
                        && output.expected_video_codec_for_input(*case) == "hevc"
                })
        }),
        "fast breadth must cover Enhanced RTMP HEVC source egress"
    );
    for encoding_prefix in ["720p", "1080p"] {
        assert!(
            cases.iter().any(|case| {
                case.codec() == MixedVideoCodec::H265
                    && mixed_output_cases_for_input(*case).iter().any(|output| {
                        output.protocol() == MixedOutputProtocol::Rtmp
                            && output.rtmp_mode() == RtmpOutputMode::Enhanced
                            && output.encoding().starts_with(encoding_prefix)
                            && output.expected_video_codec_for_input(*case) == "hevc"
                    })
            }),
            "fast breadth must cover Enhanced RTMP HEVC {encoding_prefix} profile egress"
        );
    }
    for input_protocol in [MixedInputProtocol::Rtmp, MixedInputProtocol::Srt] {
        for encoding_prefix in ["720p", "1080p"] {
            assert!(
                cases.iter().any(|case| {
                    case.protocol() == input_protocol
                        && case.codec() == MixedVideoCodec::H264
                        && mixed_output_cases_for_input(*case).iter().any(|output| {
                            output.protocol() == MixedOutputProtocol::Rtmp
                                && output.rtmp_mode() == RtmpOutputMode::Enhanced
                                && output.encoding().starts_with(encoding_prefix)
                                && output.expected_video_codec_for_input(*case) == "h264"
                        })
                }),
                "fast breadth must cover Enhanced RTMP H.264 {encoding_prefix} profile egress from {input_protocol:?} input"
            );
        }
    }
}

#[test]
fn mixed_fast_breadth_batches_reuse_three_shared_stacks() {
    assert_eq!(mixed_fast_breadth_batches().len(), 3);
    assert_eq!(
        mixed_fast_breadth_batches()
            .iter()
            .map(|batch| batch.group.as_str())
            .collect::<Vec<_>>(),
        vec!["live-rtmp", "live-srt", "file-ingest"]
    );
    for batch in mixed_fast_breadth_batches() {
        assert!(
            !batch.cases.is_empty() && batch.cases.len() <= 2,
            "{} should stay within the two-pipeline shared-stack target",
            batch.group.as_str()
        );
        for case in &batch.cases {
            assert_eq!(
                case.shared_batch_group(),
                batch.group,
                "{} should be packed into its matching shared stack family",
                case.scenario_id()
            );
            mixed_fast_breadth_selected(*case);
        }
    }
}

#[test]
fn mixed_fast_breadth_group_parser_accepts_known_groups_once() {
    assert_eq!(
        parse_mixed_fast_breadth_groups("live-srt, live-rtmp, live-srt").unwrap(),
        vec![
            MixedSharedBatchGroup::LiveSrt,
            MixedSharedBatchGroup::LiveRtmp
        ]
    );
}

#[test]
fn mixed_fast_breadth_group_parser_rejects_unknown_groups() {
    let error = parse_mixed_fast_breadth_groups("live-srt,nope").unwrap_err();
    assert!(error.contains("unknown MIXED_FAST_BREADTH_GROUPS entry 'nope'"));
}

#[test]
fn mixed_fast_breadth_defaults_collect_failures_for_failure_mapping() {
    let root_source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/test_harness.rs"
    ));
    let mixed_source = mixed_runner_matrix_source();
    let source = format!("{root_source}\n{mixed_source}");

    assert!(
        source.contains("env.collect_failures = true"),
        "mixed.fast-breadth should continue through selected rows to map failures"
    );
    assert!(
        source.contains("\"defaultCollectFailures\""),
        "mixed.fast-breadth result metadata should disclose the collection default"
    );
    assert!(
        source.contains("root.join(\"assertions.jsonl\")"),
        "mixed.fast-breadth should emit machine-readable assertion rows by default"
    );
    assert!(
        source.contains("write_matrix_scenario_progress_for_mode"),
        "mixed.fast-breadth should persist scenario/root-cause progress artifacts"
    );
    assert!(
        source.contains("MIXED_FAST_BREADTH_MODE"),
        "mixed.fast-breadth progress artifacts should identify their mode"
    );
}

#[test]
fn mixed_matrix_defaults_to_shared_batch_execution() {
    let mixed_source = mixed_runner_matrix_source();

    assert!(
        mixed_source.contains("mixed_input_matrix_correctness_shared().await"),
        "mixed.matrix should default to the shared-batch matrix path"
    );
    assert!(
        mixed_source.contains("\"execution\": \"shared-batch\""),
        "mixed.matrix result metadata should report shared-batch execution"
    );
    assert!(
        mixed_source.contains("\"sharedBatches\""),
        "mixed.matrix metadata should report shared batch group coverage"
    );
}

#[test]
fn mixed_matrix_serial_opt_out_stays_explicit() {
    let mixed_source = mixed_runner_matrix_source();

    assert!(
        mixed_source.contains("MIXED_MATRIX_SERIAL"),
        "mixed.matrix should expose explicit serial opt-out env"
    );
    assert!(
        mixed_source.contains("mixed_input_matrix_correctness_serial().await"),
        "mixed.matrix should keep the serial fallback path for bisecting"
    );
    assert!(
        mixed_source.contains("\"execution\": \"serial\""),
        "mixed.matrix serial fallback should report serial execution metadata"
    );
}

#[test]
fn mixed_signal_group_parser_rejects_unknown_groups() {
    let error = parse_mixed_signal_groups("live-rtmp,nope").unwrap_err();
    assert!(error.contains("unknown MIXED_SIGNAL_GROUPS entry 'nope'"));
}

#[test]
fn mixed_signal_defaults_to_shared_batch_execution() {
    let mixed_source = mixed_runner_matrix_source();

    assert!(
        mixed_source.contains("mixed_signal_correctness"),
        "mixed.signal should route through its shared-batch runner"
    );
    assert!(
        mixed_source.contains("\"mode\": MIXED_SIGNAL_MODE"),
        "mixed.signal result metadata should report its mode"
    );
    assert!(
        mixed_source.contains("\"signalRationale\""),
        "mixed.signal results should disclose why each sentinel case exists"
    );
    assert!(
        mixed_source.contains("\"sharedBatches\""),
        "mixed.signal coverage should report shared batch group coverage"
    );
    assert!(
        mixed_source.contains("root.join(\"assertions.jsonl\")"),
        "mixed.signal should emit machine-readable assertion rows by default"
    );
}

#[test]
fn mixed_shared_batches_delete_finished_pipelines() {
    let mixed_source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/test_harness/mixed_runner.rs"
    ));

    assert!(
        mixed_source.contains("delete_pipeline_v1(api, pipeline_id).await?"),
        "shared mixed cases should delete finished pipelines so later waves do not accumulate dead state"
    );
    assert!(
        mixed_source.contains("\"scenario.pipeline_cleanup\""),
        "pipeline cleanup should emit timing so post-run analysis can confirm the amortization step"
    );
    assert!(
        mixed_source.contains("config[\"pipelineDeleted\"] = json!(true);"),
        "mixed case results should disclose successful pipeline cleanup"
    );
}

#[test]
fn mixed_matrix_defaults_exclude_signal_and_continue_on_failure() {
    let mixed_source = mixed_runner_matrix_source();

    assert!(
        mixed_source.contains("mixed_matrix_default_check_names"),
        "mixed.matrix should derive default checks from manifest"
    );
    assert!(
        mixed_source.contains("continueOnScenarioFailure"),
        "mixed.matrix metadata should disclose continue-on-failure behavior"
    );
    assert!(
        mixed_source.contains("MIXED_MATRIX_FAIL_FAST"),
        "mixed.matrix should expose fail-fast env opt-out"
    );
    assert!(
        mixed_source.contains("\"failures\": failures"),
        "mixed.matrix should aggregate per-scenario failures in final report"
    );
    assert!(
        !mixed_default_checks().contains(&MixedCheck::Signal),
        "mixed.matrix default checks should leave signal validation to mixed.signal"
    );
    assert!(
        !mixed_default_checks().contains(&MixedCheck::SoakDrift),
        "mixed.matrix default checks should leave soak drift with signal validation"
    );
}

#[test]
fn mixed_output_progress_gate_only_applies_to_external_read_checks() {
    assert!(mixed_output_checks_need_live_progress_gate(None));
    assert!(mixed_output_checks_need_live_progress_gate(Some(&[
        "ffprobe".to_string()
    ])));
    assert!(mixed_output_checks_need_live_progress_gate(Some(&[
        "ffprobe".to_string(),
        "signal".to_string()
    ])));
    assert!(!mixed_output_checks_need_live_progress_gate(Some(&[
        "signal".to_string()
    ])));
    assert!(!mixed_output_checks_need_live_progress_gate(Some(&[
        "soak-drift".to_string()
    ])));
}

#[test]
fn mixed_progress_output_ids_excludes_helper_outputs() {
    let output_ids = vec![
        "helper-hls".to_string(),
        "rtmp-a".to_string(),
        "srt-a".to_string(),
    ];
    assert_eq!(
        mixed_progress_output_ids(&output_ids, "helper-hls"),
        vec!["rtmp-a".to_string(), "srt-a".to_string()]
    );
}
