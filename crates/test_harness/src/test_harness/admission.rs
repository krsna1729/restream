//! `srt.admission` and `sink.admission`: drive one fabric's shard pool past
//! its admission ceiling.
//!
//! Restream runs with `ADMISSION_SHARDS` (default 1) egress shards at most per
//! pool (`RESTREAM_EGRESS_SHARDS`) and a ten-minute restart backoff, so an
//! output refused once stays refused for the watch. One 8 Mbit/s fixture feed
//! fans out to `ADMISSION_OUTPUTS` outputs of the fabric (default per shard:
//! 96 SRT; 4224 sink, past its 4,096-leaf table), added one at a time so the
//! pool grows across them.
//! Gate: at least one output is refused; every refused output reports
//! `capacityRefused` at the shard layer and sends nothing; every other output
//! is live and keeps progressing through `ADMISSION_WATCH_SECS` (default 20);
//! and no `egress-shard-<n>` thread runs at or above the 62% busy where one
//! shard began to degrade.

use super::*;
use crate::thread_cpu::{
    CpuTicks, clock_ticks_per_sec, cpu_secs_between, read_proc_stat_cpu, read_thread_comms,
    select_egress_shard_threads,
};
use std::collections::BTreeMap;

const RETRY_BACKOFF_MS: &str = "600000";
/// Busy at which one SRT shard began losing outputs (runtime-crossings, SRT
/// shard capacity).
const DEGRADED_BUSY: f64 = 0.62;

/// The fabric an admission run drives past its ceiling.
#[derive(Clone, Copy)]
pub(crate) enum AdmissionFabric {
    Srt,
    /// `sink://` discard outputs: so cheap that a shard's leaf table fills
    /// long before its CPU, so the run adds past the table.
    Sink,
}

impl AdmissionFabric {
    fn mode(self) -> &'static str {
        match self {
            Self::Srt => "srt.admission",
            Self::Sink => "sink.admission",
        }
    }

    fn default_outputs_per_shard(self) -> u64 {
        match self {
            Self::Srt => 96,
            Self::Sink => 4224,
        }
    }
}

fn env_u(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Shard index -> (tid, CPU ticks) for every egress shard thread.
fn shard_ticks(pid: u32) -> Result<BTreeMap<u32, (u32, CpuTicks)>, String> {
    select_egress_shard_threads(&read_thread_comms(pid)?)
        .into_iter()
        .map(|thread| {
            let path = PathBuf::from(format!("/proc/{pid}/task/{}/stat", thread.tid));
            Ok((thread.index, (thread.tid, read_proc_stat_cpu(&path)?)))
        })
        .collect()
}

/// Output id -> diagnosis cause code, for outputs with a diagnosis.
async fn cause_codes(api: &RampApi) -> Result<HashMap<String, Value>, String> {
    let state = api.get_json("/api/v1/state").await?;
    Ok(state["outputs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|output| !output["diagnosis"].is_null())
        .filter_map(|output| {
            Some((
                output["id"].as_str()?.to_string(),
                output["diagnosis"].clone(),
            ))
        })
        .collect())
}

pub(crate) async fn admission(fabric: AdmissionFabric) -> Result<Value, String> {
    let mode = fabric.mode();
    let shards = env_u("ADMISSION_SHARDS", 1);
    let requested = env_u(
        "ADMISSION_OUTPUTS",
        fabric.default_outputs_per_shard() * shards,
    ) as usize;
    let watch = Duration::from_secs(env_u("ADMISSION_WATCH_SECS", 20));
    let work_dir = artifact_path(mode);
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    let ports = TestPorts::from_env();
    // SRT outputs need a receiver: one sink thread per 32 outputs, so the
    // receiver is not what limits the outputs at a gigabit and more.
    let sink_base = harness_port_defaults()
        .ffmpeg_srt_sink_base
        .checked_add(2002)
        .ok_or("sink port overflow")?;
    let srt_sink_ports = match fabric {
        AdmissionFabric::Srt => (0..requested.div_ceil(32))
            .map(|i| u16::try_from(i).ok().and_then(|i| sink_base.checked_add(i)))
            .collect::<Option<Vec<u16>>>()
            .ok_or("sink port overflow")?,
        AdmissionFabric::Sink => Vec::new(),
    };
    let srt_sink = match fabric {
        AdmissionFabric::Srt => Some(HarnessSrtSinkPool::start(&srt_sink_ports, 8 * 1024 * 1024)?),
        AdmissionFabric::Sink => None,
    };
    let mut child = start_restream_child_with_env(
        &default_restream_bin(),
        &ports,
        &work_dir.join("admission.sqlite"),
        &work_dir.join("restream.log"),
        &[
            ("RESTREAM_EGRESS_SHARDS", shards.to_string()),
            (
                "RESTREAM_OUTPUT_RETRY_BASE_MS",
                RETRY_BACKOFF_MS.to_string(),
            ),
            ("RESTREAM_OUTPUT_RETRY_MAX_MS", RETRY_BACKOFF_MS.to_string()),
        ],
    )
    .await?;
    let api = login_api(&ports).await?;
    let restream_pid = child.id().ok_or("restream exited before admission")?;

    let stream_key = "admission-input";
    let pid = create_pipeline_with_stream_key(&api, "admission", stream_key).await?;
    let fixture = restream_engine::test_fixtures::bench_transport_fixture("h264", "8M", false)?;
    let mut publisher = spawn_publisher(
        &fixture,
        &harness_srt_ffmpeg_url(ports.srt, stream_key, HarnessSrtMode::Publish, None),
        "mpegts",
        false,
    )
    .await?;
    wait_for_api_input_live(&api, &pid, Duration::from_secs(45)).await?;
    // Let the feed publish long enough for the sizer's rate forecast.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut ids = Vec::with_capacity(requested);
    for index in 0..requested {
        let name = format!("adm-{index:03}");
        let url = match fabric {
            AdmissionFabric::Srt => {
                harness_srt_output_url(srt_sink_ports[index / 32], &name, HarnessSrtMode::Publish)
            }
            AdmissionFabric::Sink => format!("sink://local/{name}"),
        };
        let oid = create_output(&api, &pid, &name, &url, "source").await?;
        start_output(&api, &pid, &oid).await?;
        ids.push(oid);
    }
    tokio::time::sleep(Duration::from_secs(5)).await;

    let diagnoses = cause_codes(&api).await?;
    let is_refused = |oid: &String| {
        diagnoses.get(oid).is_some_and(|diagnosis| {
            diagnosis["cause"]["code"] == "capacityRefused" && diagnosis["layer"] == "shard"
        })
    };
    let (refused, admitted): (Vec<String>, Vec<String>) = ids.iter().cloned().partition(is_refused);
    let admitted_live = if admitted.is_empty() {
        Err("no output was admitted".to_string())
    } else {
        wait_for_outputs_live_and_progressing(&api, &pid, &admitted, Duration::from_secs(60)).await
    };

    // Watch: admitted outputs advance every 5 s; refused ones stay refused
    // and send nothing; no shard thread reaches the degradation busy.
    let mut stalls = Vec::new();
    let mut unrefused = Vec::new();
    let mut refused_sent = Vec::new();
    let mut last: HashMap<String, u64> = HashMap::new();
    let ticks_per_sec = clock_ticks_per_sec();
    let mut peak_busy: BTreeMap<u32, f64> = BTreeMap::new();
    let mut ticks = shard_ticks(restream_pid)?;
    let mut ticks_at = Instant::now();
    let started = Instant::now();
    while admitted_live.is_ok() && started.elapsed() < watch {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let now_ticks = shard_ticks(restream_pid)?;
        let elapsed = ticks_at.elapsed().as_secs_f64();
        for (index, (tid, after)) in &now_ticks {
            if let Some((before_tid, before)) = ticks.get(index)
                && before_tid == tid
                && let Some(secs) = cpu_secs_between(*before, *after, ticks_per_sec)
            {
                let busy = peak_busy.entry(*index).or_default();
                *busy = busy.max(secs / elapsed);
            }
        }
        (ticks, ticks_at) = (now_ticks, Instant::now());
        let health = api.get_json("/api/v1/engine/health").await?;
        let row = |oid: &str| &health["pipelines"][&pid]["outputs"][oid];
        for oid in &admitted {
            let status = ApiOutputStatus::from_value(oid, row(oid))?;
            if status.retrying || status.bytes_out <= last.get(oid).copied().unwrap_or(0) {
                stalls.push(format!("{oid}@{}s", started.elapsed().as_secs()));
            }
            last.insert(oid.clone(), status.bytes_out);
        }
        let diagnoses = cause_codes(&api).await?;
        for oid in &refused {
            let still_refused = diagnoses
                .get(oid)
                .is_some_and(|diagnosis| diagnosis["cause"]["code"] == "capacityRefused");
            if !still_refused {
                unrefused.push(oid.clone());
            }
            if ApiOutputStatus::from_value(oid, row(oid))?.bytes_out > 0 {
                refused_sent.push(oid.clone());
            }
        }
    }
    let overloaded: Vec<u32> = peak_busy
        .iter()
        .filter(|(_, busy)| **busy >= DEGRADED_BUSY)
        .map(|(index, _)| *index)
        .collect();
    let refusal_example = refused
        .first()
        .and_then(|oid| diagnoses.get(oid))
        .cloned()
        .unwrap_or(Value::Null);
    let passed = !refused.is_empty()
        && admitted_live.is_ok()
        && stalls.is_empty()
        && unrefused.is_empty()
        && refused_sent.is_empty()
        && !peak_busy.is_empty()
        && overloaded.is_empty();
    let result = json!({
        "mode": mode,
        "shardCeiling": shards,
        "requested": requested,
        "admitted": admitted.len(),
        "refused": refused.len(),
        "refusalExample": refusal_example,
        "admittedLive": admitted_live.as_ref().map(|_| "ok").unwrap_or_else(|e| e.as_str()),
        "admittedStalls": stalls,
        "refusedThatRecovered": unrefused,
        "refusedThatSent": refused_sent,
        "shardPeakBusy": peak_busy,
        "shardsAtDegradedBusy": overloaded,
        "watchSecs": watch.as_secs(),
        "passed": passed,
    });
    std::fs::write(
        work_dir.join(format!("{mode}.json")),
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    stop_child(&mut publisher).await;
    stop_child(&mut child).await;
    if let Some(sink) = srt_sink {
        sink.stop();
    }
    if passed {
        Ok(result)
    } else {
        Err(format!("{mode}: overload gate failed: {result}"))
    }
}
