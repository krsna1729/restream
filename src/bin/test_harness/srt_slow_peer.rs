//! `srt.slow-peer`: one genuinely slow APPLICATION receiver among healthy SRT
//! outputs.
//!
//! The slow receiver (`RawSrtSink`) handshakes and accepts media normally, then
//! PAUSES APPLICATION DELIVERY while its protocol core keeps receiving, running
//! timers and generating ACK/NAK/control. That is receiver-window backpressure
//! on a live connection -- not a frozen (SIGSTOPped) peer, which
//! `fault.srt-output-stall` covers separately. Healthy siblings go to the
//! harness's fast SRT sink. SRT egress always runs at least two shards
//! (`default_egress_fabric_shards` clamps the CPU-derived count to 2..=8), so
//! even one effective CPU gives TWO shards. With `SLOW_PEER_EXACT_OWNER=1` the
//! healthy siblings are chosen with the production
//! `assign_output_to_shard` and the LIVE shard count, so every one of them
//! (and the slow output) provably lives on the same shard, hence the same IPv4
//! Owner and caller socket. (`shardId` in the output health row is not used: it
//! is not a reliable leaf -> shard attribution.)
//!
//! Environment: `SLOW_PEER_HEALTHY` (default 10), `SLOW_PEER_WATCH_SECS` (30),
//! `SLOW_PEER_EXACT_OWNER` (1: choose healthy siblings that the production
//! rendezvous function assigns to the slow output's exact shard),
//! `SLOW_PEER_LABEL` (artifact name), `SLOW_PEER_PAUSE` (1; 0 = control run).

use super::*;
use restream::media::egress::command::OutputId;
use restream::media::egress::manager::assign_output_to_shard;

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

fn shard_of(output_id: &str, live_shards: u32) -> Result<u32, String> {
    let count = std::num::NonZeroU32::new(live_shards).ok_or("no live SRT shards")?;
    Ok(assign_output_to_shard(&OutputId::new(output_id), count).index())
}

/// Create candidate healthy outputs until `want` of them are assigned, by the
/// production rendezvous function and the LIVE shard count, to the slow
/// output's shard; candidates that hash elsewhere are deleted unstarted and
/// never join the fault population. Bounded: fails rather than accept fewer.
async fn select_same_shard_siblings(
    api: &RampApi,
    pipeline_id: &str,
    slow_id: &str,
    healthy_port: u16,
    want: usize,
    shards: u32,
) -> Result<(u32, Vec<String>, usize, Value), String> {
    const CANDIDATE_CAP: usize = 128;
    let target = shard_of(slow_id, shards)?;
    let mut kept = Vec::new();
    let mut computed = serde_json::Map::new();
    let mut created = 0usize;
    while kept.len() < want {
        if created >= CANDIDATE_CAP {
            return Err(format!(
                "only {} of {want} same-shard siblings after {CANDIDATE_CAP} candidates",
                kept.len()
            ));
        }
        created += 1;
        let name = format!("healthy-{created:03}");
        let oid = create_output(
            api,
            pipeline_id,
            &name,
            &harness_srt_output_url(healthy_port, &name, HarnessSrtMode::Publish),
            "source",
        )
        .await?;
        if shard_of(&oid, shards)? == target {
            start_output(api, pipeline_id, &oid).await?;
            computed.insert(oid.clone(), json!(target));
            kept.push(oid);
        } else {
            api.delete_json(&format!("/api/v1/pipelines/{pipeline_id}/outputs/{oid}"))
                .await?;
        }
    }
    Ok((target, kept, created, Value::Object(computed)))
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

    let exact_owner = env_u("SLOW_PEER_EXACT_OWNER", 0) == 1;
    let mut healthy_ids = Vec::new();
    let slow_id;
    let mut exact_topology = Value::Null;
    if exact_owner {
        // Slow output first, so the live shard count and its computed shard are
        // known before any sibling is chosen.
        slow_id = create_output(
            &api,
            &pid,
            "slow",
            &harness_srt_output_url(slow_port, "slow", HarnessSrtMode::Publish),
            "source",
        )
        .await?;
        start_output(&api, &pid, &slow_id).await?;
        wait_for_outputs_progress(
            &api,
            &pid,
            std::slice::from_ref(&slow_id),
            Duration::from_secs(60),
        )
        .await?;
        let shards = live_srt_shard_count(&api).await?;
        let (target, kept, created, computed) =
            select_same_shard_siblings(&api, &pid, &slow_id, healthy_port, healthy_count, shards)
                .await?;
        healthy_ids = kept;
        exact_topology = json!({
            "liveSrtShardCount": shards,
            "targetShard": target,
            "slowOutputId": slow_id,
            "slowOutputComputedShard": shard_of(&slow_id, shards)?,
            "healthyOutputIds": healthy_ids,
            "healthyComputedShards": computed,
            "candidatesCreated": created,
            "addressFamily": "V4",
            "inference": "same feed + same SRT shard + same address family = same Compio runtime, same IPv4 Owner, same shared caller socket",
        });
    } else {
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
        slow_id = create_output(
            &api,
            &pid,
            "slow",
            &harness_srt_output_url(slow_port, "slow", HarnessSrtMode::Publish),
            "source",
        )
        .await?;
        start_output(&api, &pid, &slow_id).await?;
    }
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

    // Assert the topology is unchanged immediately before fault injection.
    let target_shard = exact_topology["targetShard"].as_u64().map(|v| v as u32);
    let mut owner_at_start = Value::Null;
    if exact_owner {
        let shards_now = live_srt_shard_count(&api).await?;
        if u64::from(shards_now) != exact_topology["liveSrtShardCount"].as_u64().unwrap_or(0) {
            return Err(format!(
                "live SRT shard count changed to {shards_now} before the pause"
            ));
        }
        for oid in healthy_ids.iter().chain(std::iter::once(&slow_id)) {
            if Some(shard_of(oid, shards_now)?) != target_shard {
                return Err(format!("{oid} no longer computes to the target shard"));
            }
        }
        owner_at_start = target_owner(&api, target_shard.unwrap_or(0)).await;
    }

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
    let mut owner_at_end = Value::Null;
    let mut owner_productive = true;
    if exact_owner {
        let shards_end = live_srt_shard_count(&api).await?;
        owner_at_end = target_owner(&api, target_shard.unwrap_or(0)).await;
        owner_productive = shards_end as u64
            == exact_topology["liveSrtShardCount"].as_u64().unwrap_or(0)
            && owner_at_end["faulted"] == false
            && owner_at_end["txCompletedOk"].as_u64().unwrap_or(0)
                > owner_at_start["txCompletedOk"].as_u64().unwrap_or(0)
            && owner_at_end["txPackets"].as_u64().unwrap_or(0)
                > owner_at_start["txPackets"].as_u64().unwrap_or(0);
    }
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
        && owner_productive
        && final_check.is_ok();
    let result = json!({
        "mode": "srt.slow-peer",
        "label": label,
        "healthyOutputs": healthy_count,
        "watchSecs": watch.as_secs(),
        "topologyNote": "SRT egress floors its shard count at 2 (default_egress_fabric_shards clamps to 2..=8): one effective CPU still gives TWO shards",
        "exactOwner": exact_topology,
        "targetOwnerAtPauseStart": owner_at_start,
        "targetOwnerAtPauseEnd": owner_at_end,
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
