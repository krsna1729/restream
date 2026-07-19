use super::*;

fn mixed_runner_matrix_source() -> String {
    [
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/mixed_runner.rs"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/mixed_matrix_runner.rs"
        )),
    ]
    .join("\n")
}

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
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/test_harness/live_modes.rs"
    ));

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

#[test]
fn generalized_sink_rejects_equal_video_dts() {
    let metrics = GeneralizedSinkMetrics::default();
    metrics.packets.lock().unwrap().extend([
        SinkPacket {
            media_type: "video",
            timestamp_ms: 10,
            audio_packet_type: None,
            audio_has_adts_sync: false,
            video_is_sequence_header: false,
        },
        SinkPacket {
            media_type: "video",
            timestamp_ms: 10,
            audio_packet_type: None,
            audio_has_adts_sync: false,
            video_is_sequence_header: false,
        },
    ]);

    assert!(
        !metrics.dts_monotone(),
        "FFmpeg rejects equal DTS as non-monotonic; harness must too"
    );
}

#[test]
fn generalized_sink_ignores_video_sequence_headers_for_dts() {
    let metrics = GeneralizedSinkMetrics::default();
    metrics.packets.lock().unwrap().extend([
        SinkPacket {
            media_type: "video",
            timestamp_ms: 10,
            audio_packet_type: None,
            audio_has_adts_sync: false,
            video_is_sequence_header: false,
        },
        SinkPacket {
            media_type: "video",
            timestamp_ms: 10,
            audio_packet_type: None,
            audio_has_adts_sync: false,
            video_is_sequence_header: true,
        },
        SinkPacket {
            media_type: "video",
            timestamp_ms: 11,
            audio_packet_type: None,
            audio_has_adts_sync: false,
            video_is_sequence_header: false,
        },
    ]);

    assert!(metrics.dts_monotone());
}

#[test]
fn ffprobe_compact_validator_accepts_reordered_packet_dump() {
    let log = "\
packet|stream_index=1|pts_time=10.021333|dts_time=10.021333\n\
packet|stream_index=0|pts_time=10.100000|dts_time=10.100000\n\
packet|stream_index=1|pts_time=10.000000|dts_time=10.000000\n\
stream|index=0|codec_type=video|width=1920|height=1080\n\
stream|index=1|codec_type=audio\n";

    assert_eq!(
        ffprobe_compact_video_dimensions(log).as_deref(),
        Some("1920x1080")
    );
    assert_eq!(ffprobe_compact_audio_track_count(log), 1);
    assert_eq!(ffprobe_compact_validate_dts(log), Ok(3));
}

#[test]
fn ffprobe_compact_validator_rejects_duplicate_dts() {
    let log = "\
packet|stream_index=1|pts_time=10.000000|dts_time=10.000000\n\
packet|stream_index=1|pts_time=10.000000|dts_time=10.000000\n\
stream|index=1|codec_type=audio\n";

    let error = ffprobe_compact_validate_dts(log).expect_err("duplicate DTS must fail");
    assert!(error.contains("duplicate DTS"));
}

#[test]
fn ffprobe_compact_validator_rejects_large_dts_gap() {
    let log = "\
packet|stream_index=1|pts_time=10.000000|dts_time=10.000000\n\
packet|stream_index=1|pts_time=11.000000|dts_time=11.000000\n\
stream|index=1|codec_type=audio\n";

    let error = ffprobe_compact_validate_dts(log).expect_err("large DTS gap must fail");
    assert!(error.contains("DTS gap"));
}

#[test]
fn decode_scan_video_dts_fallback_applies_to_rtmp_and_srt_muxer_warnings() {
    assert!(decode_scan_needs_video_dts_fallback(
        "rtmp://127.0.0.1/live/test",
        Some(0),
        Some("non monoton"),
    ));
    assert!(decode_scan_needs_video_dts_fallback(
        "rtmp://127.0.0.1/live/test",
        Some(0),
        Some("non-monoton"),
    ));
    assert!(decode_scan_needs_video_dts_fallback(
        "srt://127.0.0.1:9999?streamid=read:test",
        Some(0),
        Some("non monoton"),
    ));
    assert!(!decode_scan_needs_video_dts_fallback(
        "rtmp://127.0.0.1/live/test",
        Some(1),
        Some("non monoton"),
    ));
    assert!(!decode_scan_needs_video_dts_fallback(
        "rtmp://127.0.0.1/live/test",
        Some(0),
        Some("invalid data"),
    ));
    assert!(!decode_scan_needs_video_dts_fallback(
        "http://127.0.0.1/live/test",
        Some(0),
        Some("non monoton"),
    ));
}

#[test]
fn marker_gap_parser_extracts_flash_and_beep_times() {
    let black = "\
[blackdetect @ 0x1] black_start:0 black_end:2 black_duration:2\n\
[blackdetect @ 0x1] black_start:2.2 black_end:7 black_duration:4.8\n\
[blackdetect @ 0x1] black_start:7.2 black_end:12 black_duration:4.8\n\
[blackdetect @ 0x1] black_start:12.2 black_end:17 black_duration:4.8\n\
[blackdetect @ 0x1] black_start:17.2 black_end:20 black_duration:2.8\n";
    let silence = "\
[silencedetect @ 0x1] silence_start: 0\n\
[silencedetect @ 0x1] silence_end: 2.02 | silence_duration: 2.02\n\
[silencedetect @ 0x1] silence_start: 2.22\n\
[silencedetect @ 0x1] silence_end: 7.02 | silence_duration: 4.8\n\
[silencedetect @ 0x1] silence_start: 7.22\n\
[silencedetect @ 0x1] silence_end: 12.02 | silence_duration: 4.8\n\
[silencedetect @ 0x1] silence_start: 12.22\n\
[silencedetect @ 0x1] silence_end: 17.02 | silence_duration: 4.8\n\
[silencedetect @ 0x1] silence_start: 17.22\n";

    let video = marker_gaps_from_intervals(&parse_blackdetect_intervals(black));
    let audio = marker_gaps_from_intervals(&parse_silencedetect_intervals(silence));

    assert_eq!(video.len(), 4);
    assert_eq!(audio.len(), 3);
    assert!((video[0] - 2.1).abs() < 0.001);
    assert!((audio[0] - 2.12).abs() < 0.001);
}

#[test]
fn signal_quality_rejects_marker_drift() {
    let black = "\
[blackdetect @ 0x1] black_start:0 black_end:2 black_duration:2\n\
[blackdetect @ 0x1] black_start:2.2 black_end:7 black_duration:4.8\n\
[blackdetect @ 0x1] black_start:7.2 black_end:12 black_duration:4.8\n\
[blackdetect @ 0x1] black_start:12.2 black_end:17 black_duration:4.8\n\
[blackdetect @ 0x1] black_start:17.2 black_end:20 black_duration:2.8\n";
    let silence = "\
[silencedetect @ 0x1] silence_start: 0\n\
[silencedetect @ 0x1] silence_end: 2.02 | silence_duration: 2.02\n\
[silencedetect @ 0x1] silence_start: 2.22\n\
[silencedetect @ 0x1] silence_end: 7.20 | silence_duration: 4.98\n\
[silencedetect @ 0x1] silence_start: 7.40\n\
[silencedetect @ 0x1] silence_end: 12.45 | silence_duration: 5.05\n\
[silencedetect @ 0x1] silence_start: 12.65\n\
[silencedetect @ 0x1] silence_end: 17.80 | silence_duration: 5.15\n\
[silencedetect @ 0x1] silence_start: 18.00\n";
    let ashow = "\
[Parsed_ashowinfo_0 @ 0x1] n:0 pts_time:0\n\
[Parsed_ashowinfo_0 @ 0x1] n:1 pts_time:0.021333\n\
[Parsed_ashowinfo_0 @ 0x1] n:2 pts_time:0.042666\n";
    let pcm = PcmQualityReport {
        samples: 1024,
        clipping_samples: 0,
        max_step: 100,
        rms: 10.0,
    };

    let error = validate_signal_quality(black, silence, ashow, "", pcm)
        .expect_err("marker drift must fail");
    assert!(error.contains("drift") || error.contains("offset"));
}

#[test]
fn nearest_marker_pairing_tolerates_live_capture_starting_mid_cycle() {
    let video = vec![6.4125, 11.4125, 16.4125];
    let audio = vec![1.396, 6.396, 11.396, 16.396];

    let offsets = nearest_marker_offsets_ms(&video, &audio, 1000.0);

    assert_eq!(offsets.len(), 3);
    assert!(offsets.iter().all(|offset| offset.abs() < 25.0));
}

#[test]
fn audio_pts_gap_uses_median_frame_delta() {
    let ashow = "\
[Parsed_ashowinfo_0 @ 0x1] n:0 pts_time:0\n\
[Parsed_ashowinfo_0 @ 0x1] n:1 pts_time:0.021333\n\
[Parsed_ashowinfo_0 @ 0x1] n:2 pts_time:0.042666\n\
[Parsed_ashowinfo_0 @ 0x1] n:3 pts_time:0.200000\n";

    assert!(max_audio_pts_gap_ms(ashow) > 100.0);
}

#[test]
fn pcm_quality_detects_clipping_and_impulses() {
    let mut bytes = Vec::new();
    for sample in [0i16, 10, -10, 32767, -32768] {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }

    let report = analyze_pcm_s16le(&bytes);

    assert_eq!(report.samples, 5);
    assert_eq!(report.clipping_samples, 2);
    assert!(report.max_step > 30_000);
}

#[test]
fn log_has_correlation_id_detects_both_field_spellings() {
    let snake = json!({
        "fields": r#"{"correlation_id":"out-0001"}"#
    });
    let camel = json!({
        "fields": r#"{"correlationId":"stage-0002"}"#
    });
    let none = json!({
        "fields": r#"{"phase":"connect"}"#
    });

    assert!(log_has_correlation_id(&snake));
    assert!(log_has_correlation_id(&camel));
    assert!(!log_has_correlation_id(&none));
}

#[test]
fn proc_net_has_listening_port_matches_ipv4_listener_entries() {
    let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:4C4F 00000000:0000 0A 00000000:00000000 00:00000000 00000000   100        0 1 1 0000000000000000 100 0 0 10 0\n";

    assert!(proc_net_has_listening_port(table, 19535));
    assert!(!proc_net_has_listening_port(table, 1935));
}

#[test]
fn proc_net_has_listening_port_ignores_non_listen_states() {
    let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:078F 0100007F:9C40 01 00000000:00000000 00:00000000 00000000   100        0 1 1 0000000000000000 100 0 0 10 0\n";

    assert!(!proc_net_has_listening_port(table, 1935));
}

#[test]
fn kill_and_wait_child_terminates_spawned_process() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create tokio runtime");

    runtime.block_on(async {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn long-running child process");

        let started = Instant::now();
        let status = kill_and_wait_child(&mut child, "unit-test child")
            .await
            .expect("kill_and_wait_child should terminate child");
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(2),
            "kill_and_wait_child should terminate quickly, elapsed {elapsed:?}"
        );
        assert!(
            !status.success(),
            "killed process should not report a success exit status"
        );
    });
}

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
    let source = include_str!("../../../scripts/harness/rollouts/internal-backend-smoke.sh");

    assert!(source.contains("RESTREAM_INTERNAL_HEVC_TO_H264=1"));
    assert!(source.contains("ONLY_CHECKS=load,ffprobe,stage-sharing"));
    assert!(source.contains("MIXED_OUTPUT_GROUPS=rtmp.720p.a0,rtmp.720p.a1"));
}

#[test]
fn mixed_signal_skips_rtmp_hevc_publish_probe_rows() {
    let source = include_str!("mixed_checks.rs");

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
