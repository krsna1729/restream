use super::super::*;

#[test]
fn default_work_db_path_stays_under_work_dir() {
    let work_dir = Path::new(".local/artifacts/example");
    assert_eq!(
        default_work_db_path(work_dir, "suite.db"),
        work_dir.join("suite.db")
    );
}

#[test]
fn mediamtx_child_env_removes_harness_port_overrides() {
    let mut command = Command::new("mediamtx");
    for name in MEDIAMTX_CONFIG_ENV_NAMES {
        command.env(name, "12345");
    }

    remove_mediamtx_config_env(&mut command);

    let envs: HashMap<String, Option<String>> = command
        .as_std()
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect();
    for name in MEDIAMTX_CONFIG_ENV_NAMES {
        assert_eq!(envs.get(name), Some(&None));
    }
}

#[test]
fn harness_source_does_not_use_repo_root_data_db_fallback() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/test_harness.rs"
    ));
    assert!(
        !source.contains("PathBuf::from(\"data.db\")"),
        "harness modes must keep mutable DB state under WORK_DIR"
    );
}

#[test]
fn release_shards_keep_ci_logs_progress_first() {
    let harness_source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/test_harness.rs"
    ));
    let release_wrapper = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/scripts/release/harness-shard.sh"
    ));

    assert!(
        harness_source.contains("TEST_HARNESS_SUPPRESS_SUCCESS_JSON"),
        "harness should expose an explicit opt-out for success JSON in CI logs"
    );
    assert!(
        harness_source.contains("!env_flag(\"TEST_HARNESS_SUPPRESS_SUCCESS_JSON\")"),
        "success JSON should remain the default for local one-off runs"
    );
    assert!(
        release_wrapper.contains("TEST_HARNESS_SUPPRESS_SUCCESS_JSON=1"),
        "release shards should upload JSON artifacts without dumping them into progress logs"
    );
    assert!(
        release_wrapper.contains("artifact paths"),
        "release wrapper comment should preserve why artifact lines stay visible"
    );
}

#[test]
fn strip_netns_opt_removes_only_the_opt_out_flag() {
    let raw = vec![
        "bitrate-sweep".to_string(),
        "--no-netns".to_string(),
        "--work-root".to_string(),
        ".local/artifacts/example".to_string(),
    ];
    assert_eq!(
        strip_netns_opt(&raw),
        vec![
            "bitrate-sweep".to_string(),
            "--work-root".to_string(),
            ".local/artifacts/example".to_string(),
        ]
    );
}

#[test]
fn file_live_edge_duration_budget_covers_one_target_gop() {
    assert_eq!(file_live_edge_max_duration_drift_secs(0), 0.75);
    assert_eq!(file_live_edge_max_duration_drift_secs(1), 1.75);
    assert_eq!(file_live_edge_max_duration_drift_secs(2), 2.75);
}

#[test]
fn explicit_restream_bin_is_allowed_for_measurement_candidates() {
    let harness = Path::new("/repo/target/release/test_harness");
    let copied_candidate = Path::new("/tmp/restream-baseline");
    let default_debug_restream = Path::new("/repo/target/debug/restream");

    assert!(measurement_profile_ok_with_explicit(
        harness,
        copied_candidate,
        true
    ));
    assert!(!measurement_profile_ok_with_explicit(
        harness,
        default_debug_restream,
        false
    ));
    assert!(!measurement_profile_ok_with_explicit(
        Path::new("/repo/target/debug/test_harness"),
        copied_candidate,
        true
    ));
}

#[test]
fn only_non_measurement_modes_parallelize_in_suite() {
    assert!(suite_mode_is_parallelizable("srt.policy", false));
    assert!(suite_mode_is_parallelizable("fault.egress-retry", false));
    assert!(suite_mode_is_parallelizable("fault.output-stall", false));
    assert!(suite_mode_is_parallelizable("fault.resilience", false));
    assert!(suite_mode_is_parallelizable("recovery", false));
    assert!(!suite_mode_is_parallelizable("bitrate-sweep", false));
    assert!(!suite_mode_is_parallelizable("preflight", true));
}

#[test]
fn suite_profile_parser_accepts_mode_timeout_option() {
    let raw = vec![
        "--only-modes".to_string(),
        "api-smoke".to_string(),
        "--mode-timeout-secs".to_string(),
        "120".to_string(),
    ];
    assert!(!suite_modes_require_bench_profile(&raw).expect("valid suite options"));
}

#[test]
fn mixed_matrix_uses_heavyweight_suite_timeout_floor() {
    assert_eq!(suite_mode_timeout_secs("api-smoke", 900), 900);
    assert_eq!(suite_mode_timeout_secs("mixed.matrix", 900), 5400);
    assert_eq!(suite_mode_timeout_secs("mixed.matrix", 6000), 6000);
    assert_eq!(suite_mode_timeout_secs("resource-sweep", 900), 2400);
    // The bitrate sweep runs real publisher, output, sampling, and probe
    // loops at multiple bitrate points. Local release evidence finishes just
    // under the default 900s ceiling, so hosted runners can hit an artificial
    // timeout while the mode is still making expected progress.
    assert_eq!(suite_mode_timeout_secs("bitrate-sweep", 900), 2400);
}

#[test]
fn suite_elapsed_formatter_is_compact_for_logs() {
    assert_eq!(
        suite_format_elapsed(std::time::Duration::from_millis(1234)),
        "1.2s"
    );
    assert_eq!(
        suite_format_elapsed(std::time::Duration::from_secs(65)),
        "1m05s"
    );
}

#[test]
fn suite_defaults_run_fast_signal_before_heavy_release_modes() {
    assert_eq!(
        suite_default_modes(),
        vec![
            "api-smoke",
            "file.live-edge",
            "srt.policy",
            "branch-matrix",
            "fault.resilience",
            "srt-crypto-matrix",
            "ramp-family",
            "resource-sweep",
            "bitrate-sweep",
            "mixed.matrix",
        ]
    );

    for mode in suite_default_modes() {
        let spec = mode_spec(&mode).unwrap_or_else(|| panic!("{mode} must be listed"));
        assert!(
            spec.suite_order.is_some(),
            "{mode} must declare suiteOrder so release suite ordering stays intentional"
        );
    }
}

#[test]
fn fault_output_stall_sibling_count_honors_n_per_group_cap() {
    assert_eq!(effective_fault_output_stall_siblings(12, None), 12);
    assert_eq!(effective_fault_output_stall_siblings(12, Some(1)), 1);
    assert_eq!(effective_fault_output_stall_siblings(4, Some(8)), 4);
    assert_eq!(effective_fault_output_stall_siblings(0, Some(0)), 1);
}

#[test]
fn synthesized_harness_ports_are_high_and_distinct() {
    let mut reserved = HashSet::new();
    let http = env_or_allocated_port("RESTREAM_HTTP", 3030, &mut reserved);
    let rtmp = env_or_allocated_port("RESTREAM_RTMP", 1935, &mut reserved);
    let srt = env_or_allocated_port("RESTREAM_SRT", 10080, &mut reserved);
    let mtx_api = env_or_allocated_port("MTX_API", 9997, &mut reserved);
    let unique: HashSet<u16> = [http, rtmp, srt, mtx_api].into_iter().collect();

    assert_eq!(unique.len(), 4);
    assert!(unique.iter().all(|port| *port >= 20_000));
}

#[test]
fn synthesized_harness_port_ranges_do_not_overlap() {
    let mut reserved = HashSet::new();
    let sink = env_or_allocated_port_range("SINK_PORT", SINK_PORT, 256, &mut reserved);
    let hls_put = env_or_allocated_port_range("HLS_PUT_PORT", 8990, 16, &mut reserved);
    let ffmpeg_srt =
        env_or_allocated_port_range("FFMPEG_SRT_SINK_BASE", 15_000, 1024, &mut reserved);
    let ffmpeg_signal =
        env_or_allocated_port_range("FFMPEG_SIGNAL_SINK_BASE", 16_000, 1024, &mut reserved);

    let sink_end = sink as u32 + 255;
    let hls_put_end = hls_put as u32 + 15;
    let ffmpeg_srt_end = ffmpeg_srt as u32 + 1023;
    let ffmpeg_signal_end = ffmpeg_signal as u32 + 1023;

    assert!(sink >= 20_000);
    assert!(hls_put >= 20_000);
    assert!(ffmpeg_srt >= 20_000);
    assert!(ffmpeg_signal >= 20_000);
    assert!(sink_end < hls_put as u32 || hls_put_end < sink as u32);
    assert!(sink_end < ffmpeg_srt as u32 || ffmpeg_srt_end < sink as u32);
    assert!(sink_end < ffmpeg_signal as u32 || ffmpeg_signal_end < sink as u32);
    assert!(hls_put_end < ffmpeg_srt as u32 || ffmpeg_srt_end < hls_put as u32);
    assert!(hls_put_end < ffmpeg_signal as u32 || ffmpeg_signal_end < hls_put as u32);
    assert!(ffmpeg_srt_end < ffmpeg_signal as u32 || ffmpeg_signal_end < ffmpeg_srt as u32);
}

#[test]
fn parse_log_fields_handles_json_string_payloads() {
    let log = json!({
        "fields": r#"{"correlation_id":"out-0001","phase":"connect"}"#
    });

    let fields = parse_log_fields(&log).expect("parsed fields");
    assert_eq!(fields["correlation_id"], "out-0001");
    assert_eq!(fields["phase"], "connect");
}

#[test]
fn api_output_status_has_status_raw_status_phase() {
    let value = json!({
        "outputId": "out-1",
        "outputName": "primary",
        "status": "running",
        "rawStatus": "running",
        "phase": "sending",
        "bytesOut": 7,
        "metrics": {
            "bytesOut": 11,
            "packetsOut": 2
        },
        "blockedBy": {
            "stage": "transcode:out-1",
            "phase": "waitingForCapacity",
            "backend": "externalFfmpeg",
            "capacityWaitMs": 123
        }
    });

    let status = ApiOutputStatus::from_value("out-1", &value).expect("typed output status");
    assert_eq!(status.status, "running");
    assert_eq!(status.raw_status, "running");
    assert_eq!(status.phase, "sending");
    assert!(status.has_progress());
    assert_eq!(
        status
            .blocked_by
            .as_ref()
            .and_then(|blocked| blocked.stage.as_deref()),
        Some("transcode:out-1")
    );
}

#[test]
fn harness_fails_if_status_schema_drops_required_fields() {
    for missing_field in ["status", "rawStatus", "phase"] {
        let mut value = json!({
            "status": "running",
            "rawStatus": "running",
            "phase": "sending",
            "metrics": {
                "bytesOut": 0,
                "packetsOut": 0
            }
        });
        value.as_object_mut().expect("object").remove(missing_field);

        let error =
            ApiOutputStatus::from_value("out-1", &value).expect_err("required field missing");
        assert!(
            error.contains("output status for out-1"),
            "unexpected error for {missing_field}: {error}"
        );
    }
}

#[test]
fn harness_progress_status_consumes_existing_fields() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/test_harness/output_progress.rs"
    ));

    assert!(
        source.contains("ApiOutputStatus::from_value"),
        "progress checks must consume the typed API output status DTO"
    );
    assert!(
        !source.contains("[\"status\"]")
            && !source.contains("[\"rawStatus\"]")
            && !source.contains("[\"phase\"]"),
        "progress checks must not index status/rawStatus/phase directly"
    );
    assert!(
        source.contains("healthRow=missing"),
        "temporarily missing health rows should be reported as progress stalls"
    );
    assert!(
        source.contains("timed out waiting for outputs to make progress"),
        "progress deadline failures should call out timeout explicitly"
    );
    assert!(
        source.contains("[harness-progress] outputs-progress start")
            && source.contains("[harness-progress] outputs-progress wait")
            && source.contains("[harness-progress] outputs-progress pass"),
        "live harness output waits should emit coarse progress in CI logs"
    );
}

#[test]
fn ingest_waits_emit_live_progress_markers() {
    let source = [
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/live_modes.rs"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/live_modes/shared.rs"
        )),
    ]
    .join("\n");

    assert!(
        source.contains("[harness-progress] input-live start")
            && source.contains("[harness-progress] input-live wait")
            && source.contains("[harness-progress] input-live pass"),
        "input-live waits should emit coarse progress in CI logs"
    );
    assert!(
        source.contains("[harness-progress] input-media-ready start")
            && source.contains("[harness-progress] input-media-ready wait")
            && source.contains("[harness-progress] input-media-ready pass"),
        "media-ready waits should emit coarse progress in CI logs"
    );
}

#[test]
fn progress_failure_includes_cell_identity() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/test_harness/output_progress.rs"
    ));

    assert!(source.contains("output_cell_label(output_id)"));
    assert!(source.contains("unregistered-cell"));
}

#[test]
fn progress_failure_includes_dependency_chain_when_available() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/test_harness/output_progress.rs"
    ));

    for field in [
        "terminalStage",
        "blockedBy",
        "blockedByPhase",
        "backend",
        "waitMs",
    ] {
        assert!(
            source.contains(field),
            "progress failure should include {field}"
        );
    }
}

#[test]
fn recording_uses_metadata_identity_not_tmp_filename() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/test_harness/mixed_playback.rs"
    ));

    assert!(source.contains("recordingId"));
    assert!(source.contains("media_recording_identity_rejects_metadata_less_filename_fallback"));
    assert!(source.contains("media_recording_play_name_rejects_temporary_outputs"));
}

#[test]
fn hls_preview_plan_uses_graph_planner() {
    let planner = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/planner/graph_plan.rs"
    ));
    let runtime = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/media/hls/preview_graph.rs"
    ));

    assert!(planner.contains("pub fn plan_hls_preview_graph("));
    assert!(runtime.contains("plan_hls_preview_graph("));
    assert!(runtime.contains("preview_plan"));
    assert!(runtime.contains(".stages"));
    assert!(runtime.contains("spawn_preview_stage"));
}
