//! `srt.slow-peer`: one genuinely slow APPLICATION receiver among healthy SRT
//! outputs.
//!
//! The slow receiver (`RawSrtSink`) handshakes and accepts media normally, then
//! PAUSES APPLICATION DELIVERY while its protocol core keeps receiving, running
//! timers and generating ACK/NAK/control. That is receiver-window backpressure
//! on a live connection -- not a frozen (SIGSTOPped) peer, which
//! `fault.srt-output-stall` covers separately. Healthy siblings go to the
//! harness's fast SRT sink. SRT egress always runs at least two shards (the
//! product floors the CPU-derived shard count at 2), so even a one-CPU run
//! spreads outputs over two Owners by rendezvous hash; the slow leaf shares its
//! Owner and caller socket with the healthy outputs hashed to the same shard.
//! (`shardId` in the output health row is not a reliable leaf -> shard
//! attribution, so this mode does not claim one.)
//!
//! Environment: `SLOW_PEER_HEALTHY` (default 10), `SLOW_PEER_WATCH_SECS` (30),
//! `SLOW_PEER_LABEL` (artifact name), `SLOW_PEER_PAUSE` (1; 0 = control run).

use super::*;

fn env_u(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Sum every `payloadBytes` under a `payloadStats` object anywhere in `value`:
/// the retained media in the shared source rings.
fn ring_payload_bytes(value: &Value) -> u64 {
    match value {
        Value::Object(map) => {
            let own = map
                .get("payloadStats")
                .and_then(|stats| stats.get("payloadBytes"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            own + map.values().map(ring_payload_bytes).sum::<u64>()
        }
        Value::Array(items) => items.iter().map(ring_payload_bytes).sum(),
        _ => 0,
    }
}

pub(crate) async fn srt_slow_peer() -> Result<Value, String> {
    let healthy_count = env_u("SLOW_PEER_HEALTHY", 10) as usize;
    let watch = Duration::from_secs(env_u("SLOW_PEER_WATCH_SECS", 30));
    let label = std::env::var("SLOW_PEER_LABEL").unwrap_or_else(|_| "slow-peer".to_string());
    let work_dir = artifact_path("srt.slow-peer");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    let ports = TestPorts::from_env();
    let base = harness_port_defaults().ffmpeg_srt_sink_base;
    let healthy_port = base.checked_add(2000).ok_or("healthy sink port overflow")?;
    let slow_port = base.checked_add(2001).ok_or("slow sink port overflow")?;

    let healthy_sink = HarnessSrtSinkPool::start(&[healthy_port], 8 * 1024 * 1024, 2)?;
    let slow_sink = RawSrtSink::start(slow_port)?;
    let (mut child, api) = start_restream_api(
        &default_restream_bin(),
        &ports,
        &work_dir.join("slow-peer.sqlite"),
        &work_dir.join("restream.log"),
    )
    .await?;

    let stream_key = "slow-peer-input";
    let pid = create_pipeline_with_stream_key(&api, "slow-peer", stream_key).await?;
    let fixture = restream::test_fixtures::bench_transport_fixture("h264", "1.5M", false)?;
    let mut publisher = spawn_publisher(
        &fixture,
        &harness_srt_ffmpeg_url(ports.srt, stream_key, HarnessSrtMode::Publish, None),
        "mpegts",
        false,
    )
    .await?;
    wait_for_api_input_live(&api, &pid, Duration::from_secs(45)).await?;

    let mut healthy_ids = Vec::new();
    for index in 0..healthy_count {
        let oid = create_output(
            &api,
            &pid,
            &format!("healthy-{index:02}"),
            &harness_srt_output_url(
                healthy_port,
                &format!("healthy-{index:02}"),
                HarnessSrtMode::Publish,
            ),
            "source",
        )
        .await?;
        start_output(&api, &pid, &oid).await?;
        healthy_ids.push(oid);
    }
    let slow_id = create_output(
        &api,
        &pid,
        "slow",
        &harness_srt_output_url(slow_port, "slow", HarnessSrtMode::Publish),
        "source",
    )
    .await?;
    start_output(&api, &pid, &slow_id).await?;
    let mut all_ids = healthy_ids.clone();
    all_ids.push(slow_id.clone());
    wait_for_outputs_progress(&api, &pid, &all_ids, Duration::from_secs(60)).await?;
    // Real delivery into the slow sink first, so a pass is not "never connected".
    let ramp_deadline = Instant::now() + Duration::from_secs(30);
    while slow_sink.observe().data_events == 0 && Instant::now() < ramp_deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let before_pause = slow_sink.observe();
    if before_pause.data_events == 0 {
        return Err("the slow receiver never received application data before the pause".into());
    }
    tokio::time::sleep(Duration::from_secs(3)).await;

    // ---- pause application delivery on exactly one receiver
    // `SLOW_PEER_PAUSE=0` is the control run: identical shape, no pause.
    slow_sink.set_paused(env_u("SLOW_PEER_PAUSE", 1) == 1);
    let paused_at = Instant::now();
    let data_at_pause = slow_sink.observe().data_events;
    let mut samples: Vec<Value> = Vec::new();
    let mut prev: HashMap<String, u64> = HashMap::new();
    let mut window_stalls: Vec<String> = Vec::new();
    let mut retried_or_failed: Vec<String> = Vec::new();
    let mut ring_first: Option<u64> = None;
    let mut ring_max = 0u64;
    let mut owner_faulted = false;
    let mut second = 0u64;
    while paused_at.elapsed() < watch {
        tokio::time::sleep(Duration::from_secs(1)).await;
        second += 1;
        let health = api.get_json("/api/v1/engine/health").await?;
        let telemetry = api
            .get_json("/api/v1/engine/telemetry")
            .await
            .unwrap_or(Value::Null);
        let ring = ring_payload_bytes(&telemetry);
        ring_first.get_or_insert(ring);
        ring_max = ring_max.max(ring);
        if let Ok(system) = api.get_json("/metrics/system").await
            && let Some(shards) = system["egressShards"].as_array()
        {
            for shard in shards.iter().filter(|s| s["protocol"] == "srt") {
                for owner in shard["srtOwners"].as_array().into_iter().flatten() {
                    owner_faulted |= owner["faulted"].as_bool().unwrap_or(false);
                }
            }
        }
        let row = |oid: &str| &health["pipelines"][&pid]["outputs"][oid];
        let mut healthy_advanced = 0;
        for oid in &healthy_ids {
            let status = ApiOutputStatus::from_value(oid, row(oid))?;
            let before = prev.get(oid).copied().unwrap_or(0);
            if status.bytes_out > before {
                healthy_advanced += 1;
            }
            // Every 5 s a healthy output must have advanced across the window.
            if second.is_multiple_of(5)
                && status.bytes_out <= prev.get(&format!("{oid}@5")).copied().unwrap_or(0)
            {
                window_stalls.push(format!("{oid}@{second}s"));
            }
            if second.is_multiple_of(5) {
                prev.insert(format!("{oid}@5"), status.bytes_out);
            }
            if status.retrying
                || status.status == "retrying"
                || status.status == "failed"
                || status.status == "stalled"
            {
                retried_or_failed.push(format!("{oid}:{}@{second}s", status.status));
            }
            prev.insert(oid.clone(), status.bytes_out);
        }
        let slow_row = row(&slow_id);
        let slow_status = ApiOutputStatus::from_value(&slow_id, slow_row)?;
        samples.push(json!({
            "t": second,
            "healthyAdvancing": healthy_advanced,
            "slow": {
                "bytesOut": slow_status.bytes_out, "status": slow_status.status, "phase": slow_status.phase,
                "backpressureReason": slow_row["backpressureReason"],
                "feedLagUnits": slow_row["feedLagUnits"],
                "pendingBytes": slow_row["pendingBytes"],
                "packetsSentDrop": slow_row["quality"]["packetsSentDrop"],
                "retryAttempts": slow_status.retry_attempts,
            },
            "ringPayloadBytes": ring,
            "slowSinkDataEvents": slow_sink.observe().data_events,
        }));
    }
    let data_during_pause = slow_sink.observe().data_events - data_at_pause;
    // Healthy siblings still live and progressing at the end.
    let final_check =
        wait_for_outputs_live_and_progressing(&api, &pid, &healthy_ids, Duration::from_secs(15))
            .await;
    let slow_connected_at_end = slow_sink.observe().connected_now;
    slow_sink.set_paused(false);
    let ring_first = ring_first.unwrap_or(0);
    let advancing_min = samples
        .iter()
        .skip(2)
        .map(|s| s["healthyAdvancing"].as_u64().unwrap_or(0))
        .min()
        .unwrap_or(0);
    let passed = window_stalls.is_empty()
        && retried_or_failed.is_empty()
        && !owner_faulted
        && final_check.is_ok();
    let result = json!({
        "mode": "srt.slow-peer",
        "label": label,
        "healthyOutputs": healthy_count,
        "watchSecs": watch.as_secs(),
        "restreamThreadsNote": "one shard when RESTREAM_BIN pins Restream to a single CPU",
        "slowSinkDataEventsBeforePause": before_pause.data_events,
        "slowSinkDataEventsDuringPause": data_during_pause,
        "slowSinkConnectedAtEnd": slow_connected_at_end,
        "healthyMinAdvancingPerSecond": advancing_min,
        "healthyWindowStalls": window_stalls,
        "healthyRetryOrFailed": retried_or_failed,
        "ownerFaulted": owner_faulted,
        "feedRingPayloadBytes": {"first": ring_first, "max": ring_max},
        "finalHealthyCheck": final_check.as_ref().map(|_| "ok").unwrap_or_else(|e| e.as_str()),
        "samples": samples,
        "passed": passed,
    });
    std::fs::write(
        work_dir.join(format!("{label}.json")),
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    stop_child(&mut publisher).await;
    stop_child(&mut child).await;
    slow_sink.stop();
    healthy_sink.stop();
    if passed {
        Ok(result)
    } else {
        Err(format!("srt.slow-peer: isolation gate failed: {result}"))
    }
}
