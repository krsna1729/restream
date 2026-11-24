//! `srt.slow-peer`: one genuinely slow APPLICATION receiver among healthy SRT
//! outputs.
//!
//! The slow receiver (`RawSrtSink`) handshakes and accepts media normally, then
//! PAUSES APPLICATION DELIVERY while its protocol core keeps receiving, running
//! timers and generating ACK/NAK/control. That is receiver-window backpressure
//! on a live connection -- not a frozen (SIGSTOPped) peer, which
//! `fault.srt-output-stall` covers separately. Healthy siblings go to the
//! harness's fast SRT sink. SRT starts with one shard and grows from measured
//! service demand. With `SLOW_PEER_EXACT_OWNER=1` the run requires one live SRT
//! shard before and after the pause, so every output (and the slow one)
//! provably lives on that shard, hence the same IPv4 Owner and caller socket.
//! (Placement is least-loaded, so on more shards an output's shard cannot be
//! computed from its id; `shardId` in the output health row is not a reliable
//! leaf -> shard attribution either.)
//!
//! Environment: `SLOW_PEER_HEALTHY` (default 10), `SLOW_PEER_WATCH_SECS` (30),
//! `SLOW_PEER_EXACT_OWNER` (1: require the one-shard topology above),
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

/// Live SRT shard count for the (single) feed: distinct `shardIndex` values in
/// `/metrics/system` `egressShards`.
async fn live_srt_shard_count(api: &RampApi) -> Result<u32, String> {
    let system = api.get_json("/metrics/system").await?;
    let mut shards: Vec<u64> = system["egressShards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|shard| shard["protocol"] == "srt")
        .filter_map(|shard| shard["shardIndex"].as_u64())
        .collect();
    shards.sort_unstable();
    shards.dedup();
    u32::try_from(shards.len()).map_err(|e| e.to_string())
}

/// The target shard's IPv4 Owner counters from `/metrics/system`.
async fn target_owner(api: &RampApi, target: u32) -> Value {
    let Ok(system) = api.get_json("/metrics/system").await else {
        return Value::Null;
    };
    system["egressShards"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|shard| {
            shard["protocol"] == "srt" && shard["shardIndex"].as_u64() == Some(u64::from(target))
        })
        .and_then(|shard| shard["srtOwners"].as_array())
        .and_then(|owners| owners.iter().find(|owner| owner["family"] == "v4"))
        .cloned()
        .unwrap_or(Value::Null)
}

/// The exact-owner topology: every output on the one live SRT shard.
async fn require_one_srt_shard(api: &RampApi, when: &str) -> Result<(), String> {
    match live_srt_shard_count(api).await? {
        1 => Ok(()),
        shards => Err(format!(
            "exact owner needs one live SRT shard {when}, found {shards}"
        )),
    }
}

pub(crate) async fn srt_slow_peer() -> Result<Value, String> {
    let mut run = start_slow_peer().await?;

    // Assert the topology immediately before fault injection.
    let (exact_topology, owner_at_start) = if run.exact_owner {
        exact_owner_topology(&run).await?
    } else {
        (Value::Null, Value::Null)
    };

    let watch = watch_pause(&run).await?;
    let data_during_pause = run.slow_sink.observe().data_events - watch.data_at_pause;
    // Healthy siblings still live and progressing at the end.
    let final_check = wait_for_outputs_live_and_progressing(
        &run.api,
        &run.pid,
        &run.healthy_ids,
        Duration::from_secs(15),
    )
    .await;
    let (owner_at_end, owner_productive) = finish_owner_evidence(&run, &owner_at_start).await?;
    let slow_connected_at_end = run.slow_sink.observe().connected_now;
    run.slow_sink.set_paused(false);
    let ring_first = watch.ring_first.unwrap_or(0);
    let advancing_min = watch
        .samples
        .iter()
        .skip(2)
        .map(|s| s["healthyAdvancing"].as_u64().unwrap_or(0))
        .min()
        .unwrap_or(0);
    let passed = watch.window_stalls.is_empty()
        && watch.retried_or_failed.is_empty()
        && !watch.owner_faulted
        && owner_productive
        && final_check.is_ok();
    let result = json!({
        "mode": "srt.slow-peer",
        "label": run.label.clone(),
        "healthyOutputs": run.healthy_count,
        "watchSecs": run.watch.as_secs(),
        "topologyNote": "SRT starts with one shard and sizes from output demand; exact-owner assertions use the live topology, not a CPU-derived floor",
        "exactOwner": exact_topology,
        "targetOwnerAtPauseStart": owner_at_start,
        "targetOwnerAtPauseEnd": owner_at_end,
        "slowSinkDataEventsBeforePause": run.before_pause.data_events,
        "slowSinkDataEventsDuringPause": data_during_pause,
        "slowSinkConnectedAtEnd": slow_connected_at_end,
        "healthyMinAdvancingPerSecond": advancing_min,
        "healthyWindowStalls": watch.window_stalls,
        "healthyRetryOrFailed": watch.retried_or_failed,
        "ownerFaulted": watch.owner_faulted,
        "feedRingPayloadBytes": {"first": ring_first, "max": watch.ring_max},
        "finalHealthyCheck": final_check.as_ref().map(|_| "ok").unwrap_or_else(|e| e.as_str()),
        "samples": watch.samples,
        "passed": passed,
    });
    std::fs::write(
        run.work_dir.join(format!("{}.json", run.label)),
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    stop_child(&mut run.publisher).await;
    stop_child(&mut run.child).await;
    run.slow_sink.stop();
    run.healthy_sink.stop();
    if passed {
        Ok(result)
    } else {
        Err(format!("srt.slow-peer: isolation gate failed: {result}"))
    }
}

/// One slow peer run's handles and the pre-pause baseline.
struct SlowPeerRun {
    api: RampApi,
    child: Child,
    publisher: Child,
    healthy_sink: HarnessSrtSinkPool,
    slow_sink: RawSrtSink,
    pid: String,
    healthy_ids: Vec<String>,
    slow_id: String,
    label: String,
    watch: Duration,
    healthy_count: usize,
    work_dir: PathBuf,
    before_pause: RawSrtSinkObservation,
    exact_owner: bool,
}

/// Start the sinks, Restream, the ingest publisher and all outputs, then ramp
/// real delivery into the slow sink.
async fn start_slow_peer() -> Result<SlowPeerRun, String> {
    let healthy_count = env_u("SLOW_PEER_HEALTHY", 10) as usize;
    let watch = Duration::from_secs(env_u("SLOW_PEER_WATCH_SECS", 30));
    let label = std::env::var("SLOW_PEER_LABEL").unwrap_or_else(|_| "slow-peer".to_string());
    let work_dir = artifact_path("srt.slow-peer");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    let ports = TestPorts::from_env();
    let base = harness_port_defaults().ffmpeg_srt_sink_base;
    let healthy_port = base.checked_add(2000).ok_or("healthy sink port overflow")?;
    let slow_port = base.checked_add(2001).ok_or("slow sink port overflow")?;

    let healthy_sink = HarnessSrtSinkPool::start(&[healthy_port], 8 * 1024 * 1024)?;
    let slow_sink = RawSrtSink::start(slow_port)?;
    let (child, api) = start_restream_api(
        &default_restream_bin(),
        &ports,
        &work_dir.join("slow-peer.sqlite"),
        &work_dir.join("restream.log"),
    )
    .await?;

    let stream_key = "slow-peer-input";
    let pid = create_pipeline_with_stream_key(&api, "slow-peer", stream_key).await?;
    let fixture = restream_engine::test_fixtures::bench_transport_fixture("h264", "1.5M", false)?;
    let publisher = spawn_publisher(
        &fixture,
        &harness_srt_ffmpeg_url(ports.srt, stream_key, HarnessSrtMode::Publish, None),
        "mpegts",
        false,
    )
    .await?;
    wait_for_api_input_live(&api, &pid, Duration::from_secs(45)).await?;

    let exact_owner = env_u("SLOW_PEER_EXACT_OWNER", 0) == 1;
    let healthy_ids = start_healthy_outputs(&api, &pid, healthy_count, healthy_port).await?;
    let slow_id = start_slow_output(&api, &pid, slow_port).await?;
    let mut all_ids = healthy_ids.clone();
    all_ids.push(slow_id.clone());
    wait_for_outputs_progress(&api, &pid, &all_ids, Duration::from_secs(60)).await?;
    let before_pause = ramp_slow_delivery(&slow_sink).await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    Ok(SlowPeerRun {
        api,
        child,
        publisher,
        healthy_sink,
        slow_sink,
        pid,
        healthy_ids,
        slow_id,
        label,
        watch,
        healthy_count,
        work_dir,
        before_pause,
        exact_owner,
    })
}

async fn start_healthy_outputs(
    api: &RampApi,
    pid: &str,
    count: usize,
    healthy_port: u16,
) -> Result<Vec<String>, String> {
    let mut healthy_ids = Vec::new();
    for index in 0..count {
        let oid = create_output(
            api,
            pid,
            &format!("healthy-{index:02}"),
            &harness_srt_output_url(
                healthy_port,
                &format!("healthy-{index:02}"),
                HarnessSrtMode::Publish,
            ),
            "source",
        )
        .await?;
        start_output(api, pid, &oid).await?;
        healthy_ids.push(oid);
    }
    Ok(healthy_ids)
}

async fn start_slow_output(api: &RampApi, pid: &str, slow_port: u16) -> Result<String, String> {
    let slow_id = create_output(
        api,
        pid,
        "slow",
        &harness_srt_output_url(slow_port, "slow", HarnessSrtMode::Publish),
        "source",
    )
    .await?;
    start_output(api, pid, &slow_id).await?;
    Ok(slow_id)
}

/// Wait for real delivery into the slow sink, so a pass is not "never
/// connected".
async fn ramp_slow_delivery(slow_sink: &RawSrtSink) -> Result<RawSrtSinkObservation, String> {
    let ramp_deadline = Instant::now() + Duration::from_secs(30);
    while slow_sink.observe().data_events == 0 && Instant::now() < ramp_deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let before_pause = slow_sink.observe();
    if before_pause.data_events == 0 {
        return Err("the slow receiver never received application data before the pause".into());
    }
    Ok(before_pause)
}

/// The one-shard topology and the target Owner's counters before the pause.
async fn exact_owner_topology(run: &SlowPeerRun) -> Result<(Value, Value), String> {
    require_one_srt_shard(&run.api, "before the pause").await?;
    let topology = json!({
        "liveSrtShardCount": 1,
        "slowOutputId": run.slow_id,
        "healthyOutputIds": run.healthy_ids,
        "addressFamily": "V4",
        "inference": "same feed + one SRT shard + same address family = same Compio runtime, same IPv4 Owner, same shared caller socket",
    });
    let owner_at_start = target_owner(&run.api, 0).await;
    Ok((topology, owner_at_start))
}

/// Sampling state across the pause window.
struct PauseWatch {
    samples: Vec<Value>,
    prev: HashMap<String, u64>,
    window_stalls: Vec<String>,
    retried_or_failed: Vec<String>,
    ring_first: Option<u64>,
    ring_max: u64,
    owner_faulted: bool,
    data_at_pause: u64,
}

impl PauseWatch {
    fn new(data_at_pause: u64) -> Self {
        Self {
            samples: Vec::new(),
            prev: HashMap::new(),
            window_stalls: Vec::new(),
            retried_or_failed: Vec::new(),
            ring_first: None,
            ring_max: 0,
            owner_faulted: false,
            data_at_pause,
        }
    }

    /// Sample one second of the pause window.
    async fn sample_second(&mut self, run: &SlowPeerRun, second: u64) -> Result<(), String> {
        let health = run.api.get_json("/api/v1/engine/health").await?;
        let telemetry = run
            .api
            .get_json("/api/v1/engine/telemetry")
            .await
            .unwrap_or(Value::Null);
        let ring = ring_payload_bytes(&telemetry);
        self.ring_first.get_or_insert(ring);
        self.ring_max = self.ring_max.max(ring);
        self.observe_owner_faults(&run.api).await;
        let row = |oid: &str| &health["pipelines"][&run.pid]["outputs"][oid];
        let mut healthy_advanced = 0;
        for oid in &run.healthy_ids {
            let status = ApiOutputStatus::from_value(oid, row(oid))?;
            if self.track_healthy(oid, &status, second) {
                healthy_advanced += 1;
            }
            self.prev.insert(oid.clone(), status.bytes_out);
        }
        let slow_row = row(&run.slow_id);
        let slow_status = ApiOutputStatus::from_value(&run.slow_id, slow_row)?;
        self.samples.push(json!({
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
            "slowSinkDataEvents": run.slow_sink.observe().data_events,
        }));
        Ok(())
    }

    /// Track one healthy output's progress and record every 5 s window stall
    /// and retry/failed status. Returns whether it advanced since the last
    /// second.
    fn track_healthy(&mut self, oid: &str, status: &ApiOutputStatus, second: u64) -> bool {
        let before = self.prev.get(oid).copied().unwrap_or(0);
        // Every 5 s a healthy output must have advanced across the window.
        if second.is_multiple_of(5)
            && status.bytes_out <= self.prev.get(&format!("{oid}@5")).copied().unwrap_or(0)
        {
            self.window_stalls.push(format!("{oid}@{second}s"));
        }
        if second.is_multiple_of(5) {
            self.prev.insert(format!("{oid}@5"), status.bytes_out);
        }
        if status.retrying
            || status.status == "retrying"
            || status.status == "failed"
            || status.status == "stalled"
        {
            self.retried_or_failed
                .push(format!("{oid}:{}@{second}s", status.status));
        }
        status.bytes_out > before
    }

    async fn observe_owner_faults(&mut self, api: &RampApi) {
        if let Ok(system) = api.get_json("/metrics/system").await
            && let Some(shards) = system["egressShards"].as_array()
        {
            for shard in shards.iter().filter(|s| s["protocol"] == "srt") {
                for owner in shard["srtOwners"].as_array().into_iter().flatten() {
                    self.owner_faulted |= owner["faulted"].as_bool().unwrap_or(false);
                }
            }
        }
    }
}

/// Pause application delivery on the one slow receiver and sample the window.
async fn watch_pause(run: &SlowPeerRun) -> Result<PauseWatch, String> {
    // `SLOW_PEER_PAUSE=0` is the control run: identical shape, no pause.
    run.slow_sink.set_paused(env_u("SLOW_PEER_PAUSE", 1) == 1);
    let paused_at = Instant::now();
    let mut watch = PauseWatch::new(run.slow_sink.observe().data_events);
    let mut second = 0u64;
    while paused_at.elapsed() < run.watch {
        tokio::time::sleep(Duration::from_secs(1)).await;
        second += 1;
        watch.sample_second(run, second).await?;
    }
    Ok(watch)
}

/// The target Owner after the pause, and whether it stayed productive.
async fn finish_owner_evidence(
    run: &SlowPeerRun,
    owner_at_start: &Value,
) -> Result<(Value, bool), String> {
    if !run.exact_owner {
        return Ok((Value::Null, true));
    }
    let owner_at_end = target_owner(&run.api, 0).await;
    let productive = require_one_srt_shard(&run.api, "after the pause")
        .await
        .is_ok()
        && owner_at_end["faulted"] == false
        && owner_at_end["txCompletedOk"].as_u64().unwrap_or(0)
            > owner_at_start["txCompletedOk"].as_u64().unwrap_or(0)
        && owner_at_end["txPackets"].as_u64().unwrap_or(0)
            > owner_at_start["txPackets"].as_u64().unwrap_or(0);
    Ok((owner_at_end, productive))
}
