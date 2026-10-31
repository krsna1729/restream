//! Alerts from the picture and sound checks and from destination
//! baselines.

use super::*;
use serde_json::json;

fn snapshot_with_pipeline(pipeline_id: &str, input_status: &str) -> serde_json::Value {
    json!({
        "generatedAt": "2026-06-25T00:00:00Z",
        "srtListener": { "bondingAvailable": true },
        "pipelines": {
            pipeline_id: {
                "input": {
                    "status": input_status,
                    "readerMetrics": []
                },
                "outputs": {}
            }
        }
    })
}

fn snapshot_with_qa(qa: serde_json::Value) -> serde_json::Value {
    let mut snap = snapshot_with_pipeline("pipe1", "on");
    snap["pipelines"]["pipe1"]["input"]["qa"] = qa;
    snap
}

fn qa_alert_ids(qa: serde_json::Value) -> Vec<String> {
    derive_alerts(&snapshot_with_qa(qa))
        .into_iter()
        .map(|alert| alert.id)
        .collect()
}

#[test]
fn picture_and_sound_checks_alert_once_a_condition_has_held_long_enough() {
    use restream_engine::stream_qa::{BLACK_ALERT_MS, SILENT_ALERT_MS, STILL_ALERT_MS};
    // Short of each threshold: nothing.
    assert!(
        qa_alert_ids(json!({
            "blackForMs": BLACK_ALERT_MS - 1,
            "silentForMs": SILENT_ALERT_MS - 1,
            "stillForMs": STILL_ALERT_MS - 1,
        }))
        .is_empty()
    );
    // Black is critical and names the pipeline's input; silence beside it.
    let alerts = derive_alerts(&snapshot_with_qa(json!({
        "blackForMs": BLACK_ALERT_MS,
        "stillForMs": STILL_ALERT_MS,
        "silentForMs": SILENT_ALERT_MS,
    })));
    let ids: Vec<&str> = alerts.iter().map(|alert| alert.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "pipeline:pipe1:input:picture_black",
            "pipeline:pipe1:input:audio_silent"
        ],
        "a black picture is not also reported as frozen"
    );
    assert_eq!(alerts[0].severity, Severity::Critical);
    assert_eq!(alerts[1].severity, Severity::Warning);
    // A still picture that is not black is a warning of its own.
    assert_eq!(
        qa_alert_ids(json!({ "stillForMs": STILL_ALERT_MS })),
        ["pipeline:pipe1:input:picture_frozen"]
    );
    // Absent findings (checks off, or stale) raise nothing.
    assert!(qa_alert_ids(json!({ "blackForMs": null, "loudnessLufs": -23.0 })).is_empty());
}

#[test]
fn a_still_slate_and_a_silent_later_track_are_named_for_what_they_are() {
    use restream_engine::stream_qa::{SILENT_ALERT_MS, STILL_ALERT_MS};
    assert_eq!(
        qa_alert_ids(json!({ "stillForMs": STILL_ALERT_MS, "stillIsSlate": true })),
        ["pipeline:pipe1:input:picture_slate"]
    );
    assert_eq!(
        qa_alert_ids(json!({
            "silentTracks": [
                { "track": 1, "silentForMs": SILENT_ALERT_MS },
                { "track": 2, "silentForMs": SILENT_ALERT_MS - 1 },
            ],
        })),
        ["pipeline:pipe1:input:audio_silent:track2"],
        "track 2 (index 1) has been silent long enough; track 3 not yet"
    );
}

#[test]
fn a_destination_off_its_usual_is_a_named_warning() {
    use crate::destination_baselines::{Anomaly, Signal};
    let anomalies = [
        Anomaly {
            pipeline_id: "pipe1".into(),
            output_id: "out1".into(),
            signal: Signal::DeliveryRatio,
            value: 0.96,
            usual: 0.999,
            since_ms: 1,
        },
        Anomaly {
            pipeline_id: "pipe1".into(),
            output_id: "out1".into(),
            signal: Signal::RttMs,
            value: 180.0,
            usual: 40.0,
            since_ms: 1,
        },
    ];
    let mut alerts = anomaly_alerts(&anomalies, "2026-10-24T00:00:00Z");
    let ids: Vec<&str> = alerts.iter().map(|alert| alert.id.as_str()).collect();
    assert_eq!(
        ids,
        ["output:out1:anomaly:delivery", "output:out1:anomaly:rtt"]
    );
    assert!(
        alerts
            .iter()
            .all(|alert| alert.severity == Severity::Warning)
    );
    assert_eq!(
        alerts[1].evidence[0].signal,
        "round trip 180 ms, usually 40 ms"
    );
    let pipelines = [("pipe1".to_string(), "Sunday".to_string())].into();
    let outputs = [("out1".to_string(), "YouTube main".to_string())].into();
    name_subjects(&mut alerts, &pipelines, &outputs);
    assert_eq!(
        alerts[0].title,
        "Sunday: YouTube main delivers less than usual"
    );
}
