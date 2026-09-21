//! Per-rung aggregation for the packet-rate contract.
//!
//! Split from `packet_contract.rs` so the sampler (what is measured) and the
//! rung summary (what the numbers are rolled up to, and how the rung is
//! judged) stay reviewable separately.

use serde_json::{Value, json};

use super::measurement::round2;
use super::packet_contract::MIN_RATED_WINDOW_SECS;
use super::packet_contract_verdict::sample_validity;

/// Aggregate one rung's samples: mean/peak rates, peak gauges, cost proxies,
/// the workload dimensions, and the worst validity verdict with the union of
/// its reasons.
pub(super) fn rung_summary(
    scenario: &str,
    outputs: u64,
    run_samples: usize,
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
        "shardQueueOverflows",
        "shardDriverBudgetViolations",
        "shardFeedResyncs",
        "ownerTxInFlightMax",
        "ownerTxCapacityMax",
        "ownerTxExhaustions",
        "ownerTxFailedSends",
        "ownerServiceBudgetExhausted",
        "ownerCallerInFlightHwm",
        "ownerCallerQueuedHwm",
        "ownerRxRingDropped",
        "ownerRxTruncated",
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

    json!({
        "scenario": scenario,
        "label": last["label"],
        "outputs": outputs,
        "samples": samples.len(),
        "ratedSamples": rated,
        "runSamples": run_samples,
        "workload": {
            "ingestTypes": last["ingestTypes"],
            "egressMix": last["egressMix"],
            "configuredOutputs": last["outputs"],
        },
        "ratedWindowSecs": round2(rated_window_secs),
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
