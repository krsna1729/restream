//! "Explain this incident": the layer that broke, read from the incident's
//! flight-recorder window, and a fix the agent plane can plan.
//!
//! Deterministic: the same alert and window give the same explanation. The
//! walk is the one `restream_health::diagnosis` makes for a live output,
//! upstream first (ingest, feed, then shard or destination), over the
//! incident's recorded samples rather than one instant: an upstream fault
//! through most of the incident explains every downstream symptom. Shards
//! and destinations are per output and not in the samples, so for them the
//! alert's own engine diagnosis decides and the window adds what the
//! pipeline's outputs did.

use restream_agent_contract::{PlanRequest, ProposedChange};
use restream_health::alerts::Alert;
use restream_health::diagnosis::{Cause, Layer};
use serde::Serialize;

use super::flight_recorder::FlightSample;

/// How far before the alert's first sighting the window starts: the onset.
pub const EXPLAIN_LEAD_MS: u64 = 60_000;
/// A stage this many seconds behind the source is not keeping up.
const FEED_LAG_SEC: f64 = 2.0;
/// A stage whose input waited this share of the interval is too slow.
const FEED_BLOCKED: f64 = 0.5;
/// Lost packets, as a share of those received, that break the picture.
const INGEST_LOSS_SHARE: f64 = 0.02;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Explanation {
    pub alert_id: String,
    pub pipeline_id: Option<String>,
    pub output_id: Option<String>,
    pub window: Window,
    /// The broken layer; absent when neither the window nor the alert
    /// names one.
    pub layer: Option<Layer>,
    pub summary: String,
    /// Measured facts that point at `layer`.
    pub evidence: Vec<String>,
    /// Layers upstream of `layer` that the window shows healthy, and why.
    pub ruled_out: Vec<RuledOut>,
    /// A fix the plant can apply, as an agent-plane request; absent when no
    /// plant change fixes the cause.
    pub proposal: Option<PlanRequest>,
    /// What a person does: the fix outside the plant, or what to check
    /// before applying the proposal.
    pub recommendation: String,
}

/// The samples read: from the onset lead to the incident's end.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub from_ms: u64,
    pub to_ms: u64,
    /// When the alert was first seen; samples from here on are the
    /// incident's own.
    pub incident_from_ms: u64,
    pub samples: usize,
    pub incident_samples: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuledOut {
    pub layer: Layer,
    pub reason: String,
}

/// A layer the window shows broken: what, how we know, what to do.
struct Finding {
    layer: Layer,
    summary: String,
    evidence: Vec<String>,
    recommendation: String,
}

/// Explain `alert` from `samples` (the pipeline's flight record from
/// `incident_from_ms - EXPLAIN_LEAD_MS` to `to_ms`, oldest first).
pub fn explain(
    alert: &Alert,
    incident_from_ms: u64,
    to_ms: u64,
    samples: &[FlightSample],
) -> Explanation {
    let incident: Vec<&FlightSample> = samples
        .iter()
        .filter(|sample| sample.at_ms >= incident_from_ms && sample.at_ms <= to_ms)
        .collect();
    let before = samples
        .iter()
        .rev()
        .find(|sample| sample.at_ms < incident_from_ms);
    let window = Window {
        from_ms: incident_from_ms.saturating_sub(EXPLAIN_LEAD_MS),
        to_ms,
        incident_from_ms,
        samples: samples.len(),
        incident_samples: incident.len(),
    };
    let mut ruled_out = Vec::new();
    let upstream = if incident.is_empty() {
        None
    } else {
        ingest(&incident, &mut ruled_out).or_else(|| feed(&incident, before, &mut ruled_out))
    };

    let mut explanation = Explanation {
        alert_id: alert.id.clone(),
        pipeline_id: alert.pipeline_id.clone(),
        output_id: alert.output_id.clone(),
        window,
        layer: None,
        summary: String::new(),
        evidence: Vec::new(),
        ruled_out: Vec::new(),
        proposal: None,
        recommendation: String::new(),
    };
    if let Some(finding) = upstream {
        explanation.layer = Some(finding.layer);
        explanation.summary = finding.summary;
        explanation.evidence = finding.evidence;
        explanation.recommendation = finding.recommendation;
        explanation.ruled_out = ruled_out;
        return explanation;
    }

    let Some(diagnosis) = alert.diagnosis.as_ref() else {
        explanation.summary = format!(
            "{}: the recorded window shows no ingest or feed fault, and the alert names no layer.",
            alert.title
        );
        explanation.evidence = window_note(&incident);
        explanation.ruled_out = ruled_out;
        explanation.recommendation = alert.recommended_action.clone();
        return explanation;
    };
    // The engine judged this output at the alert; keep only the window's
    // verdicts on layers upstream of the one it names.
    ruled_out.retain(|ruled: &RuledOut| ruled.layer < diagnosis.layer);
    explanation.layer = Some(diagnosis.layer);
    explanation.summary = cause_summary(&diagnosis.cause);
    explanation.evidence = outputs_evidence(&incident);
    explanation.ruled_out = ruled_out;
    let (proposal, recommendation) = remedy(alert, diagnosis.layer, &diagnosis.cause);
    explanation.proposal = proposal;
    explanation.recommendation = recommendation;
    explanation
}

fn half_or_more(count: usize, of: usize) -> bool {
    count * 2 >= of
}

fn ingest(incident: &[&FlightSample], ruled_out: &mut Vec<RuledOut>) -> Option<Finding> {
    let n = incident.len();
    let absent = incident
        .iter()
        .filter(|sample| {
            sample
                .ingest
                .as_ref()
                .is_none_or(|ingest| ingest.packets_per_sec <= 0.0)
        })
        .count();
    if half_or_more(absent, n) {
        return Some(Finding {
            layer: Layer::Ingest,
            summary: format!(
                "No media reached the pipeline for {absent} of {n} samples: the publisher was gone."
            ),
            evidence: vec![format!(
                "ingest absent or silent in {absent} of the incident's {n} samples"
            )],
            recommendation: "Restore the publisher (the encoder or source feeding this \
                             pipeline). Every output recovers once media arrives; no plant \
                             change fixes a missing source."
                .to_string(),
        });
    }
    let lossy: Vec<f64> = incident
        .iter()
        .filter_map(|sample| {
            let ingest = sample.ingest.as_ref()?;
            let share = ingest.loss_per_sec? / ingest.packets_per_sec.max(1.0);
            (share >= INGEST_LOSS_SHARE).then_some(share)
        })
        .collect();
    if half_or_more(lossy.len(), n) {
        let worst = lossy.iter().copied().fold(0.0, f64::max);
        return Some(Finding {
            layer: Layer::Ingest,
            summary: format!(
                "The contribution link lost packets in {} of {n} samples (up to {:.1}%).",
                lossy.len(),
                worst * 100.0
            ),
            evidence: vec![format!(
                "ingest loss at or above {:.0}% of received packets in {} of {n} samples",
                INGEST_LOSS_SHARE * 100.0,
                lossy.len()
            )],
            recommendation: "Check the network between the encoder and this server, or raise \
                             the SRT latency. The outputs only carry what arrives."
                .to_string(),
        });
    }
    let mut rates: Vec<f64> = incident
        .iter()
        .filter_map(|sample| sample.ingest.as_ref()?.bitrate_bps)
        .collect();
    rates.sort_by(f64::total_cmp);
    let median = rates.get(rates.len() / 2).copied();
    ruled_out.push(RuledOut {
        layer: Layer::Ingest,
        reason: match median {
            Some(bps) => format!(
                "publisher present in {} of {n} samples at a median {:.1} Mb/s",
                n - absent,
                bps / 1e6
            ),
            None => format!("publisher present in {} of {n} samples", n - absent),
        },
    });
    None
}

fn feed(
    incident: &[&FlightSample],
    before: Option<&FlightSample>,
    ruled_out: &mut Vec<RuledOut>,
) -> Option<Finding> {
    let n = incident.len();
    let overflows = |sample: &FlightSample| sample.ring.as_ref().map(|ring| ring.overflows);
    let start = before
        .and_then(overflows)
        .or_else(|| incident.first().and_then(|sample| overflows(sample)));
    let end = incident.iter().rev().find_map(|sample| overflows(sample));
    let fell_off = match (start, end) {
        (Some(start), Some(end)) => end.saturating_sub(start),
        _ => 0,
    };

    // Each stage: samples where it lagged or blocked, and its worst lag.
    let mut stages: Vec<(&str, usize, f64)> = Vec::new();
    for sample in incident {
        for decoder in &sample.decoders {
            let lag = decoder.lag_sec.unwrap_or(0.0);
            let slow = lag > FEED_LAG_SEC || decoder.input_blocked.unwrap_or(0.0) >= FEED_BLOCKED;
            match stages
                .iter_mut()
                .find(|(stage, ..)| *stage == decoder.stage)
            {
                Some(entry) => {
                    entry.1 += usize::from(slow);
                    entry.2 = entry.2.max(lag);
                }
                None => stages.push((&decoder.stage, usize::from(slow), lag)),
            }
        }
    }
    let slowest = stages
        .iter()
        .filter(|(_, slow, _)| half_or_more(*slow, n))
        .max_by(|a, b| a.1.cmp(&b.1).then(a.2.total_cmp(&b.2)));
    if let Some((stage, slow, lag)) = slowest {
        return Some(Finding {
            layer: Layer::Feed,
            summary: format!(
                "Stage {stage} fell behind the source in {slow} of {n} samples (up to {lag:.1} s)."
            ),
            evidence: vec![format!(
                "{stage}: more than {FEED_LAG_SEC:.0} s behind or input blocked at least {:.0}% \
                 of the interval in {slow} of {n} samples",
                FEED_BLOCKED * 100.0
            )],
            recommendation: format!(
                "This host cannot run {stage} in real time. Use a lighter preset for its \
                 outputs, or send them at source quality."
            ),
        });
    }
    if fell_off > 0 {
        return Some(Finding {
            layer: Layer::Feed,
            summary: format!(
                "Readers fell off the source ring {fell_off} times during the incident."
            ),
            evidence: vec![format!("source ring overflows rose by {fell_off}")],
            recommendation: "A stage or output reading this pipeline could not keep up with \
                             the source; check the stages' load and the host's CPU."
                .to_string(),
        });
    }
    ruled_out.push(RuledOut {
        layer: Layer::Feed,
        reason: if stages.is_empty() {
            "no reader fell off the source ring".to_string()
        } else {
            format!(
                "no reader fell off the source ring; every stage stayed within \
                 {FEED_LAG_SEC:.0} s of the source"
            )
        },
    });
    None
}

fn cause_summary(cause: &Cause) -> String {
    match cause {
        Cause::NoPublisher => "The pipeline had no live publisher.".to_string(),
        Cause::StageNotProducing { stage, phase } => {
            format!("Stage {stage} was not producing ({phase}).")
        }
        Cause::ShardPanicked { shard } => format!("Egress shard {shard} panicked."),
        Cause::ShardStalled { shard } => format!("Egress shard {shard} stalled."),
        Cause::ShardStopped { shard } => {
            format!("Egress shard {shard} stopped while the output still ran on it.")
        }
        Cause::CapacityRefused { error } => format!("Admission refused the output: {error}."),
        Cause::RetriesExhausted => "The output spent its restart budget.".to_string(),
        Cause::RetryBudgetNearlySpent { attempts, max } => {
            format!("The output kept failing: {attempts} of {max} restarts spent.")
        }
        Cause::Rejected { error, .. } => format!("The destination rejected the output: {error}."),
        Cause::Backpressured { stalled: true } => {
            "Data waited on the send path and stopped draining.".to_string()
        }
        Cause::Backpressured { stalled: false } => {
            "Data waited on the send path and drained slowly.".to_string()
        }
        Cause::NoProgress => "The output ran but completed no send.".to_string(),
    }
}

fn outputs_evidence(incident: &[&FlightSample]) -> Vec<String> {
    if incident.is_empty() {
        return window_note(incident);
    }
    let max = |field: fn(&FlightSample) -> u32| {
        incident
            .iter()
            .map(|sample| field(sample))
            .max()
            .unwrap_or(0)
    };
    let worst_delivery = incident
        .iter()
        .filter(|sample| sample.outputs.offered_bps > 0.0)
        .map(|sample| (sample.outputs.delivered_bps, sample.outputs.offered_bps))
        .min_by(|a, b| (a.0 / a.1).total_cmp(&(b.0 / b.1)));
    let mut evidence = vec![format!(
        "the pipeline's outputs: up to {} stalled, {} retrying ({} restart attempts)",
        max(|sample| sample.outputs.stalled),
        max(|sample| sample.outputs.retrying),
        max(|sample| sample.outputs.retry_attempts),
    )];
    if let Some((delivered, offered)) = worst_delivery {
        evidence.push(format!(
            "at worst {:.1} of {:.1} Mb/s offered was delivered",
            delivered / 1e6,
            offered / 1e6
        ));
    }
    evidence
}

fn window_note(incident: &[&FlightSample]) -> Vec<String> {
    if incident.is_empty() {
        vec!["no flight-recorder samples cover the incident".to_string()]
    } else {
        outputs_evidence(incident)
    }
}

/// The plant change that addresses a shard or destination cause, if any,
/// and what a person should know.
fn remedy(alert: &Alert, layer: Layer, cause: &Cause) -> (Option<PlanRequest>, String) {
    let change = |kind: &str, output_id: &str| ProposedChange {
        kind: kind.to_string(),
        pipeline_id: alert.pipeline_id.clone(),
        output_id: Some(output_id.to_string()),
        name: None,
        url: None,
        monitoring_url: None,
        config: None,
        desired_state: None,
    };
    let layer_name = match layer {
        Layer::Ingest => "ingest",
        Layer::Feed => "feed",
        Layer::Shard => "shard",
        Layer::Destination => "destination",
    };
    let plan = |verb: &str, kinds: &[&str], output_id: &str| PlanRequest {
        intent: format!(
            "{verb} output {output_id} ({layer_name} layer): {}",
            cause_summary(cause)
        ),
        pipeline_id: alert.pipeline_id.clone(),
        proposed_changes: kinds.iter().map(|kind| change(kind, output_id)).collect(),
    };
    let Some(output_id) = alert.output_id.as_deref() else {
        return (None, alert.recommended_action.clone());
    };
    let restart = ["stopOutput", "startOutput"];
    match cause {
        Cause::ShardPanicked { .. } | Cause::ShardStalled { .. } | Cause::ShardStopped { .. } => (
            Some(plan("Restart", &restart, output_id)),
            "Restarting the output starts a fresh attempt on a running shard.".to_string(),
        ),
        Cause::RetriesExhausted | Cause::RetryBudgetNearlySpent { .. } => (
            Some(plan("Restart", &restart, output_id)),
            "A restart gives the output a fresh retry budget; if the destination still \
             fails, check it before applying."
                .to_string(),
        ),
        Cause::Backpressured { .. } | Cause::NoProgress => (
            Some(plan("Restart", &restart, output_id)),
            "The destination or the network is slow; a restart reconnects. If it recurs, \
             check the path to the destination."
                .to_string(),
        ),
        Cause::Rejected { .. } => (
            Some(plan("Stop", &["stopOutput"], output_id)),
            "The destination refuses the stream: stop retrying, fix the output's URL or \
             stream key, then start it."
                .to_string(),
        ),
        Cause::CapacityRefused { .. } => (
            None,
            "The host is at its egress capacity: stop lower-priority outputs or add a host."
                .to_string(),
        ),
        Cause::NoPublisher | Cause::StageNotProducing { .. } => {
            (None, alert.recommended_action.clone())
        }
    }
}

#[cfg(test)]
mod tests;
