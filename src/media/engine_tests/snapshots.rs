use super::*;

#[test]
fn pipe_metrics_snapshot_correctness() {
    let pm = PipeMetrics::default();
    let snap = pm.snapshot();

    // All counters start at zero; avg fields are also zero.
    assert_eq!(snap.stalls, 0);
    assert_eq!(snap.stall_us, 0);
    assert_eq!(snap.avg_stall_us, 0);
    assert_eq!(snap.idles, 0);
    assert_eq!(snap.idle_us, 0);
    assert_eq!(snap.avg_idle_us, 0);

    // Stdin stall accumulation and average.
    pm.record_stall(2_000);
    pm.record_stall(6_000);
    let snap = pm.snapshot();
    assert_eq!(snap.stalls, 2);
    assert_eq!(snap.stall_us, 8_000);
    assert_eq!(snap.avg_stall_us, 4_000);

    // Stdout idle accumulation and average.
    pm.record_idle(3_000);
    let snap = pm.snapshot();
    assert_eq!(snap.idles, 1);
    assert_eq!(snap.idle_us, 3_000);
    assert_eq!(snap.avg_idle_us, 3_000);

    // StageMetricsSnapshot is a fixed typed struct with no pipe-metrics
    // fields, so the two counter families can no longer be conflated at the
    // type level.
    let sm = StageMetrics::new();
    sm.record_in(64);
    let ssnap = sm.snapshot();
    assert_eq!(ssnap.packets_in, 1);
    assert_eq!(ssnap.bytes_in, 64);
}

#[tokio::test]
async fn health_snapshot_marks_outputs_stopped_without_ingest() {
    let engine = MediaEngine::new();
    engine
        .register_egress("output-1", "pipeline-1", "rtmp://example/live/test")
        .await;

    let snapshot =
        test_health_snapshot(&engine, &["pipeline-1".to_string()], &HashMap::new()).await;

    assert_eq!(
        snapshot["pipelines"]["pipeline-1"]["outputs"]["output-1"]["status"],
        "stopped"
    );
}

#[tokio::test]
async fn health_snapshot_marks_failed_egress_status_when_input_is_live() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipeline-1", "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .register_egress("output-1", "pipeline-1", "rtmp://example/live/test")
        .await;
    engine
        .record_egress_error("output-1", "send", "connection refused")
        .await;

    let snapshot =
        test_health_snapshot(&engine, &["pipeline-1".to_string()], &HashMap::new()).await;

    let output = &snapshot["pipelines"]["pipeline-1"]["outputs"]["output-1"];
    assert_eq!(output["status"], "failed");
    assert_eq!(output["rawStatus"], "running");
    assert_eq!(output["phase"], "failed");
    assert_eq!(output["failurePhase"], "send");
}

#[tokio::test]
async fn health_snapshot_marks_live_output_stalled_without_progress() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipeline-1", "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .register_egress("output-1", "pipeline-1", "rtmp://example/live/test")
        .await;
    {
        let mut egresses = engine.egresses.active.write().await;
        let egress = egresses.get_mut("output-1").unwrap();
        egress.start_instant = Instant::now()
            .checked_sub(std::time::Duration::from_millis(
                EGRESS_PROGRESS_STALE_MS + 1,
            ))
            .unwrap();
    }

    let snapshot =
        test_health_snapshot(&engine, &["pipeline-1".to_string()], &HashMap::new()).await;

    assert_eq!(
        snapshot["pipelines"]["pipeline-1"]["outputs"]["output-1"]["status"],
        "stalled"
    );
}

#[tokio::test]
async fn health_snapshot_keeps_local_hls_segmenter_running_without_bytes_out() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipeline-1", "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .register_egress("output-1", "pipeline-1", "hls://localhost/hls/test")
        .await;
    engine.update_egress_phase("output-1", EP::Segmenting).await;
    {
        let mut egresses = engine.egresses.active.write().await;
        let egress = egresses.get_mut("output-1").unwrap();
        egress.start_instant = Instant::now()
            .checked_sub(std::time::Duration::from_millis(
                EGRESS_PROGRESS_STALE_MS + 1,
            ))
            .unwrap();
    }

    let snapshot =
        test_health_snapshot(&engine, &["pipeline-1".to_string()], &HashMap::new()).await;

    let output = &snapshot["pipelines"]["pipeline-1"]["outputs"]["output-1"];
    assert_eq!(output["status"], "running");
    assert_eq!(output["phase"], "segmenting");
    assert_eq!(output["bytesOut"], 0);
}

#[tokio::test]
async fn health_snapshot_includes_all_ingest_audio_tracks() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipeline-audio", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_audio_tracks(
            "pipeline-audio",
            vec![
                AudioMeta {
                    codec: "aac".to_string(),
                    sample_rate: 48_000,
                    channels: 2,
                    channel_layout: None,
                    track_index: 0,
                    pid: Some(0x101),
                    language: Some("eng".to_string()),
                    title: None,
                    profile: None,
                },
                AudioMeta {
                    codec: "aac".to_string(),
                    sample_rate: 44_100,
                    channels: 1,
                    channel_layout: None,
                    track_index: 1,
                    pid: Some(0x102),
                    language: None,
                    title: None,
                    profile: None,
                },
            ],
        )
        .await;

    let snapshot =
        test_health_snapshot(&engine, &["pipeline-audio".to_string()], &HashMap::new()).await;
    let tracks = snapshot["pipelines"]["pipeline-audio"]["input"]["audioTracks"]
        .as_array()
        .unwrap();

    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0]["pid"], 0x101);
    assert_eq!(tracks[0]["language"], "eng");
    assert_eq!(tracks[1]["trackIndex"], 1);
}

#[tokio::test]
async fn health_snapshot_reports_probe_readiness() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipeline-probe", "stream-key", "srt")
        .await
        .unwrap();

    let pending =
        test_health_snapshot(&engine, &["pipeline-probe".to_string()], &HashMap::new()).await;
    let pending_input = &pending["pipelines"]["pipeline-probe"]["input"];
    assert_eq!(pending_input["probeReady"], false);
    assert_eq!(pending_input["probeStatus"], "pending");
    assert!(pending_input["probePendingMs"].as_u64().is_some());

    let video = Some(VideoMeta {
        codec: "h264".to_string(),
        width: 1920,
        height: 1080,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    });
    let audio = AudioMeta {
        track_index: 0,
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };
    engine
        .update_ingest_meta("pipeline-probe", video, Some(audio.clone()), None)
        .await;
    engine
        .update_ingest_audio_tracks("pipeline-probe", vec![audio])
        .await;

    let ready =
        test_health_snapshot(&engine, &["pipeline-probe".to_string()], &HashMap::new()).await;
    let ready_input = &ready["pipelines"]["pipeline-probe"]["input"];
    assert_eq!(ready_input["probeReady"], true);
    assert_eq!(ready_input["probeStatus"], "ready");
    assert!(ready_input["probePendingMs"].is_null());
}

#[tokio::test]
async fn health_snapshot_marks_hls_preview_active_when_consumer_exists() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-hls";

    let _ = engine.ensure_hls_preview_segmenter(pipeline_id).await;
    let snapshot = test_health_snapshot(&engine, &[pipeline_id.to_string()], &HashMap::new()).await;

    assert_eq!(
        snapshot["pipelines"][pipeline_id]["hlsPreview"]["active"],
        true
    );
}

#[tokio::test]
async fn health_snapshot_marks_cancelled_hls_preview_inactive() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-hls-cancelled";

    let _ = engine.ensure_hls_preview_segmenter(pipeline_id).await;
    let token = engine
        .get_hls_preview_cancel_token(pipeline_id)
        .await
        .unwrap();
    token.cancel();

    let snapshot = test_health_snapshot(&engine, &[pipeline_id.to_string()], &HashMap::new()).await;

    assert_eq!(
        snapshot["pipelines"][pipeline_id]["hlsPreview"]["active"],
        false
    );
}

#[tokio::test]
async fn health_and_graph_expose_reader_lag_overflow_and_packet_age() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-reader-metrics";
    let rb = engine.get_or_create_pipeline(pipeline_id).await;

    rb.push(test_video_packet(0, 0, true));
    let _reader = Reader::new("graph-reader".to_string(), rb.clone());
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    rb.push(test_audio_packet(10, 10));

    let snapshot = test_health_snapshot(&engine, &[pipeline_id.to_string()], &HashMap::new()).await;
    let reader_metrics = snapshot["pipelines"][pipeline_id]["input"]["readerMetrics"]
        .as_array()
        .unwrap();
    assert_eq!(reader_metrics.len(), 1);
    assert_eq!(reader_metrics[0]["name"], "graph-reader");
    assert_eq!(reader_metrics[0]["lagSlots"], 2);
    assert_eq!(reader_metrics[0]["overflowCount"], 0);
    assert!(
        !reader_metrics[0]["packetAgeMs"].is_null(),
        "health reader metrics should expose unread packet age"
    );

    let graph = crate::api_runtime_views::processing_graph(&engine, pipeline_id, &[]).await;
    let source = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["type"] == "ring_buffer")
        .unwrap();
    let graph_readers = source["details"]["readers"].as_array().unwrap();
    assert_eq!(graph_readers.len(), 1);
    assert_eq!(graph_readers[0]["lagSlots"], 2);
    assert_eq!(graph_readers[0]["overflowCount"], 0);
    assert!(
        !graph_readers[0]["packetAgeMs"].is_null(),
        "graph reader metrics should expose unread packet age"
    );
}

#[tokio::test]
async fn health_snapshot_exposes_bonding_and_member_telemetry() {
    let engine = MediaEngine::new();
    engine
        .runtime
        .listener_stats
        .bonding_available
        .store(true, Ordering::Relaxed);
    engine
        .try_register_ingest("pipeline-bond", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_publisher_quality(
            "pipeline-bond",
            PublisherQuality {
                srt_bonded: Some(true),
                srt_group_member_count: Some(2),
                srt_group_connected_members: Some(2),
                srt_group_active_members: Some(1),
                srt_group_broken_members: Some(0),
                ..PublisherQuality::default()
            },
        )
        .await;

    let snapshot =
        test_health_snapshot(&engine, &["pipeline-bond".to_string()], &HashMap::new()).await;
    let quality = &snapshot["pipelines"]["pipeline-bond"]["input"]["publisher"]["quality"];

    assert_eq!(snapshot["srtListener"]["bondingAvailable"], true);
    assert_eq!(quality["srtBonded"], true);
    assert_eq!(quality["srtGroupMemberCount"], 2);
    assert_eq!(quality["srtGroupConnectedMembers"], 2);
    assert_eq!(quality["srtGroupActiveMembers"], 1);
    assert_eq!(quality["srtGroupBrokenMembers"], 0);
}

#[tokio::test]
async fn health_snapshot_exposes_runtime_limit_and_rtmp_listener_errors() {
    let engine = MediaEngine::new();
    engine
        .runtime
        .rtmp_listener_stats
        .rtmp_accept_errors
        .store(7, Ordering::Relaxed);
    engine
        .runtime
        .rtmp_listener_stats
        .rtmp_fd_exhaustion_errors
        .store(3, Ordering::Relaxed);

    let snapshot = test_health_snapshot(&engine, &[], &HashMap::new()).await;

    assert_eq!(snapshot["rtmpListener"]["acceptErrors"], 7);
    assert_eq!(snapshot["rtmpListener"]["fdExhaustionErrors"], 3);
    assert_eq!(
        snapshot["runtimeLimits"]["nofile"]["configured"],
        engine.config.tuning.nofile_limit
    );
    let host_settings = snapshot["hostSettings"]
        .as_array()
        .expect("host settings should be an array");
    assert!(
        host_settings
            .iter()
            .any(|setting| setting["key"] == "runtime.nofile"),
        "host settings should expose the process nofile row"
    );
    assert!(
        host_settings
            .iter()
            .any(|setting| setting["key"] == "net.core.rmem_max"),
        "host settings should expose the SRT receive buffer ceiling row"
    );
    assert!(
        host_settings
            .iter()
            .any(|setting| setting["key"] == "net.core.wmem_max"),
        "host settings should expose the SRT send buffer ceiling row"
    );
    assert!(
        host_settings
            .iter()
            .any(|setting| setting["key"] == "runtime.tokio.worker_threads"),
        "host settings should expose Tokio worker sizing"
    );
    assert!(
        host_settings
            .iter()
            .any(|setting| setting["key"] == "runtime.tokio.max_blocking_threads"),
        "host settings should expose Tokio blocking-pool sizing"
    );
    assert!(
        !host_settings
            .iter()
            .any(|setting| setting["key"] == "kernel.perf_event_paranoid"),
        "host settings should not expose profiling-only settings"
    );
    assert!(
        snapshot["runtimeLimits"]["nofile"]
            .get("satisfied")
            .and_then(|value| value.as_bool())
            .is_some(),
        "nofile limit snapshot should expose whether the configured target is satisfied"
    );
}

#[tokio::test]
async fn health_summary_includes_runtime_host_settings() {
    let engine = MediaEngine::new();

    let summary = test_health_summary_snapshot(&engine).await;
    assert!(
        summary["hostSettings"]
            .as_array()
            .is_some_and(|settings| !settings.is_empty()),
        "summary health should expose runtime host settings"
    );
}
