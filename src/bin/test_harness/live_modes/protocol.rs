use super::*;

async fn start_local_mediamtx(
    config_path: &Path,
    log_path: &Path,
    ports: HarnessPortDefaults,
) -> Result<Child, String> {
    std::fs::write(
        config_path,
        format!(
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nrtmp: yes\nrtmpAddress: :{}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{}\nhls: yes\nhlsAddress: :{}\nhlsPartDuration: 200ms\nhlsSegmentDuration: 2s\nwebrtc: no\nmoq: no\napi: yes\napiAddress: :{}\nmetrics: no\npaths:\n  all:\n",
            ports.mtx_rtmp, ports.mtx_srt, ports.mtx_hls, ports.mtx_api
        ),
    )
    .map_err(|e| e.to_string())?;
    let log = std::fs::File::create(log_path).map_err(|e| e.to_string())?;
    let stderr_log = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new("mediamtx");
    let mut child = remove_mediamtx_config_env(&mut command)
        .arg(config_path)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr_log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/v3/paths/list", ports.mtx_api),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut child).await;
        return Err(format!("mediamtx did not become ready: {err}"));
    }
    Ok(child)
}

async fn install_bframe_transcode_profiles(api: &RampApi) -> Result<(), String> {
    let settings = api.get_json("/api/v1/settings").await?;
    let mut profiles: restream::domain::transcode_profile::TranscodeProfiles =
        serde_json::from_value(settings["transcodeProfiles"].clone())
            .map_err(|error| format!("parse transcode profiles: {error}"))?;

    for (name, bframes) in [("h264_bf0", 0usize), ("h264_bf2", 2usize)] {
        profiles.insert(
            name.to_string(),
            restream::domain::transcode_profile::TranscodeProfile {
                preset: "veryfast".to_string(),
                tune: String::new(),
                crf: 23,
                gop: 60,
                bframes,
                bitrate: 0,
                max_bitrate: 0,
                width: 0,
                height: 0,
            },
        );
    }

    api.patch_json("/api/v1/settings", json!({ "transcodeProfiles": profiles }))
        .await?;
    Ok(())
}

#[derive(Clone, Copy)]
enum ExpectedBframeSignal {
    None,
    Present,
}

async fn run_transcode_bframe_probe_case(
    api: &RampApi,
    pipeline_id: &str,
    work_dir: &Path,
    mediamtx_rtmp_port: u16,
    label: &str,
    encoding: &str,
    expected_signal: ExpectedBframeSignal,
) -> Result<Value, String> {
    let stream_name = format!("e2e-bframe-{label}");
    let publish_url = format!("rtmp://127.0.0.1:{mediamtx_rtmp_port}/live/{stream_name}");
    let output_id = create_output(api, pipeline_id, label, &publish_url, encoding).await?;
    if let Err(error) = start_output(api, pipeline_id, &output_id).await {
        stop_mixed_outputs(api, pipeline_id, std::slice::from_ref(&output_id)).await;
        return Err(format!("{label}: start output failed: {error}"));
    }

    let probe = wait_for_probe_shape(
        label,
        &publish_url,
        None,
        "h264",
        1,
        Duration::from_secs(30),
    )
    .await;
    let packet_path = work_dir.join(format!("{label}-packets.json"));
    let packet_probe = ffprobe_video_packets(&publish_url, &packet_path).await;
    stop_mixed_outputs(api, pipeline_id, std::slice::from_ref(&output_id)).await;

    let probe = probe?;
    let packet_probe = packet_probe?;
    let packet_count = count_video_packets(&packet_probe);
    let bframe_count = count_bframe_packets(&packet_probe);
    let dts_monotone = video_dts_monotone(&packet_probe);
    let bframe_signal_ok = match expected_signal {
        ExpectedBframeSignal::None => bframe_count == 0,
        ExpectedBframeSignal::Present => bframe_count > 0,
    };
    let passed = packet_count >= 30 && dts_monotone && bframe_signal_ok;

    let mut result = json!({
        "passed": passed,
        "encoding": encoding,
        "readUrl": publish_url,
        "packetArtifact": packet_path,
        "packetCount": packet_count,
        "bframeCount": bframe_count,
        "dtsMonotone": dts_monotone,
        "expectedBframes": match expected_signal {
            ExpectedBframeSignal::None => 0,
            ExpectedBframeSignal::Present => 2,
        },
        "probe": probe,
    });
    if packet_count < 30 {
        result["error"] = json!(format!(
            "{label}: expected at least 30 video packets, got {packet_count}"
        ));
    } else if !bframe_signal_ok {
        result["error"] = match expected_signal {
            ExpectedBframeSignal::None => {
                json!(format!("{label}: expected no packets with PTS > DTS"))
            }
            ExpectedBframeSignal::Present => {
                json!(format!("{label}: expected packets with PTS > DTS"))
            }
        };
    } else if !dts_monotone {
        result["error"] = json!(format!("{label}: DTS values are not monotone"));
    }

    if passed {
        Ok(result)
    } else {
        Err(format!("{label}: transcode B-frame probe failed: {result}"))
    }
}

fn srt_publish_url(port: u16, stream_key: &str, crypto: Option<(&str, u32)>) -> String {
    harness_srt_ffmpeg_url(port, stream_key, HarnessSrtMode::Publish, crypto)
}

fn srt_read_url(port: u16, stream_key: &str, crypto: Option<(&str, u32)>) -> String {
    harness_srt_ffmpeg_url(port, stream_key, HarnessSrtMode::Read, crypto)
}

async fn expect_ingest_rejected(
    api: &RampApi,
    pipeline_id: &str,
    fixture: &Path,
    publish_url: &str,
    label: &str,
) -> Result<Value, String> {
    let mut publisher = spawn_publisher(fixture, publish_url, "mpegts", true).await?;
    tokio::time::sleep(Duration::from_secs(4)).await;
    let live = wait_for_api_input_live(api, pipeline_id, Duration::from_secs(1))
        .await
        .is_ok();
    stop_child(&mut publisher).await;
    if live {
        return Err(format!("{label}: ingest unexpectedly went live"));
    }
    wait_for_api_input_off(api, pipeline_id, Duration::from_secs(5)).await?;
    Ok(json!({"passed": true, "label": label}))
}

async fn expect_srt_read_failure(url: &str, label: &str) -> Result<Value, String> {
    match ffprobe(url).await {
        Ok(probe) => Err(format!("{label}: read unexpectedly succeeded: {probe}")),
        Err(error) => Ok(json!({"passed": true, "label": label, "error": error})),
    }
}

async fn create_srt_policy_pipeline(
    api: &RampApi,
    name: &str,
    policy: Value,
) -> Result<String, String> {
    create_srt_policy_pipeline_with_key(api, name, name, policy).await
}

async fn create_srt_policy_pipeline_with_key(
    api: &RampApi,
    name: &str,
    stream_key: &str,
    policy: Value,
) -> Result<String, String> {
    let pipeline = api
        .post_json(
            "/api/v1/pipelines",
            json!({"name": name, "streamKey": stream_key, "srtIngestPolicy": policy}),
        )
        .await?;
    pipeline["pipeline"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{name} pipeline id missing"))
}

pub(crate) async fn srt_policy_correctness() -> Result<Value, String> {
    let work_dir = artifact_path("srt.policy");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let ports = TestPorts::from_env();

    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture = checked_h264_fixture()?;

    let mut results = serde_json::Map::new();

    api.patch_json(
        "/api/v1/settings",
        json!({"srtIngest": {"mode": "plaintext", "pbkeylen": 16, "passphrase": null}}),
    )
    .await?;
    let plain_inherit_id =
        create_srt_policy_pipeline(&api, "policy-plain-inherit", json!({"mode": "inherit"}))
            .await?;
    let mut plain_pub = spawn_publisher(
        &fixture,
        &srt_publish_url(ports.srt, "policy-plain-inherit", None),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(&api, &plain_inherit_id, Duration::from_secs(15)).await?;
    let plain_read_probe = ffprobe(&srt_read_url(ports.srt, "policy-plain-inherit", None)).await?;
    assert_media_only(&plain_read_probe, "plain inherit read")?;
    stop_child(&mut plain_pub).await;
    wait_for_api_input_off(&api, &plain_inherit_id, Duration::from_secs(10)).await?;
    results.insert(
        "globalPlaintextInherit".to_string(),
        json!({"passed": true, "readProbe": plain_read_probe}),
    );

    api.patch_json(
        "/api/v1/settings",
        json!({"srtIngest": {"mode": "encrypted", "passphrase": "globalpass123", "pbkeylen": 16}}),
    )
    .await?;
    let global_enc_id =
        create_srt_policy_pipeline(&api, "policy-global-enc", json!({"mode": "inherit"})).await?;
    let mut global_enc_pub = spawn_publisher(
        &fixture,
        &srt_publish_url(ports.srt, "policy-global-enc", Some(("globalpass123", 16))),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(&api, &global_enc_id, Duration::from_secs(15)).await?;
    let global_enc_read = ffprobe(&srt_read_url(
        ports.srt,
        "policy-global-enc",
        Some(("globalpass123", 16)),
    ))
    .await?;
    assert_media_only(&global_enc_read, "global encrypted read")?;
    let global_enc_read_fail = expect_srt_read_failure(
        &srt_read_url(ports.srt, "policy-global-enc", None),
        "global encrypted plaintext read",
    )
    .await?;
    stop_child(&mut global_enc_pub).await;
    wait_for_api_input_off(&api, &global_enc_id, Duration::from_secs(10)).await?;
    let global_enc_publish_fail = expect_ingest_rejected(
        &api,
        &global_enc_id,
        &fixture,
        &srt_publish_url(ports.srt, "policy-global-enc", None),
        "global encrypted plaintext publish",
    )
    .await?;
    results.insert(
        "globalEncrypted16Inherit".to_string(),
        json!({
            "passed": true,
            "readProbe": global_enc_read,
            "plaintextReadRejected": global_enc_read_fail,
            "plaintextPublishRejected": global_enc_publish_fail,
        }),
    );

    let plain_override_id =
        create_srt_policy_pipeline(&api, "policy-plain-override", json!({"mode": "plaintext"}))
            .await?;
    let mut plain_override_pub = spawn_publisher(
        &fixture,
        &srt_publish_url(ports.srt, "policy-plain-override", None),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(&api, &plain_override_id, Duration::from_secs(15)).await?;
    let plain_override_read =
        ffprobe(&srt_read_url(ports.srt, "policy-plain-override", None)).await?;
    assert_media_only(&plain_override_read, "plain override read")?;
    stop_child(&mut plain_override_pub).await;
    wait_for_api_input_off(&api, &plain_override_id, Duration::from_secs(10)).await?;
    results.insert(
        "globalEncrypted16PipelinePlaintext".to_string(),
        json!({"passed": true, "readProbe": plain_override_read}),
    );

    api.patch_json(
        "/api/v1/settings",
        json!({"srtIngest": {"mode": "plaintext", "pbkeylen": 16, "passphrase": null}}),
    )
    .await?;
    for (label, stream_key, passphrase, pbkeylen) in [
        (
            "pipelineEncrypted24",
            "policy-enc-24",
            "pipepass1234",
            24u32,
        ),
        (
            "pipelineEncrypted32",
            "policy-enc-32",
            "pipepass12345",
            32u32,
        ),
    ] {
        let pipeline_id = create_srt_policy_pipeline_with_key(
            &api,
            label,
            stream_key,
            json!({"mode": "encrypted", "passphrase": passphrase, "pbkeylen": pbkeylen}),
        )
        .await?;
        let mut pub_ok = spawn_publisher(
            &fixture,
            &srt_publish_url(ports.srt, stream_key, Some((passphrase, pbkeylen))),
            "mpegts",
            true,
        )
        .await?;
        wait_for_api_input_live(&api, &pipeline_id, Duration::from_secs(15)).await?;
        let read_ok = ffprobe(&srt_read_url(
            ports.srt,
            stream_key,
            Some((passphrase, pbkeylen)),
        ))
        .await?;
        assert_media_only(&read_ok, label)?;
        let read_plain_fail = expect_srt_read_failure(
            &srt_read_url(ports.srt, stream_key, None),
            &format!("{label} plaintext read"),
        )
        .await?;
        let read_wrong_pass_fail = expect_srt_read_failure(
            &srt_read_url(ports.srt, stream_key, Some(("wrongpass123", pbkeylen))),
            &format!("{label} wrong passphrase read"),
        )
        .await?;
        stop_child(&mut pub_ok).await;
        wait_for_api_input_off(&api, &pipeline_id, Duration::from_secs(10)).await?;
        let publish_plain_fail = expect_ingest_rejected(
            &api,
            &pipeline_id,
            &fixture,
            &srt_publish_url(ports.srt, stream_key, None),
            &format!("{label} plaintext publish"),
        )
        .await?;
        results.insert(
            label.to_string(),
            json!({
                "passed": true,
                "readProbe": read_ok,
                "plaintextReadRejected": read_plain_fail,
                "wrongPassphraseReadRejected": read_wrong_pass_fail,
                "plaintextPublishRejected": publish_plain_fail,
            }),
        );
    }

    stop_child(&mut child).await;
    let value = Value::Object(results);
    let path = work_dir.join("results.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    Ok(value)
}

pub(crate) async fn bframe_rtmp_correctness() -> Result<Value, String> {
    let work_dir = artifact_path("timestamp.bframe");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let mediamtx_config = work_dir.join("mediamtx.yml");
    let mediamtx_log = work_dir.join("mediamtx.log");
    let all_ports = harness_port_defaults();
    let sink_port = harness_port_defaults().sink;
    let ports = TestPorts::from_env();

    let mut mediamtx = start_local_mediamtx(&mediamtx_config, &mediamtx_log, all_ports).await?;
    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let pipeline_id =
        create_pipeline_with_stream_key(&api, "B-frame RTMP source", "e2e-bframe-src").await?;

    let sink_url = format!("rtmp://127.0.0.1:{sink_port}/live/e2e-bframe-sink");
    let output_id = create_output(&api, &pipeline_id, "bframe-sink", &sink_url, "source").await?;

    let sink_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink_server = start_generalized_sink_server(sink_port, sink_metrics.clone()).await?;

    let fixture = checked_h264_fixture()?;

    let mut publisher = spawn_publisher(
        &fixture,
        &format!("rtmp://127.0.0.1:{}/live/e2e-bframe-src", ports.rtmp),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(&api, &pipeline_id, Duration::from_secs(15)).await?;
    println!("[timestamp.bframe] Source ingest established");

    start_output(&api, &pipeline_id, &output_id).await?;

    let deadline = Instant::now() + Duration::from_secs(15);
    while sink_metrics.video_count.load(Ordering::Relaxed) < 30 {
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    let packets_path = work_dir.join("bframe-packets.json");
    let read_url = format!("rtmp://127.0.0.1:{}/live/e2e-bframe-src", ports.rtmp);
    let packet_probe = ffprobe_video_packets(&read_url, &packets_path).await?;
    let packet_count = count_video_packets(&packet_probe);
    let bframe_count = count_bframe_packets(&packet_probe);
    let ffprobe_dts_monotone = video_dts_monotone(&packet_probe);

    let sink_dts_monotone = sink_metrics.dts_monotone();
    let video_count = sink_metrics.video_count.load(Ordering::Relaxed);
    let sink_summary = sink_metrics.summary();

    let source_passed =
        packet_count >= 30 && bframe_count > 0 && ffprobe_dts_monotone && sink_dts_monotone;
    let mut source_results = json!({
        "passed": source_passed,
        "packetCount": packet_count,
        "bframeCount": bframe_count,
        "ffprobeDtsMonotone": ffprobe_dts_monotone,
        "sinkDtsMonotone": sink_dts_monotone,
        "sinkVideoCount": video_count,
        "sink": sink_summary,
    });
    if packet_count < 30 {
        source_results["error"] = json!(format!(
            "expected at least 30 video packets, got {packet_count}"
        ));
    } else if bframe_count == 0 {
        source_results["error"] = json!("RTMP egress did not expose any packets with PTS > DTS");
    } else if !ffprobe_dts_monotone || !sink_dts_monotone {
        source_results["error"] = json!("RTMP egress DTS values are not monotone");
    }

    install_bframe_transcode_profiles(&api).await?;
    let transcode_bframes_0 = run_transcode_bframe_probe_case(
        &api,
        &pipeline_id,
        &work_dir,
        all_ports.mtx_rtmp,
        "h264-bf0",
        "h264_bf0",
        ExpectedBframeSignal::None,
    )
    .await?;
    let transcode_bframes_2 = run_transcode_bframe_probe_case(
        &api,
        &pipeline_id,
        &work_dir,
        all_ports.mtx_rtmp,
        "h264-bf2",
        "h264_bf2",
        ExpectedBframeSignal::Present,
    )
    .await?;

    stop_child(&mut publisher).await;
    stop_generalized_sink_server(sink_server);
    stop_child(&mut child).await;
    stop_child(&mut mediamtx).await;

    let passed = source_passed
        && transcode_bframes_0["passed"].as_bool().unwrap_or(false)
        && transcode_bframes_2["passed"].as_bool().unwrap_or(false);
    let results = json!({
        "passed": passed,
        "sourcePassthrough": source_results,
        "transcodeBframes0": transcode_bframes_0,
        "transcodeBframes2": transcode_bframes_2,
    });

    let path = work_dir.join("results.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&results).unwrap())
        .map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&results).unwrap());
    if passed {
        Ok(results)
    } else {
        Err(format!("RTMP B-frame round-trip failed: {results}"))
    }
}
