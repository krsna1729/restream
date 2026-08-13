use super::super::*;

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
fn proc_net_has_bound_udp_port_matches_any_state() {
    // UDP has no LISTEN state: `/proc/net/udp`'s state column reads `07`
    // (TCP_CLOSE) the moment a bind() succeeds — presence of the local
    // port is itself the readiness signal, unlike the TCP LISTEN check.
    let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n\
   0: 0100007F:C4C1 00000000:0000 07 00000000:00000000 00:00000000 00000000   100        0 1 1 0000000000000000 100\n";

    assert!(proc_net_has_bound_udp_port(table, 50369));
    assert!(!proc_net_has_bound_udp_port(table, 1935));
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
