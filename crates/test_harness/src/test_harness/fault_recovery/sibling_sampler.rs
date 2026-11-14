//! Consumer observations throughout a fault, not endpoint counter deltas.
use super::super::*;

pub(super) struct Victim {
    pub(super) pipeline: String,
    pub(super) output: String,
    pub(super) selected: String,
    pub(super) standby: String,
    pub(super) sink: SrtSinkCountersHandle,
}

pub(super) async fn watch(
    api: &RampApi,
    victims: &[Victim],
    fault: impl Future<Output = Result<Value, String>>,
) -> Result<Value, String> {
    static NEXT_CAPTURE: AtomicUsize = AtomicUsize::new(0);
    let path = artifact_path("fault.sibling-isolation").join(format!(
        "samples-{}.jsonl",
        NEXT_CAPTURE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut capture = std::fs::File::create(&path).map_err(|error| error.to_string())?;
    let start = Instant::now();
    let mut previous: Vec<_> = victims
        .iter()
        .map(|v| v.sink.snapshot().discarded_bytes)
        .collect();
    let mut progressed = vec![start; victims.len()];
    let mut max_gap = vec![0u128; victims.len()];
    let mut input_previous = vec![0u64; victims.len() * 2];
    let mut input_progressed = vec![start; victims.len() * 2];
    let mut input_max_gap = vec![0u128; victims.len() * 2];
    let mut samples = 0u64;
    let mut ticks = tokio::time::interval(Duration::from_millis(250));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut result = None;
    let mut finished = None;
    tokio::pin!(fault);
    loop {
        tokio::select! {
            value = &mut fault, if result.is_none() => {
                result = Some(value?);
                finished = Some(Instant::now());
            }
            _ = ticks.tick() => {
                let now = Instant::now();
                let mut delivered = Vec::new();
                for (i, victim) in victims.iter().enumerate() {
                    let sink = victim.sink.snapshot();
                    if sink.accepted != 1 || sink.closed != 0 {
                        return Err(format!("healthy consumer {} reconnected or disconnected: {sink:?}", victim.output));
                    }
                    let bytes = sink.discarded_bytes;
                    if now.duration_since(progressed[i]) > Duration::from_millis(1500) {
                        return Err(format!("consumer {i} observation gap exceeded 1500 ms"));
                    }
                    if bytes > previous[i] {
                        max_gap[i] = max_gap[i].max(now.duration_since(progressed[i]).as_millis());
                        progressed[i] = now;
                    }
                    previous[i] = bytes;
                    delivered.push(bytes);
                }
                // Bound the WHOLE observation, not each of three serial API
                // reads: their individual deadlines otherwise add up past the
                // delivery-gap bound under lifecycle logging load.
                let input_requests = victims.iter().map(|victim| async move {
                    api.get_json(&format!("/api/v1/pipelines/{}/inputs", victim.pipeline)).await
                });
                let (health, input_rows) = tokio::time::timeout(Duration::from_millis(750), async {
                    tokio::try_join!(
                        api.get_json("/api/v1/engine/health"),
                        futures_util::future::try_join_all(input_requests),
                    )
                }).await.map_err(|_| "whole victim observation exceeded 750 ms")??;
                let mut input_bytes = Vec::new();
                for (victim_index, victim) in victims.iter().enumerate() {
                    let pipeline = &health["pipelines"][&victim.pipeline];
                    let output = &pipeline["outputs"][&victim.output];
                    if pipeline["input"]["status"] != "on" || output["status"] != "running" || output["retrying"] != false {
                        return Err(format!("victim unhealthy during fault: {pipeline}"));
                    }
                    let inputs = &input_rows[victim_index];
                    if inputs["selectedInputId"] != victim.selected { return Err(format!("selection changed: {inputs}")); }
                    for (input_index, (id, state)) in [(&victim.selected, "active"), (&victim.standby, "standby")].into_iter().enumerate() {
                        let input = inputs["inputs"].as_array().and_then(|rows| rows.iter().find(|row| row["id"] == *id))
                            .ok_or_else(|| format!("victim input missing: {inputs}"))?;
                        if input["runtime"]["connected"] != true || input["runtime"]["forwardingState"] != state {
                            return Err(format!("victim input unhealthy: {input}"));
                        }
                        let index = victim_index * 2 + input_index;
                        let bytes = input["runtime"]["bytesReceived"].as_u64().ok_or("input bytes missing")?;
                        if input_progressed[index].elapsed() > Duration::from_millis(1500) {
                            return Err(format!("input {id} observation gap exceeded 1500 ms (previousBytes={}, currentBytes={bytes})", input_previous[index]));
                        }
                        if bytes > input_previous[index] {
                            input_max_gap[index] = input_max_gap[index].max(input_progressed[index].elapsed().as_millis());
                            input_progressed[index] = Instant::now();
                        }
                        input_previous[index] = bytes;
                        input_bytes.push(bytes);
                    }
                }
                if progressed.iter().any(|at| at.elapsed() > Duration::from_millis(1500)) {
                    return Err("consumer progress observation delayed beyond 1500 ms".into());
                }
                let sample = json!({"atMs": start.elapsed().as_millis(), "consumerBytes": delivered, "inputBytes": input_bytes});
                writeln!(capture, "{sample}").map_err(|error| error.to_string())?;
                samples += 1;
                if finished.is_some_and(|at: Instant| at.elapsed() >= Duration::from_secs(2)) { break; }
                if start.elapsed() > Duration::from_secs(180) { return Err("fault exceeded 180 s".into()); }
            }
        }
    }
    Ok(
        json!({"passed": true, "fault": result, "maxObservedDeliveryGapMs": max_gap, "maxObservedInputReceiveGapMs": input_max_gap, "deliveryGapLimitMs": 1500, "sampleIntervalMs": 250, "samplesPath": path, "samples": samples}),
    )
}
