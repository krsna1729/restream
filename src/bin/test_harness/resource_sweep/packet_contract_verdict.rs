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
    match sample["capacityActiveLeaves"].as_u64() {
        Some(active) => require(
            &mut invalid,
            &mut reasons,
            active < expected_outputs,
            format!("{active} of {expected_outputs} expected outputs active"),
        ),
        None => {
            invalid = true;
            reasons.push("active output count is not observable".to_string());
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
