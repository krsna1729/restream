//! The validity verdict of a packet-rate contract sample.
//!
//! Split out of `packet_contract.rs` so the sampler and the verdict stay
//! reviewable separately: the sampler publishes numbers, this module decides
//! whether those numbers describe a comparable, lossless rung.

use serde_json::Value;

/// The fixed product workload from the roadmap: 8 Mbps of MPEG-TS payload per
/// output is 1,000,000 bytes/s, and at the 1316-byte SRT payload the roadmap
/// assumes that is ~760 first-transmission DATA packets/s per output.
pub(super) const EXPECTED_PAYLOAD_BYTES_PER_OUTPUT_PER_SEC: f64 = 1_000_000.0;
pub(super) const EXPECTED_DATA_PPS_PER_OUTPUT: f64 = 760.0;
/// The fixture contract's own tolerance (`8.0 ± 0.4 Mbps`). Applied over the
/// whole common rated window, because the fixture is VBR and that contract is a
/// whole-span average.
pub(super) const WORKLOAD_TOLERANCE: f64 = 0.05;

/// Verdict for one sample. `invalid` means the rung cannot be compared with
/// any other rung (a required participant is missing, stalled, retrying or
/// broken); `contaminated` means the host or the peer dropped datagrams, so
/// the path under test was not lossless; `healthy` means none of those fired.
/// Only rated samples get a verdict — the first sample has no interval yet.
pub(super) fn sample_validity(
    sample: &Value,
    expected_outputs: u64,
) -> Option<(&'static str, Vec<String>)> {
    sample["srtTxDatagramsPerSec"].as_f64()?;
    let mut reasons = Vec::new();
    let mut invalid = false;
    // `fired` is the failure condition: the reason is recorded only when it
    // actually happened, so a clean sample produces an empty reason list.
    let require = |invalid: &mut bool, reasons: &mut Vec<String>, fired: bool, reason: String| {
        if fired {
            *invalid = true;
            reasons.push(reason);
        }
    };

    let shards = sample["egressShardCount"].as_u64().unwrap_or(0);
    let owners = sample["srtOwnerCount"].as_u64().unwrap_or(0);
    if shards == 0 || owners == 0 {
        invalid = true;
        reasons.push(format!(
            "no live SRT shard/owner to measure (shards={shards}, owners={owners})"
        ));
    }
    match sample["shardUnhealthyCount"].as_u64() {
        Some(count) => require(
            &mut invalid,
            &mut reasons,
            count > 0,
            format!("{count} SRT shard(s) in a non-healthy state"),
        ),
        None => {
            invalid = true;
            reasons.push("SRT shard health is not observable".to_string());
        }
    }
    // Exact match, not "at least": an isolated rung must run exactly the
    // outputs it declares, so extra live leaves (leaked state, a stray output)
    // fail the rung just as missing ones do.
    match sample["capacityActiveLeaves"].as_u64() {
        Some(active) => require(
            &mut invalid,
            &mut reasons,
            active != expected_outputs,
            format!("{active} of {expected_outputs} expected outputs active"),
        ),
        None => {
            invalid = true;
            reasons.push("active output count is not observable".to_string());
        }
    }

    // Remote rungs depend on the sink hosts' own drop counters, which only
    // their state endpoints can report. Missing telemetry is a reason, and a
    // restarted sink means the window is no longer one measurement.
    let expected_peers = sample["expectedPeers"].as_u64().unwrap_or(0);
    if expected_peers > 0 {
        match sample["peers"].as_array() {
            Some(peers) if peers.len() as u64 == expected_peers => {
                for peer in peers {
                    let host = peer["host"].as_str().unwrap_or("peer");
                    if let Some(error) = peer["error"].as_str() {
                        reasons.push(format!("peer {host} telemetry unavailable: {error}"));
                        continue;
                    }
                    if peer["runIdChanged"] == true {
                        invalid = true;
                        reasons.push(format!("peer {host} sink restarted mid-rung"));
                    }
                    for (key, label) in [
                        ("udpRcvbufErrorsPerSec", "UDP receive-buffer"),
                        ("udpSndbufErrorsPerSec", "UDP send-buffer"),
                        ("udpInErrorsPerSec", "UDP receive"),
                        ("nicRxDroppedPerSec", "NIC receive"),
                        ("nicTxDroppedPerSec", "NIC transmit"),
                    ] {
                        match peer[key].as_f64() {
                            Some(rate) if rate > 0.0 => {
                                reasons.push(format!("peer {host} {label} drops: {key}={rate}"))
                            }
                            None => reasons.push(format!("peer {host} {key} not observable")),
                            _ => {}
                        }
                    }
                }
            }
            peers => reasons.push(format!(
                "expected {expected_peers} peer state reading(s), got {}",
                peers.map(|peers| peers.len()).unwrap_or(0)
            )),
        }
    }
    if let Some(retries) = sample["shardRetriesPerSec"].as_f64() {
        require(
            &mut invalid,
            &mut reasons,
            retries > 0.0,
            format!("output retries during the sample ({retries}/s)"),
        );
    }
    require(
        &mut invalid,
        &mut reasons,
        sample["ownerFaulted"] == true,
        "an SRT owner faulted".to_string(),
    );
    // Host and peer pressure: drops here mean the measured path lost
    // datagrams, which is exactly what a no-loss contract rung must not have.
    for key in [
        "udpRcvbufErrorsPerSec",
        "udpSndbufErrorsPerSec",
        "udpInErrorsPerSec",
        "nicRxDroppedPerSec",
        "nicTxDroppedPerSec",
    ] {
        match sample[key].as_f64() {
            Some(rate) => {
                if rate > 0.0 {
                    reasons.push(format!("{key}={rate}"));
                }
            }
            None => reasons.push(format!("{key} is not observable")),
        }
    }
    // Monotonic fault/pressure counters are judged as rated-window deltas, so
    // a ramp-up or settle-period event cannot condemn a steady-state window.
    // `ownerTxFailedSends` and a feed resync are faults; the rest are pressure
    // that still rules out a no-loss baseline.
    for (key, invalidating) in [
        ("ownerTxFailedSendsDelta", true),
        ("shardFeedResyncsDelta", true),
        ("ownerTxExhaustionsDelta", false),
        ("ownerServiceBudgetExhaustedDelta", false),
        ("ownerRxRingDroppedDelta", false),
        ("ownerRxTruncatedDelta", false),
        ("shardDriverBudgetViolationsDelta", false),
        ("shardQueueOverflowsDelta", false),
    ] {
        match sample[key].as_u64() {
            Some(0) => {}
            Some(value) => {
                if invalidating {
                    invalid = true;
                }
                reasons.push(format!("{key}={value} in the rated window"));
            }
            None => {
                if invalidating {
                    invalid = true;
                }
                reasons.push(format!("{key} is not observable"));
            }
        }
    }
    // Immediate workload failures only: a stall (zero delivery) is a per-sample
    // fault, and connection churn means the steady state was not steady. The
    // ±5% workload-conformance check runs over the whole common rated window in
    // the rung summary, because the fixture is VBR and its tolerance is a
    // whole-span average, not a per-second one.
    if expected_peers > 0 {
        if let Some(peers) = sample["peers"].as_array() {
            for peer in peers {
                let host = peer["host"].as_str().unwrap_or("peer");
                if peer["error"].as_str().is_some() {
                    continue; // already a reason
                }
                if peer["closedPerSec"].as_f64().is_some_and(|rate| rate > 0.0)
                    || peer["acceptedPerSec"]
                        .as_f64()
                        .is_some_and(|rate| rate > 0.0)
                {
                    invalid = true;
                    reasons.push(format!(
                        "peer {host} connection churn during the rated window (accepted/closed)"
                    ));
                }
                match peer["payloadBytesPerSec"].as_f64() {
                    Some(0.0) => reasons.push(format!("peer {host} delivered no payload")),
                    Some(_) => {}
                    None => reasons.push(format!(
                        "peer {host} delivered payload rate is not observable"
                    )),
                }
            }
        }
    } else if sample["srtDataFirstPps"].as_f64() == Some(0.0) {
        reasons.push("no first-transmission DATA sent in the sample".to_string());
    }

    // The roadmap's target is a healthy NO-LOSS path, so retransmissions in
    // the rated window are end-to-end loss evidence: nonzero contaminates, and
    // an unobservable retransmit counter is a missing sensor, not a pass. No
    // percentage threshold is invented — zero is the no-loss condition.
    match sample["srtDataRetransmitPps"].as_f64() {
        Some(0.0) => {}
        Some(rate) => reasons.push(format!(
            "SRT retransmissions in the rated window: {rate}/s (no-loss contract)"
        )),
        None => reasons.push("srtDataRetransmitPps is not observable".to_string()),
    }

    Some(if invalid {
        ("invalid", reasons)
    } else if reasons.is_empty() {
        ("healthy", reasons)
    } else {
        ("contaminated", reasons)
    })
}
