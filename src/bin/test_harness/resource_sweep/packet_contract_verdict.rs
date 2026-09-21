//! The validity verdict of a packet-rate contract sample.
//!
//! Split out of `packet_contract.rs` so the sampler and the verdict stay
//! reviewable separately: the sampler publishes numbers, this module decides
//! whether those numbers describe a comparable, lossless rung.

use serde_json::Value;

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
                        ("udpRcvbufErrorsPerSec", "receive-buffer"),
                        ("udpInErrorsPerSec", "receive"),
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
    for key in ["ownerTxFailedSends", "ownerTxExhaustions"] {
        match sample[key].as_u64() {
            Some(value) => require(
                &mut invalid,
                &mut reasons,
                value > 0,
                format!("{key}={value}"),
            ),
            None => {
                invalid = true;
                reasons.push(format!("{key} is not observable"));
            }
        }
    }
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
    match sample["shardFeedResyncs"].as_u64() {
        Some(value) => require(
            &mut invalid,
            &mut reasons,
            value > 0,
            format!("{value} output feed resync(s)"),
        ),
        None => {
            invalid = true;
            reasons.push("shardFeedResyncs is not observable".to_string());
        }
    }
    for key in [
        "shardDriverBudgetViolations",
        "shardQueueOverflows",
        "ownerServiceBudgetExhausted",
    ] {
        match sample[key].as_u64() {
            Some(0) => {}
            Some(value) => reasons.push(format!("{key}={value}")),
            None => reasons.push(format!("{key} is not observable")),
        }
    }
    Some(if invalid {
        ("invalid", reasons)
    } else if reasons.is_empty() {
        ("healthy", reasons)
    } else {
        ("contaminated", reasons)
    })
}
