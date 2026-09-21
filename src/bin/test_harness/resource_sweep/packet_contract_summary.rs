//! Per-rung aggregation for the packet-rate contract.
//!
//! Split from `packet_contract.rs` so the sampler (what is measured) and the
//! rung summary (what the numbers are rolled up to, and how the rung is
//! judged) stay reviewable separately.

use serde_json::{Value, json};

use super::measurement::round2;
use super::packet_contract::MIN_RATED_WINDOW_SECS;
use super::packet_contract_run::cpu_masks_disjoint;
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
    // Workload conformance over each peer's own observed window: the peer
    // reports a payload delta with its own observation interval, so bytes are
    // integrated as `payloadBytesDelta` (never `rate × the measuring host's
    // interval`, which is a different span) and compared against
    // `expectedOutputs × 1 MB/s × observedSecs`. The observed seconds are
    // recorded and must cover the contract minimum, so a rung cannot be judged
    // on a window the peer barely observed.
    let mut workload_reasons: Vec<String> = Vec::new();
    let mut peer_delivery = serde_json::Map::new();
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
                let Some(delta) = peer["payloadBytesDelta"].as_u64() else {
                    continue;
                };
                let Some(peer_interval) = peer["intervalSecs"].as_f64() else {
                    continue;
                };
                let entry = match peer_delivery.get_mut(host) {
                    Some(Value::Object(entry)) => entry,
                    _ => {
                        peer_delivery.insert(
                            host.to_string(),
                            json!({
                                "deliveredBytes": 0_u64,
                                "observedSecs": 0.0_f64,
                                "expectedOutputs": 0_u64,
                            }),
                        );
                        match peer_delivery.get_mut(host) {
                            Some(Value::Object(entry)) => entry,
                            _ => unreachable!("just inserted"),
                        }
                    }
                };
                let delivered = entry["deliveredBytes"].as_u64().unwrap_or(0) + delta;
                let observed = entry["observedSecs"].as_f64().unwrap_or(0.0) + peer_interval;
                entry.insert("deliveredBytes".to_string(), json!(delivered));
                entry.insert("observedSecs".to_string(), json!(observed));
                if let Some(expected_outputs) = peer["expectedOutputs"].as_u64() {
                    entry.insert("expectedOutputs".to_string(), json!(expected_outputs));
                }
            }
        }
        if let Some(pps) = sample["srtDataFirstPps"].as_f64() {
            data_first_total += pps * interval;
            data_first_window += interval;
        }
    }
    for (host, entry) in peer_delivery.iter_mut() {
        let Some(entry) = entry.as_object_mut() else {
            continue;
        };
        let delivered = entry["deliveredBytes"].as_u64().unwrap_or(0);
        let observed = entry["observedSecs"].as_f64().unwrap_or(0.0);
        let expected_outputs = entry["expectedOutputs"].as_u64().unwrap_or(0);
        let expected =
            expected_outputs as f64 * EXPECTED_PAYLOAD_BYTES_PER_OUTPUT_PER_SEC * observed;
        entry.insert(
            "coverageRatio".to_string(),
            json!(if rated_window_secs > 0.0 {
                round2(observed / rated_window_secs)
            } else {
                0.0
            }),
        );
        entry.insert(
            "deliveredBytesPerSec".to_string(),
            json!(if observed > 0.0 {
                round2(delivered as f64 / observed)
            } else {
                0.0
            }),
        );
        entry.insert(
            "expectedBytesPerSec".to_string(),
            json!(round2(
                expected_outputs as f64 * EXPECTED_PAYLOAD_BYTES_PER_OUTPUT_PER_SEC
            )),
        );
        if observed < MIN_RATED_WINDOW_SECS {
            workload_reasons.push(format!(
                "peer {host} observed only {observed:.1}s of the common window"
            ));
            continue;
        }
        if expected <= 0.0 {
            workload_reasons.push(format!("peer {host} expected delivery is not observable"));
            continue;
        }
        let ratio = delivered as f64 / expected;
        if (ratio - 1.0).abs() > WORKLOAD_TOLERANCE {
            workload_reasons.push(format!(
                "peer {host} delivered {delivered} B over {observed:.1}s observed against {expected:.0} B expected at the 8 Mbps workload ({:.0}%)",
                ratio * 100.0
            ));
        }
    }
    if peer_delivery.is_empty() && data_first_window > 0.0 && outputs > 0 {
        let per_output = data_first_total / data_first_window / outputs as f64;
        if (per_output / EXPECTED_DATA_PPS_PER_OUTPUT - 1.0).abs() > WORKLOAD_TOLERANCE {
            workload_reasons.push(format!(
                "sent {per_output:.0} first-transmission DATA pps/output over the observed window against the ~{EXPECTED_DATA_PPS_PER_OUTPUT:.0} of the 8 Mbps workload"
            ));
        }
    }
    if !workload_reasons.is_empty() {
        if status == "healthy" {
            status = "contaminated";
        }
        reasons.extend(workload_reasons);
    }

    // Topology and the *observed* CPU masks: what ran where, not what was asked
    // for. A same-host topology (veth/loopback) is only baseline-eligible when
    // the measured datapath and the receivers have disjoint CPUs.
    let restream_cpus = last["restreamCpusAllowed"].as_str().map(str::to_string);
    let mut peer_cpus = serde_json::Map::new();
    let mut disjoint = None;
    if let Some(peers) = last["peers"].as_array() {
        for peer in peers {
            let Some(host) = peer["host"].as_str() else {
                continue;
            };
            let Some(mask) = peer["cpusAllowedList"].as_str() else {
                continue;
            };
            peer_cpus.insert(host.to_string(), json!(mask));
            if let Some(restream) = &restream_cpus {
                let pair = cpu_masks_disjoint(restream, mask);
                disjoint = Some(disjoint.unwrap_or(true) && pair.unwrap_or(false));
            }
        }
    }
    let topology_kind = run["topologyKind"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let same_host = topology_kind != "remote";
    let topology = json!({
        "kind": topology_kind,
        "netns": run["topologyNetns"],
        "peerHosts": run["peerTargets"],
        "restreamCpusAllowed": restream_cpus,
        "peerCpusAllowed": peer_cpus,
        "cpuPartitioned": disjoint,
        "sameHost": same_host,
    });

    let workload = json!({
        "ingestTypes": last["ingestTypes"],
        "egressMix": last["egressMix"],
        "configuredOutputs": last["outputs"],
        "transcode": last["transcode"],
    });
    let baseline = baseline_eligibility(
        run,
        scenario,
        outputs,
        rated_window_secs,
        if rated == 0 {
            "no-rated-samples"
        } else {
            status
        },
        rated,
        samples.len(),
        &workload,
        &topology,
    );

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
        "countsPerWindow": mean_peak(&[
            "ownerTxFailedSendsDelta",
            "ownerTxExhaustionsDelta",
            "ownerServiceBudgetExhaustedDelta",
            "ownerRxRingDroppedDelta",
            "ownerRxTruncatedDelta",
            "shardFeedResyncsDelta",
            "shardDriverBudgetViolationsDelta",
            "shardQueueOverflowsDelta",
        ]),
        "peerDelivery": peer_delivery,
        "topology": topology,
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
#[allow(clippy::too_many_arguments)]
pub(super) fn baseline_eligibility(
    run: &Value,
    scenario: &str,
    outputs: u64,
    common_window_secs: f64,
    runtime_status: &str,
    rated_samples: usize,
    total_samples: usize,
    workload: &Value,
    topology: &Value,
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
    // 300/500/1000 need peers that are actually remote: a loopback target is
    // the same host's sink, which the 100-output shakedown already showed
    // cannot carry the workload losslessly.
    let peer_hosts: Vec<String> = run["peerStateEndpoint"]["hosts"]
        .as_array()
        .map(|hosts| {
            hosts
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let is_loopback = |host: &str| {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host == "127.0.0.1" || host == "localhost" || host == "::1" || host == "0.0.0.0"
    };
    let remote_peers = !peer_hosts.is_empty() && peer_hosts.iter().all(|host| !is_loopback(host));
    // "Remote" must mean another machine, not just a non-loopback address: a
    // same-host netns/veth peer is still this host's CPU and kernel.
    if outputs >= 300 && (!remote_peers || topology["sameHost"] != false) {
        reasons.push(format!(
            "{outputs}-output rungs need a second host: peers {peer_hosts:?} in topology {:?}",
            topology["kind"]
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
    // Same-host lanes must prove the receiver is not on the measured CPUs.
    if topology["sameHost"] != false {
        match topology["cpuPartitioned"].as_bool() {
            Some(true) => {}
            Some(false) => reasons.push(
                "same-host topology without disjoint CPU masks: the receivers share the measured CPUs"
                    .to_string(),
            ),
            None => reasons.push(
                "same-host topology without observed CPU masks (RESTREAM_CPUSET/SRT_SINK_CPUSET)"
                    .to_string(),
            ),
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
    // Every sample of a correctly primed rung is rated; a rung with unrated
    // samples cannot be promoted from a `healthy` default.
    if rated_samples == 0 || rated_samples != total_samples {
        reasons.push(format!(
            "{rated_samples} of {total_samples} samples are rated (a contractual rung primes first and rates every sample)"
        ));
    }
    if run["restreamBinExplicit"].as_bool() != Some(false) {
        reasons.push(
            "RESTREAM_BIN was overridden: the provenance stamp only covers the default sibling binary"
                .to_string(),
        );
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
            "ratedSamples": rated_samples,
            "totalSamples": total_samples,
            "restreamBinExplicit": run["restreamBinExplicit"],
            "remotePeers": remote_peers,
            "peerHosts": peer_hosts,
            "topologyKind": topology["kind"],
            "cpuPartitioned": topology["cpuPartitioned"],
            "workload": workload.clone(),
        }
    })
}
