//! `srt.admission`: drive one SRT shard pool past its admission ceiling
//! (Q-034).
//!
//! Restream runs with one egress shard per pool (`RESTREAM_EGRESS_SHARDS=1`)
//! and a ten-minute restart backoff, so an output refused once stays refused
//! for the watch. One 8 Mbit/s fixture feed fans out to `ADMISSION_OUTPUTS`
//! (default 96) SRT outputs, more than one shard admits. Gate: at least one
//! output is refused; every refused output reports `capacityRefused` at the
//! shard layer and sends nothing; every other output is live and keeps
//! progressing through `ADMISSION_WATCH_SECS` (default 20).

use super::*;

const RETRY_BACKOFF_MS: &str = "600000";

fn env_u(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
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

pub(crate) async fn srt_admission() -> Result<Value, String> {
    let requested = env_u("ADMISSION_OUTPUTS", 96) as usize;
    let watch = Duration::from_secs(env_u("ADMISSION_WATCH_SECS", 20));
    let work_dir = artifact_path("srt.admission");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    let ports = TestPorts::from_env();
    let sink_port = harness_port_defaults()
        .ffmpeg_srt_sink_base
        .checked_add(2002)
        .ok_or("sink port overflow")?;
    let sink = HarnessSrtSinkPool::start(&[sink_port], 8 * 1024 * 1024)?;
    let mut child = start_restream_child_with_env(
        &default_restream_bin(),
        &ports,
        &work_dir.join("admission.sqlite"),
        &work_dir.join("restream.log"),
        &[
            ("RESTREAM_EGRESS_SHARDS", "1".to_string()),
            (
                "RESTREAM_OUTPUT_RETRY_BASE_MS",
                RETRY_BACKOFF_MS.to_string(),
            ),
            ("RESTREAM_OUTPUT_RETRY_MAX_MS", RETRY_BACKOFF_MS.to_string()),
        ],
    )
    .await?;
    let api = login_api(&ports).await?;

    let stream_key = "admission-input";
    let pid = create_pipeline_with_stream_key(&api, "admission", stream_key).await?;
    let fixture = restream::test_fixtures::bench_transport_fixture("h264", "8M", false)?;
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
        let oid = create_output(
            &api,
            &pid,
            &name,
            &harness_srt_output_url(sink_port, &name, HarnessSrtMode::Publish),
            "source",
        )
        .await?;
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
    // and send nothing.
    let mut stalls = Vec::new();
    let mut unrefused = Vec::new();
    let mut refused_sent = Vec::new();
    let mut last: HashMap<String, u64> = HashMap::new();
    let started = Instant::now();
    while admitted_live.is_ok() && started.elapsed() < watch {
        tokio::time::sleep(Duration::from_secs(5)).await;
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
    let refusal_example = refused
        .first()
        .and_then(|oid| diagnoses.get(oid))
        .cloned()
        .unwrap_or(Value::Null);
    let passed = !refused.is_empty()
        && admitted_live.is_ok()
        && stalls.is_empty()
        && unrefused.is_empty()
        && refused_sent.is_empty();
    let result = json!({
        "mode": "srt.admission",
        "requested": requested,
        "admitted": admitted.len(),
        "refused": refused.len(),
        "refusalExample": refusal_example,
        "admittedLive": admitted_live.as_ref().map(|_| "ok").unwrap_or_else(|e| e.as_str()),
        "admittedStalls": stalls,
        "refusedThatRecovered": unrefused,
        "refusedThatSent": refused_sent,
        "watchSecs": watch.as_secs(),
        "passed": passed,
    });
    std::fs::write(
        work_dir.join("srt.admission.json"),
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    stop_child(&mut publisher).await;
    stop_child(&mut child).await;
    sink.stop();
    if passed {
        Ok(result)
    } else {
        Err(format!("srt.admission: overload gate failed: {result}"))
    }
}
