use super::*;

#[test]
fn runtime_log_noise_matcher_only_flags_decoder_noise_patterns() {
    assert!(mixed_runtime_log_noise_matches(
        "[hevc @ 0x1] PPS id out of range: 0"
    ));
    assert!(mixed_runtime_log_noise_matches(
        "[hevc @ 0x1] Could not find ref with POC 0"
    ));
    assert!(mixed_runtime_log_noise_matches(
        "[hevc @ 0x1] Error constructing the frame RPS."
    ));
    assert!(!mixed_runtime_log_noise_matches(
        "stage exit pipeline=pipe encoding=720p"
    ));
}

#[test]
fn mixed_output_group_selector_preserves_matrix_order_and_deduplicates() {
    let requested = parse_csv_env_values("rtmp.720p.a1, rtmp.720p.a0,rtmp.720p.a1");

    let selected = selected_mixed_output_cases(multi_track_mixed_output_cases(), Some(&requested))
        .expect("known output groups should select rows");

    assert_eq!(
        selected.iter().map(MixedOutputCase::id).collect::<Vec<_>>(),
        vec!["rtmp.720p.a0", "rtmp.720p.a1"]
    );
}

#[test]
fn mixed_output_group_selector_rejects_unknown_rows() {
    let requested = vec!["rtmp.missing".to_string()];

    let error = selected_mixed_output_cases(multi_track_mixed_output_cases(), Some(&requested))
        .expect_err("unknown output groups should fail before live setup");

    assert!(error.contains("MIXED_OUTPUT_GROUPS contains unknown output group"));
    assert!(error.contains("rtmp.720p.a0"));
}

#[test]
fn mixed_env_register_output_cell_writes_outputs_json() {
    let temp = std::env::temp_dir().join(format!(
        "restream-mixed-output-registry-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp).expect("temp dir");
    let env = MixedEnv::from_env_with_default_work_dir("mixed.registry", temp.clone());

    env.register_output_cell(HarnessOutputCell {
        scenario_id: "mixed.asset.file.h264.a1.bf0".to_string(),
        batch_group: "rtmp.source".to_string(),
        wave: 0,
        pipeline_id: "pipe".to_string(),
        output_id: "output-1".to_string(),
        output_name: "rtmp.source-1".to_string(),
        cell_id: "rtmp.source".to_string(),
        duplicate_index: 1,
        protocol: "rtmp".to_string(),
        encoding: "source".to_string(),
        rtmp_mode: Some(RtmpOutputMode::Legacy.as_str().to_string()),
        selected_audio_track: None,
        publish_url: "rtmp://127.0.0.1:1935/live/out".to_string(),
        read_url: None,
        expected_dimensions: Some("1920x1080".to_string()),
        expected_audio_tracks: Some(1),
        terminal_stage: None,
    })
    .expect("cell registered");

    let body = std::fs::read_to_string(env.outputs_json_path()).expect("outputs.json");
    let json: Value = serde_json::from_str(&body).expect("valid output registry json");
    assert_eq!(
        json["schemaVersion"],
        mixed_artifacts::MIXED_OUTPUTS_SCHEMA_VERSION
    );
    assert_eq!(json["outputs"][0]["outputId"], "output-1");
    assert_eq!(
        env.output_cell_label("output-1").expect("cell label"),
        "mixed.asset.file.h264.a1.bf0 / rtmp.source / out1"
    );

    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn matrix_progress_writes_root_cause_summary_artifact() {
    let temp = std::env::temp_dir().join(format!(
        "restream-mixed-root-cause-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp).expect("temp dir");
    let scenario_path = temp.join("scenario.json");
    let rows = matrix_case_progress_rows();
    let failures = vec![
        "mixed input case mixed.live.rtmp.h265.a2.bf2 failed: stream 0 has DTS gap 0.900000s"
            .to_string(),
    ];

    write_matrix_scenario_progress(&scenario_path, "shared-batch", false, &rows, &failures)
        .expect("progress json");

    let scenario_body = std::fs::read_to_string(&scenario_path).expect("scenario.json");
    let scenario: Value = serde_json::from_str(&scenario_body).expect("valid scenario json");
    assert_eq!(
        scenario["rootCauseSummary"]["causes"][0]["cause"],
        "timestamp_discontinuity"
    );
    assert_eq!(
        scenario["caseProgress"][0]["hlsPreviewTiming"],
        "before-fanout"
    );
    assert_eq!(
        scenario["caseProgress"][0]["probeSampling"]["policy"],
        "last-duplicate"
    );
    assert_eq!(
        scenario["caseProgress"][0]["supportedHlsPreviewTimings"],
        json!(["before-fanout", "after-progress", "disabled"])
    );
    assert_eq!(
        scenario["caseProgress"][0]["supportedProbeSamplingPolicies"],
        json!([
            "all-duplicates",
            "first-duplicate",
            "last-duplicate",
            "representative"
        ])
    );
    assert_eq!(
        scenario["artifacts"]["rootCauseSummaryJson"],
        temp.join("root-cause-summary.json")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(
        scenario["artifacts"]["artifactIndexJson"],
        temp.join("artifact-index.json").to_string_lossy().as_ref()
    );

    let summary_body =
        std::fs::read_to_string(temp.join("root-cause-summary.json")).expect("root cause summary");
    let summary: Value = serde_json::from_str(&summary_body).expect("valid summary json");
    assert_eq!(summary["totalFailures"], 1);
    assert_eq!(
        summary["causes"][0]["scenarios"][0],
        "mixed.live.rtmp.h265.a2.bf2"
    );
    let index_body =
        std::fs::read_to_string(temp.join("artifact-index.json")).expect("artifact index");
    let index: Value = serde_json::from_str(&index_body).expect("valid artifact index");
    assert_eq!(index["mode"], MIXED_MATRIX_MODE);
    assert_eq!(index["scenarioJson"], json!(scenario_path));
    assert_eq!(
        index["rootCauseSummaryJson"],
        json!(temp.join("root-cause-summary.json"))
    );
    assert_eq!(
        index["cases"][0]["artifactIndexJson"],
        json!(temp.join("asset/file/h264/a1/bf0/artifact-index.json"))
    );
    assert_eq!(
        index["cases"][0]["outputsJson"],
        json!(temp.join("asset/file/h264/a1/bf0/outputs.json"))
    );
    assert_eq!(
        index["cases"][0]["sqliteSnapshotDir"],
        json!(temp.join("asset/file/h264/a1/bf0/sqlite-snapshot"))
    );
    assert_eq!(
        index["cases"][0]["media"],
        json!(temp.join("asset/file/h264/a1/bf0/media"))
    );

    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn runtime_log_noise_scan_scopes_to_pipeline_id() {
    let temp = std::env::temp_dir().join(format!(
        "restream-mixed-runtime-log-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp).expect("temp dir");
    let log_path = temp.join("restream.log");
    std::fs::write(
        &log_path,
        concat!(
            "INFO pipeline_id=pipe-ok normal line\n",
            "ERROR pipeline_id=pipe-bad [hevc @ 0x1] PPS id out of range: 0\n",
            "ERROR pipeline_id=pipe-other [hevc @ 0x1] PPS id out of range: 0\n"
        ),
    )
    .expect("log write");

    let matches = mixed_runtime_log_noise_lines(&log_path, "pipe-bad");
    assert_eq!(matches.len(), 1);
    assert!(matches[0].contains("pipe-bad"));

    std::fs::remove_file(&log_path).ok();
    std::fs::remove_dir_all(&temp).ok();
}
