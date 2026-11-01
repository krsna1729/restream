//! Explanations of incidents recorded from a live server: each fixture is
//! the alert and the pipeline's flight record as the server served them
//! (`GET /api/v1/agent/incidents/explanation` and `…/flight-recorder`).

use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Recorded {
    alert: Alert,
    incident_from_ms: u64,
    to_ms: u64,
    samples: Vec<FlightSample>,
}

/// An output to a closed port while a publisher ran: the destination
/// rejected it.
fn destination() -> Recorded {
    serde_json::from_str(include_str!("destination_incident.json")).unwrap()
}

/// The same pipeline after its publisher was killed.
fn ingest() -> Recorded {
    serde_json::from_str(include_str!("ingest_incident.json")).unwrap()
}

fn run(recorded: &Recorded) -> Explanation {
    explain(
        &recorded.alert,
        recorded.incident_from_ms,
        recorded.to_ms,
        &recorded.samples,
    )
}

fn proposed_kinds(explanation: &Explanation) -> Vec<&str> {
    explanation
        .proposal
        .iter()
        .flat_map(|plan| &plan.proposed_changes)
        .map(|change| change.kind.as_str())
        .collect()
}

#[test]
fn a_rejecting_destination_is_named_and_its_output_stopped() {
    let recorded = destination();
    let explanation = run(&recorded);

    assert_eq!(explanation.layer, Some(Layer::Destination));
    let ruled: Vec<Layer> = explanation.ruled_out.iter().map(|r| r.layer).collect();
    assert_eq!(ruled, [Layer::Ingest, Layer::Feed]);
    assert_eq!(proposed_kinds(&explanation), ["stopOutput"]);
    let plan = explanation.proposal.as_ref().unwrap();
    assert_eq!(plan.pipeline_id, recorded.alert.pipeline_id);
    assert_eq!(plan.proposed_changes[0].output_id, recorded.alert.output_id);
    assert!(plan.intent.contains("destination layer"), "{}", plan.intent);
}

#[test]
fn a_missing_publisher_is_named_and_no_plant_change_proposed() {
    let explanation = run(&ingest());

    assert_eq!(explanation.layer, Some(Layer::Ingest));
    // The onset minute is read but not counted: the publisher was still up
    // for most of it.
    let window = explanation.window;
    assert_eq!((window.samples, window.incident_samples), (26, 10));
    assert!(
        explanation.summary.contains("10 of 10 samples"),
        "{}",
        explanation.summary
    );
    assert!(explanation.ruled_out.is_empty());
    assert!(explanation.proposal.is_none());
    assert!(
        explanation.recommendation.contains("publisher"),
        "{}",
        explanation.recommendation
    );
}

#[test]
fn an_ingest_gap_through_the_incident_explains_a_destination_alert() {
    let mut recorded = destination();
    let from = recorded.incident_from_ms;
    for sample in recorded.samples.iter_mut().filter(|s| s.at_ms >= from) {
        sample.ingest = None;
    }
    let explanation = run(&recorded);

    assert_eq!(explanation.layer, Some(Layer::Ingest));
    assert!(explanation.proposal.is_none());
}

#[test]
fn a_one_sample_ingest_blip_does_not_move_the_blame() {
    let mut recorded = destination();
    let from = recorded.incident_from_ms;
    let blip = recorded
        .samples
        .iter_mut()
        .find(|s| s.at_ms >= from)
        .unwrap();
    blip.ingest = None;
    let explanation = run(&recorded);

    assert_eq!(explanation.layer, Some(Layer::Destination));
    assert_eq!(proposed_kinds(&explanation), ["stopOutput"]);
}
