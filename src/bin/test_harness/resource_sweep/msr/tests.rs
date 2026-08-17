use super::*;

#[test]
fn canonical_plan_has_exact_zipf_and_protocol_totals() {
    let plan = msr_output_plan();
    assert_eq!(MSR_RANK_COUNTS.iter().sum::<usize>(), MSR_TOTAL_OUTPUTS);
    assert_eq!(plan.len(), MSR_TOTAL_OUTPUTS);
    assert_eq!(
        plan.iter()
            .filter(|output| output.protocol == MsrProtocol::Rtmp)
            .count(),
        MSR_RTMP_OUTPUTS
    );
    assert_eq!(
        plan.iter()
            .filter(|output| output.protocol == MsrProtocol::Rtmp)
            .filter(|output| output.rtmp_mode == RtmpOutputMode::Enhanced)
            .count(),
        MSR_RTMP_OUTPUTS
    );
    assert_eq!(
        msr_plan_json(
            &plan,
            &[30],
            MsrProtocolMix::Canonical,
            MsrRunProfile::Canonical
        )["outputs"]["enhancedRtmp"]
            .as_u64(),
        Some(MSR_RTMP_OUTPUTS as u64)
    );
    assert_eq!(
        plan.iter()
            .filter(|output| output.protocol == MsrProtocol::Srt)
            .count(),
        MSR_SRT_OUTPUTS
    );
}

#[test]
fn protocol_mix_can_generate_isolated_plans() {
    let rtmp_plan = msr_output_plan_for_mix(MsrProtocolMix::RtmpOnly);
    assert_eq!(
        rtmp_plan
            .iter()
            .filter(|output| output.protocol == MsrProtocol::Rtmp)
            .count(),
        MSR_TOTAL_OUTPUTS
    );
    assert!(
        rtmp_plan
            .iter()
            .all(|output| output.rtmp_mode == RtmpOutputMode::Enhanced)
    );
    assert_eq!(
        rtmp_plan
            .iter()
            .filter(|output| output.protocol == MsrProtocol::Srt)
            .count(),
        0
    );

    let srt_plan = msr_output_plan_for_mix(MsrProtocolMix::SrtOnly);
    assert_eq!(
        srt_plan
            .iter()
            .filter(|output| output.protocol == MsrProtocol::Rtmp)
            .count(),
        0
    );
    assert_eq!(
        srt_plan
            .iter()
            .filter(|output| output.protocol == MsrProtocol::Srt)
            .count(),
        MSR_TOTAL_OUTPUTS
    );
}

#[test]
fn protocol_mix_parser_accepts_calibration_shapes() {
    assert_eq!(
        MsrProtocolMix::parse("canonical").unwrap(),
        MsrProtocolMix::Canonical
    );
    assert_eq!(
        MsrProtocolMix::parse("rtmp-only").unwrap(),
        MsrProtocolMix::RtmpOnly
    );
    assert_eq!(
        MsrProtocolMix::parse("srt-only").unwrap(),
        MsrProtocolMix::SrtOnly
    );
    assert_eq!(
        MsrProtocolMix::parse("srt-every:10").unwrap(),
        MsrProtocolMix::SrtEvery(10)
    );
    assert!(MsrProtocolMix::parse("srt-every:0").is_err());
    assert!(MsrProtocolMix::parse("banana").is_err());
}

#[test]
fn every_output_selects_its_rank_audio_track() {
    for output in msr_output_plan() {
        assert_eq!(output.language_code, MSR_LANGUAGE_CODES[output.rank - 1]);
        assert_eq!(output.language_name, MSR_LANGUAGE_NAMES[output.rank - 1]);
        assert_eq!(
            output.encoding,
            format!("source+atrack:{}", output.rank - 1)
        );
    }
}

#[test]
fn signal_calibration_keeps_full_shape_but_uses_two_track_oracle_fixture() {
    let plan = msr_output_plan_for_mix_and_profile(
        MsrProtocolMix::Canonical,
        MsrRunProfile::SignalCalibration,
    );

    assert_eq!(plan.len(), MSR_TOTAL_OUTPUTS);
    assert_eq!(
        plan.iter()
            .filter(|output| output.protocol == MsrProtocol::Rtmp)
            .count(),
        MSR_RTMP_OUTPUTS
    );
    assert_eq!(
        plan.iter()
            .filter(|output| output.protocol == MsrProtocol::Srt)
            .count(),
        MSR_SRT_OUTPUTS
    );
    assert!(
        plan.iter()
            .all(|output| output.name.starts_with("msr-signal-rank"))
    );
    assert!(plan.iter().all(|output| {
        output.encoding == "source+atrack:0" || output.encoding == "source+atrack:1"
    }));
    assert_eq!(MsrRunProfile::SignalCalibration.audio_tracks(), 2);
}

#[test]
fn signal_sample_selection_is_deterministic_and_includes_srt_when_present() {
    let plan = msr_output_plan();
    let samples = msr_signal_sample_outputs(&plan[..30], 4);

    assert_eq!(samples.len(), 4);
    assert_eq!(samples[0].ordinal, 1);
    assert!(
        samples
            .iter()
            .any(|output| output.protocol == MsrProtocol::Srt),
        "checkpoint sample should include the SRT output when one exists"
    );
}

#[test]
fn srt_outputs_use_mediamtx_standard_stream_id() {
    let env = ResourceSweepEnv {
        work_dir: PathBuf::from("."),
        summary_json: PathBuf::from("summary.json"),
        summary_csv: PathBuf::from("summary.csv"),
        samples_jsonl: PathBuf::from("samples.jsonl"),
        restream_log: PathBuf::from("restream.log"),
        mediamtx_log: PathBuf::from("mediamtx.log"),
        mediamtx_config: PathBuf::from("mediamtx.yml"),
        restream_bin: PathBuf::from("restream"),
        restream_db_path: PathBuf::from("restream.db"),
        restream_http: 3030,
        restream_rtmp: 1935,
        restream_srt: 10080,
        mtx_rtmp: 1936,
        mtx_rtmps: 1937,
        mtx_srt: 8891,
        mtx_api: 9997,
        peer_count: 1,
        peer_mode: ResourceSweepPeer::Mediamtx,
        srt_sink_backend: HarnessSrtSinkBackend::Libsrt,
        sample_secs: 1,
        sample_interval_ms: 1000,
        settle_secs: 1,
        ingest_counts: Vec::new(),
        egress_counts: Vec::new(),
        scenario_filter: None,
        lifecycle: ResourceSweepLifecycle::Continuous,
        no_cleanup: false,
        srt_crypto: HarnessSrtCrypto::plaintext(),
        backend_policy_env: Vec::new(),
        rtmps_tls: None,
    };
    let output = MsrOutputSpec {
        ordinal: 20,
        rank: 1,
        language_code: "eng",
        language_name: "English",
        protocol: MsrProtocol::Srt,
        rtmp_mode: RtmpOutputMode::Legacy,
        encoding: "source+atrack:0".to_string(),
        name: "msr-rank01-srt-0001".to_string(),
    };

    assert_eq!(
        msr_output_url(&env, &output),
        "srt://127.0.0.1:8891?streamid=#!::m=publish,r=msr-rank01-srt-0001"
    );
    assert_eq!(msr_mediamtx_path(&output), "msr-rank01-srt-0001");
}

#[test]
fn peer_instance_selection_round_robins_by_ordinal() {
    assert_eq!(msr_peer_instance(1, 1), 0);
    assert_eq!(msr_peer_instance(1200, 1), 0);
    assert_eq!(msr_peer_instance(1, 4), 1);
    assert_eq!(msr_peer_instance(4, 4), 0);
    assert_eq!(msr_peer_instance(5, 4), 1);
    assert_eq!(msr_peer_instance(1200, 4), 0);
    assert_eq!(msr_peer_instance(1, 1200), 1);
    assert_eq!(msr_peer_instance(1199, 1200), 1199);
    assert_eq!(msr_peer_instance(1200, 1200), 0);
    // peer_count == 0 must not panic (divide-by-zero); it clamps to 1.
    assert_eq!(msr_peer_instance(7, 0), 0);
}

#[test]
fn output_and_read_urls_target_the_same_instance_the_output_publishes_to() {
    let mut env = base_test_env();
    env.peer_count = 4;
    let output = MsrOutputSpec {
        ordinal: 5,
        rank: 1,
        language_code: "eng",
        language_name: "English",
        protocol: MsrProtocol::Rtmp,
        rtmp_mode: RtmpOutputMode::Enhanced,
        encoding: "source+atrack:0".to_string(),
        name: "msr-rank01-rtmp-0001".to_string(),
    };

    // ordinal 5 % 4 == instance 1 -> mtx_rtmp + 1 == 1937.
    assert_eq!(
        msr_output_url(&env, &output),
        "rtmp://127.0.0.1:1937/live/msr-rank01-rtmp-0001"
    );
    assert_eq!(
        msr_read_url(&env, &output),
        "rtmp://127.0.0.1:1937/live/msr-rank01-rtmp-0001"
    );
}

#[test]
fn expected_paths_group_by_the_instance_each_output_publishes_to() {
    let plan = msr_output_plan();
    let checkpoint = &plan[..8]; // ordinals 1..=8, mixed rtmp/srt except every-20th

    let groups = msr_group_expected_paths_by_instance(checkpoint, 4);

    // ordinals 1..=8 % 4 -> instances 1,2,3,0,1,2,3,0
    let mut instances = groups.iter().map(|(index, _)| *index).collect::<Vec<_>>();
    instances.sort_unstable();
    assert_eq!(instances, vec![0, 1, 2, 3]);
    let total_paths: usize = groups.iter().map(|(_, paths)| paths.len()).sum();
    assert_eq!(total_paths, checkpoint.len());

    // Single-instance grouping (peer_count == 1, the default) always yields
    // exactly one group covering every expected path.
    let single = msr_group_expected_paths_by_instance(checkpoint, 1);
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].0, 0);
    assert_eq!(single[0].1.len(), checkpoint.len());
}

fn base_test_env() -> ResourceSweepEnv {
    ResourceSweepEnv {
        work_dir: PathBuf::from("."),
        summary_json: PathBuf::from("summary.json"),
        summary_csv: PathBuf::from("summary.csv"),
        samples_jsonl: PathBuf::from("samples.jsonl"),
        restream_log: PathBuf::from("restream.log"),
        mediamtx_log: PathBuf::from("mediamtx.log"),
        mediamtx_config: PathBuf::from("mediamtx.yml"),
        restream_bin: PathBuf::from("restream"),
        restream_db_path: PathBuf::from("restream.db"),
        restream_http: 3030,
        restream_rtmp: 1935,
        restream_srt: 10080,
        mtx_rtmp: 1936,
        mtx_rtmps: 1937,
        mtx_srt: 8891,
        mtx_api: 9997,
        peer_count: 1,
        peer_mode: ResourceSweepPeer::Mediamtx,
        srt_sink_backend: HarnessSrtSinkBackend::Libsrt,
        sample_secs: 1,
        sample_interval_ms: 1000,
        settle_secs: 1,
        ingest_counts: Vec::new(),
        egress_counts: Vec::new(),
        scenario_filter: None,
        lifecycle: ResourceSweepLifecycle::Continuous,
        no_cleanup: false,
        srt_crypto: HarnessSrtCrypto::plaintext(),
        backend_policy_env: Vec::new(),
        rtmps_tls: None,
    }
}

#[test]
fn requested_mahashivratri_language_codes_are_present() {
    let required = [
        "eng", "tam", "hin", "tel", "kan", "mar", "nep", "ben", "mal", "guj", "ori", "ita", "spa",
        "fra", "deu", "rus", "por", "ara", "ind",
    ];
    for language_code in required {
        assert!(
            MSR_LANGUAGE_CODES.contains(&language_code),
            "missing required MSR language code {language_code}"
        );
    }
    assert_eq!(
        MSR_LANGUAGE_CODES
            .iter()
            .filter(|code| **code == "zho")
            .count(),
        2,
        "Simplified and Traditional Chinese both require zho entries"
    );
}

#[test]
fn report_includes_mediamtx_path_health_columns() {
    let mut aggregate = report_test_aggregate();
    aggregate.path_health = Some(MediaMtxPathHealth {
        expected_paths: 30,
        ready_paths: 30,
        reader_count: 0,
        paths_with_tracks: 30,
        inbound_frame_errors: 0,
        bytes_received_before: 1_000,
        bytes_received_after: 5_000_000,
        bytes_received_delta: 4_999_000,
        sample_secs: 3,
    });
    aggregate.post_sample_path_health = Some(MediaMtxPathHealth {
        expected_paths: 30,
        ready_paths: 30,
        reader_count: 0,
        paths_with_tracks: 30,
        inbound_frame_errors: 0,
        bytes_received_before: 5_000_000,
        bytes_received_after: 6_000_000,
        bytes_received_delta: 1_000_000,
        sample_secs: 2,
    });

    let report = format_msr_report(30, 30, 29, 29, 1, &[aggregate]);

    assert!(report.contains("MediaMTX ready"));
    assert!(report.contains("29 Enhanced RTMP"));
    assert!(report.contains("MediaMTX bytes delta"));
    assert!(report.contains("| 30 | rtmp:29,srt:1 | 30/30 |"));
    assert!(report.contains("loopback MediaMTX path API byte-growth proof"));
}

#[test]
fn report_uses_engine_health_wording_and_sink_columns_in_sink_mode() {
    let mut aggregate = report_test_aggregate();
    aggregate.path_health = None;
    aggregate.post_sample_path_health = None;
    aggregate.sink_verification = Some(MsrSinkVerification {
        outputs_expected: 30,
        outputs_present: 30,
        bytes_out_before: 1_000,
        bytes_out_after: 4_000_000,
        bytes_out_delta: 3_999_000,
        packets_sent_drop: 0,
        sample_secs: 2,
    });

    let report = format_msr_report(30, 30, 29, 29, 1, &[aggregate]);

    assert!(report.contains("loopback engine health API byte-growth proof"));
    assert!(report.contains("restream engine health API"));
    assert!(report.contains("| 30 | rtmp:29,srt:1 | 30/30 |"));
}

fn report_test_aggregate() -> MsrCheckpointAggregate {
    MsrCheckpointAggregate {
        resource: ResourceAggregate {
            scenario: MSR_MODE.to_string(),
            label: "30-outputs".to_string(),
            lifecycle: "continuous".to_string(),
            pipelines: 1,
            outputs: 30,
            ingest_types: "h264-srt-30a".to_string(),
            egress_mix: "rtmp:29,srt:1".to_string(),
            transcode: "no".to_string(),
            sample_count: 6,
            restream_cpu_avg_pct: 30.0,
            restream_cpu_peak_pct: 40.0,
            ffmpeg_cpu_avg_pct: 0.0,
            ffmpeg_cpu_peak_pct: 0.0,
            total_cpu_avg_pct: 32.1,
            total_cpu_peak_pct: 42.4,
            voluntary_ctxt_switches_avg_per_sec: 0.0,
            voluntary_ctxt_switches_peak_per_sec: 0.0,
            nonvoluntary_ctxt_switches_avg_per_sec: 0.0,
            nonvoluntary_ctxt_switches_peak_per_sec: 0.0,
            thread_count_peak: 0,
            rss_avg_kb: 90.0 * 1024.0,
            rss_peak_kb: 90 * 1024,
            ffmpeg_rss_peak_kb: 0,
            retained_peak_kb: 0,
            source_ring_peak_kb: 0,
            transcoder_ring_peak_kb: 0,
            tsmux_ring_peak_kb: 0,
            avio_len_peak_kb: 0,
            avio_hwm_peak_kb: 92,
            anonymous_peak_kb: 0,
            private_dirty_peak_kb: 0,
            shared_clean_peak_kb: 0,
            pss_peak_kb: 0,
            unattributed_peak_kb: 0,
            active_transcoder_buffers_peak: 0,
            ingests_peak: 1,
            egresses_peak: 30,
            stages_peak: 1,
            pipeline_count_peak: 1,
        },
        path_health: None,
        post_sample_path_health: None,
        sink_verification: None,
        ffprobe_checks: Vec::new(),
    }
}

#[test]
fn ffprobe_sample_selection_is_seeded_and_includes_srt_when_present() {
    let plan = msr_output_plan();
    let samples = msr_ffprobe_sample_outputs(&plan[..30], 4, 1234);

    assert_eq!(samples.len(), 4);
    assert!(
        samples
            .iter()
            .any(|output| output.protocol == MsrProtocol::Srt),
        "sampled correctness gate should include an SRT output when the checkpoint has one"
    );
    assert_eq!(samples, msr_ffprobe_sample_outputs(&plan[..30], 4, 1234));
}

#[test]
fn ffprobe_confidence_uses_without_replacement_detection_math() {
    let confidence = msr_ffprobe_detection_confidence(60, 1200, 0.05);

    assert!(
        confidence > 0.95,
        "60 samples should give >95% chance to catch at least one defect when >=5% are bad"
    );
}
