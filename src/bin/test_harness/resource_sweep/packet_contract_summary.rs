//! Per-rung aggregation for the packet-rate contract.
//!
//! Split from `packet_contract.rs` so the sampler (what is measured) and the
//! rung summary (what the numbers are rolled up to, and how the rung is
//! judged) stay reviewable separately.

use serde_json::{Value, json};

use super::measurement::round2;
use super::packet_contract::MIN_RATED_WINDOW_SECS;
use super::packet_contract_verdict::sample_validity;
use super::packet_contract_verdict::{
    EXPECTED_DATA_PPS_PER_OUTPUT, EXPECTED_PAYLOAD_BYTES_PER_OUTPUT_PER_SEC, WORKLOAD_TOLERANCE,
};

/// Aggregate one rung's samples: mean/peak rates, peak gauges, cost proxies,
/// the workload dimensions, and the worst validity verdict with the union of
/// its reasons.
pub(super) fn rung_summary(
    scenario: &str,
    outputs: u64,
    run_samples: usize,
    run: &Value,
    samples: &[&Value],
) -> Value {
    let mean_peak = |keys: &[&str]| -> serde_json::Map<String, Value> {
        let mut out = serde_json::Map::new();
        for key in keys {
            let values: Vec<f64> = samples
                .iter()
                .filter_map(|sample| sample[key].as_f64())
                .collect();
            if values.is_empty() {
                continue;
            }
            out.insert(
                key.to_string(),
                json!({
                    "mean": round2(values.iter().sum::<f64>() / values.len() as f64),
                    "peak": round2(values.iter().copied().fold(f64::MIN, f64::max)),
                    "samples": values.len(),
                }),
            );
        }
        out
    };
    let rate_keys = [
        "ownerTxFailedSendsDelta",
        "ownerTxExhaustionsDelta",
        "ownerServiceBudgetExhaustedDelta",
        "ownerRxRingDroppedDelta",
        "ownerRxTruncatedDelta",
        "shardFeedResyncsDelta",
        "shardDriverBudgetViolationsDelta",
        "shardQueueOverflowsDelta",
        "srtTxDatagramsPerSec",
        "srtDataFirstPps",
        "srtDataRetransmitPps",
        "srtControlPps",
        "srtRetransmitShare",
        "srtTxCompletedPerSec",
        "srtRxDatagramsPerSec",
        "srtServiceVisitsPerSec",
        "srtServiceActionsPerSec",
        "srtMaintenanceActionsPerSec",
        "shardLoopIterationsPerSec",
        "shardMediaTicksPerSec",
        "shardReadyVisitsPerSec",
        "shardRetriesPerSec",
        "udpInErrorsPerSec",
        "udpRcvbufErrorsPerSec",
        "udpSndbufErrorsPerSec",
        "nicRxDroppedPerSec",
        "nicTxDroppedPerSec",
    ];
    let gauge_keys = [
        "shardReadyDepthMax",
        "shardReadyDepthHwmMax",
        "shardUnhealthyCount",
        "shardBudgetExhaustions",
        "ownerTxInFlightMax",
        "ownerTxCapacityMax",
        "ownerCallerInFlightHwm",
        "ownerCallerQueuedHwm",
    ];
    let mut gauges = serde_json::Map::new();
    for key in gauge_keys {
        gauges.insert(
            key.to_string(),
            json!(
                samples
                    .iter()
                    .filter_map(|sample| sample[key].as_u64())
                    .max()
            ),
        );
    }

    // Verdict: the worst status any rated sample reached, with each distinct
    // reason and how many samples reported it.
    let mut status = "healthy";
    let mut reason_counts: Vec<(String, usize)> = Vec::new();
    let mut rated = 0_usize;
    for sample in samples {
        let Some((sample_status, reasons)) = sample_validity(sample, outputs) else {
            continue;
        };
        rated += 1;
        status = match (status, sample_status) {
            ("invalid", _) | (_, "invalid") => "invalid",
            ("contaminated", _) | (_, "contaminated") => "contaminated",
            _ => "healthy",
        };
        for reason in reasons {
            match reason_counts.iter_mut().find(|(seen, _)| *seen == reason) {
                Some((_, count)) => *count += 1,
                None => reason_counts.push((reason, 1)),
            }
        }
    }
    let mut reasons: Vec<String> = reason_counts
        .into_iter()
        .map(|(reason, count)| format!("{reason} (in {count}/{rated} rated samples)"))
        .collect();
    let last = samples.last().copied().unwrap_or(&Value::Null);
    // The rated window is the span the packet counters actually cover: from
    // the prime to the last rated sample. Configuration is not evidence.
    let rated_window_secs = samples
        .iter()
        .filter_map(|sample| sample["ratedSecs"].as_f64())
        .fold(0.0_f64, f64::max);
    if rated_window_secs < MIN_RATED_WINDOW_SECS {
        if status == "healthy" {
            status = "contaminated";
        }
        reasons.push(format!(
            "rated window {rated_window_secs:.1}s is shorter than the {MIN_RATED_WINDOW_SECS:.0}s contract minimum"
        ));
    }

    // Baseline eligibility is deliberately separate from runtime validity:
    // `healthy` describes the datapath, this describes whether the artifact
    // may be recorded as a contractual baseline at all.
    // Workload conformance over the whole common rated window: each sample's
    // rate times its own interval reconstructs the delivered bytes, so this is
    // the prime-to-final average the fixture's ±5% contract describes — a VBR
    // stream may exceed the band for one second without failing the rung.
    let mut workload_reasons: Vec<String> = Vec::new();
    let mut peer_totals: Vec<(String, f64, f64)> = Vec::new();
    let mut data_first_total = 0.0_f64;
    let mut data_first_window = 0.0_f64;
    for sample in samples {
        let Some(interval) = sample["intervalSecs"].as_f64() else {
            continue;
        };
        if let Some(peers) = sample["peers"].as_array() {
            for peer in peers {
                let Some(host) = peer["host"].as_str() else {
                    continue;
                };
                let delivered = peer["payloadBytesPerSec"].as_f64().unwrap_or(0.0) * interval;
                let entry = match peer_totals.iter_mut().find(|(seen, ..)| seen == host) {
                    Some(entry) => entry,
                    None => {
                        peer_totals.push((host.to_string(), 0.0, 0.0));
                        peer_totals.last_mut().expect("just pushed")
                    }
                };
                entry.1 += delivered;
                if let Some(expected_outputs) = peer["expectedOutputs"].as_u64() {
                    entry.2 = expected_outputs as f64
                        * EXPECTED_PAYLOAD_BYTES_PER_OUTPUT_PER_SEC
                        * rated_window_secs;
                }
            }
        }
        if let Some(pps) = sample["srtDataFirstPps"].as_f64() {
            data_first_total += pps * interval;
            data_first_window += interval;
        }
    }
    for (host, delivered, expected) in &peer_totals {
        if *expected <= 0.0 {
            workload_reasons.push(format!("peer {host} expected delivery is not observable"));
            continue;
        }
        let ratio = delivered / expected;
        if (ratio - 1.0).abs() > WORKLOAD_TOLERANCE {
            workload_reasons.push(format!(
                "peer {host} delivered {delivered:.0} B over the common window against {expected:.0} B expected at the 8 Mbps workload ({:.0}%)",
                ratio * 100.0
            ));
        }
    }
    if peer_totals.is_empty() && data_first_window > 0.0 && outputs > 0 {
        let per_output = data_first_total / data_first_window / outputs as f64;
        if (per_output / EXPECTED_DATA_PPS_PER_OUTPUT - 1.0).abs() > WORKLOAD_TOLERANCE {
            workload_reasons.push(format!(
                "sent {per_output:.0} first-transmission DATA pps/output over the common window against the ~{EXPECTED_DATA_PPS_PER_OUTPUT:.0} of the 8 Mbps workload"
            ));
        }
    }
    if !workload_reasons.is_empty() {
        if status == "healthy" {
            status = "contaminated";
        }
        reasons.extend(workload_reasons);
    }

    let workload = json!({
        "ingestTypes": last["ingestTypes"],
        "egressMix": last["egressMix"],
        "configuredOutputs": last["outputs"],
        "transcode": last["transcode"],
    });
    let baseline =
        baseline_eligibility(run, scenario, outputs, rated_window_secs, status, &workload);

    json!({
        "scenario": scenario,
        "label": last["label"],
        "outputs": outputs,
        "samples": samples.len(),
        "ratedSamples": rated,
        "runSamples": run_samples,
        "workload": workload,
        "commonRatedWindowSecs": round2(rated_window_secs),
        "baselineEligible": baseline,
        "validity": { "status": if rated == 0 { "no-rated-samples" } else { status }, "reasons": reasons },
        "ratesPerSec": mean_peak(&rate_keys),
        "gaugesPeak": gauges,
        "cost": mean_peak(&[
            "cpuMicrosPerSrtPacket",
            "srtTxDatagramsPerOutputPerSec",
            "srtDataFirstPpsPerOutput",
        ]),
    })
}

/// Whether this rung may be recorded as a contractual baseline. Kept apart
/// from the runtime verdict so an accidental promotion is mechanically
/// impossible: a `healthy` datapath on a dirty tree, a non-canonical workload,
/// an off-ladder output count or a short window is still not a baseline.
pub(super) fn baseline_eligibility(
    run: &Value,
    scenario: &str,
    outputs: u64,
    common_window_secs: f64,
    runtime_status: &str,
    workload: &Value,
) -> Value {
    let mut reasons = Vec::new();
    let sha = run["gitSha"].as_str();
    if sha.is_none() {
        reasons.push("the artifact records no git SHA".to_string());
    }
    match run["gitDirty"].as_bool() {
        Some(false) => {}
        Some(true) => reasons.push("the work tree was dirty at run time".to_string()),
        None => reasons.push("the work-tree state is not observable".to_string()),
    }
    // Build provenance: a clean SHA at run time does not prove the executed
    // binary came from it.
    match run["buildProvenance"].as_object() {
        Some(build) => {
            if build["gitSha"].as_str() != sha {
                reasons.push(format!(
                    "the bench binaries were built from {:?}, not the recorded HEAD {sha:?}",
                    build["gitSha"]
                ));
            }
            if build["gitDirty"].as_bool() != Some(false) {
                reasons.push("the bench binaries were built from a dirty tree".to_string());
            }
        }
        None => reasons.push(
            "no bench build provenance stamp next to the harness binary (build with scripts/build/bench-harness.sh)"
                .to_string(),
        ),
    }
    if run["lifecycle"].as_str() != Some("isolated") {
        reasons.push(format!(
            "lifecycle is {:?}, not the isolated rung lifecycle",
            run["lifecycle"]
        ));
    }
    if run["peerMode"].as_str() != Some("sink") {
        reasons.push(format!(
            "peer mode is {:?}, not the harness sink peer",
            run["peerMode"]
        ));
    }
    if run["bitrateLabel"].as_str() != Some("8M") {
        reasons.push(format!(
            "workload bitrate label is {:?}, not 8M",
            run["bitrateLabel"]
        ));
    }
    let configured: Vec<u64> = run["egressCounts"]
        .as_array()
        .map(|counts| counts.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    if configured != vec![outputs] {
        reasons.push(format!(
            "the invocation configured egress rungs {configured:?}, not exactly [{outputs}]"
        ));
    }
    let filter: Vec<String> = run["scenarioFilter"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if filter != vec!["egress-growth-source-srt".to_string()] {
        reasons.push(format!(
            "the scenario filter is {filter:?}, not exactly the canonical SRT fanout"
        ));
    }
    if scenario != "egress-growth-source-srt" {
        reasons.push(format!(
            "scenario {scenario:?} is not the canonical SRT fanout"
        ));
    }
    if run["settleSecs"].as_u64().unwrap_or(0) < 10 {
        reasons.push(format!(
            "settle window is {}s, below the 10s minimum",
            run["settleSecs"]
        ));
    }
    if !matches!(outputs, 100 | 300 | 500 | 1000) {
        reasons.push(format!("output count {outputs} is not a ladder rung"));
    }
    if outputs >= 300 && run["peerStateEndpoint"].is_null() {
        reasons.push(format!(
            "{outputs}-output rungs need remote sink peers, not loopback"
        ));
    }
    for (key, expected) in [
        ("ingestTypes", "h264-srt"),
        ("egressMix", "srt-source"),
        ("transcode", "no"),
    ] {
        if workload[key].as_str() != Some(expected) {
            reasons.push(format!(
                "workload {key} is {:?}, not {expected:?}",
                workload[key]
            ));
        }
    }
    if common_window_secs < MIN_RATED_WINDOW_SECS {
        reasons.push(format!(
            "common rated window {common_window_secs:.1}s is shorter than {MIN_RATED_WINDOW_SECS:.0}s"
        ));
    }
    if runtime_status != "healthy" {
        reasons.push(format!("runtime validity is {runtime_status}, not healthy"));
    }
    json!({
        "eligible": reasons.is_empty(),
        "reasons": reasons,
        "checks": {
            "gitSha": sha,
            "gitDirty": run["gitDirty"],
            "buildProvenance": run["buildProvenance"],
            "lifecycle": run["lifecycle"],
            "peerMode": run["peerMode"],
            "bitrateLabel": run["bitrateLabel"],
            "egressCounts": configured,
            "scenarioFilter": filter,
            "settleSecs": run["settleSecs"],
            "scenario": scenario,
            "outputs": outputs,
            "commonRatedWindowSecs": round2(common_window_secs),
            "runtimeValidity": runtime_status,
            "workload": workload.clone(),
        }
    })
}
