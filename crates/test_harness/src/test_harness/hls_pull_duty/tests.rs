//! Unit tests for the `hls-pull-duty` mode's pure logic: option parsing and
//! the checks over the input row, probe and log.

use super::*;
use serde_json::json;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

#[test]
fn defaults_are_a_short_single_pull_run() {
    let options = HlsPullOptions::from_args(&[]).expect("defaults parse");
    assert_eq!(options.duration_secs, 15);
    assert_eq!(options.reconcile_interval_ms, 1_000);
    assert!(!options.fail_unresolvable);
    assert_eq!(options.window(), Duration::from_secs(15));
    assert_eq!(options.backstop(), Duration::from_secs(10));
}

#[test]
fn flags_parse_in_both_forms() {
    let spaced = HlsPullOptions::from_args(&args(&[
        "--duration-secs",
        "20",
        "--reconcile-interval-ms",
        "250",
    ]))
    .expect("spaced flags parse");
    assert_eq!(spaced.duration_secs, 20);
    assert_eq!(spaced.reconcile_interval_ms, 250);
    assert_eq!(spaced.backstop(), Duration::from_millis(2_500));

    let inline = HlsPullOptions::from_args(&args(&["--duration-secs=30", "--fail-unresolvable"]))
        .expect("inline flags parse");
    assert_eq!(inline.duration_secs, 30);
    assert!(inline.fail_unresolvable);
}

#[test]
fn unknown_and_malformed_options_are_rejected() {
    for raw in [
        vec!["--nope"],
        vec!["--duration-secs"],
        vec!["--duration-secs", "0"],
        vec!["--duration-secs", "x"],
        vec!["--reconcile-interval-ms", "50"],
        vec!["--reconcile-interval-ms", "60001"],
        vec!["--fail-unresolvable=yes"],
    ] {
        let parsed = HlsPullOptions::from_args(&args(&raw));
        assert!(parsed.is_err(), "{raw:?} should be rejected");
    }
}

#[test]
fn failing_variant_points_the_good_pull_at_the_unresolvable_url() {
    let preview = "http://127.0.0.1:3030/hls/pipe/master.m3u8";
    let good = HlsPullOptions::default();
    assert_eq!(good.pull_source_url(preview), preview);
    assert_eq!(good.bad_pull_url(), UNRESOLVABLE_PULL_URL);

    let failing = HlsPullOptions {
        fail_unresolvable: true,
        ..HlsPullOptions::default()
    };
    assert_eq!(failing.pull_source_url(preview), UNRESOLVABLE_PULL_URL);
    assert!(
        UNRESOLVABLE_PULL_URL.contains(".invalid"),
        "the unresolvable URL must be under the reserved .invalid TLD"
    );
}

#[test]
fn input_row_and_session_protocol_read_the_runtime_fields() {
    let inputs = json!({
        "inputs": [
            {"id": "in-1", "kind": "hlsPull", "runtime": {"connected": false, "protocol": null}},
            {"id": "in-2", "kind": "hlsPull", "runtime": {"connected": true, "protocol": "hls-pull"}},
        ]
    });
    let row = find_input(&inputs, "in-2").expect("input found");
    assert_eq!(session_protocol(row), Some("hls-pull"));
    let idle = find_input(&inputs, "in-1").expect("input found");
    assert_eq!(session_protocol(idle), None);
    assert!(find_input(&inputs, "missing").is_none());
}

#[test]
fn pull_published_requires_bytes_video_audio_and_a_keyframe() {
    let probe = json!({
        "ingest": {"protocol": "hls-pull", "bytesReceived": 100, "uptimeSeconds": 10.0},
        "video": {"codec": "h264"},
        "audioTracks": [{"codec": "aac"}],
        "gop": {"keyframeCount": 3, "averageIntervalSec": 2.0}
    });
    assert!(pull_published_ok(&probe));
    assert_eq!(pull_media_summary(&probe)["mediaSeconds"], 4.0);

    // The preview master's audio is a separate rendition the pull follows: a
    // pull that publishes no audio has lost it.
    let mut no_audio = probe.clone();
    no_audio["audioTracks"] = json!([]);
    assert!(!pull_published_ok(&no_audio));

    let mut no_bytes = probe.clone();
    no_bytes["ingest"]["bytesReceived"] = json!(0);
    assert!(!pull_published_ok(&no_bytes));

    let mut no_video = probe.clone();
    no_video["video"] = json!(null);
    assert!(!pull_published_ok(&no_video));

    let no_gop = json!({"ingest": {"bytesReceived": 10}, "video": {"codec": "h264"}});
    assert!(!pull_published_ok(&no_gop));
}

#[test]
fn pull_delivered_requires_monotone_video_and_audio() {
    let delivered = json!({
        "videoCount": 10,
        "audioCount": 20,
        "keyframeCount": 2,
        "bytes": 4096,
        "dtsMonotone": true
    });
    assert!(pull_delivered_ok(&delivered));

    for broken in [
        ("videoCount", json!(0)),
        ("audioCount", json!(0)),
        ("keyframeCount", json!(0)),
        ("bytes", json!(0)),
        ("dtsMonotone", json!(false)),
    ] {
        let mut value = delivered.clone();
        value[broken.0] = broken.1;
        assert!(!pull_delivered_ok(&value), "{} must fail", broken.0);
    }
}

#[test]
fn log_reports_pull_failure_needs_the_input_and_the_message() {
    let log = "2026-10-17T00:00:00Z WARN restream: pipeline=pipe input=input_abc error=\"dns\" HLS pull failed; retrying\n";
    assert!(log_reports_pull_failure(log, "input_abc"));
    assert!(!log_reports_pull_failure(log, "input_other"));
    assert!(!log_reports_pull_failure("nothing here", "input_abc"));
}

#[test]
fn primary_input_id_finds_the_primary_row() {
    let inputs = json!({
        "inputs": [
            {"id": "in-pull", "role": "backup", "selected": true},
            {"id": "input_primary_p", "role": "primary", "selected": false},
        ]
    });
    assert_eq!(primary_input_id(&inputs), Some("input_primary_p"));
    assert_eq!(primary_input_id(&json!({"inputs": []})), None);
    assert_eq!(primary_input_id(&json!({})), None);
}

#[test]
fn pipeline_present_finds_the_row() {
    let list = json!({"pipelines": [{"id": "p1"}, {"id": "p2"}]});
    assert!(pipeline_present(&list, "p2"));
    assert!(!pipeline_present(&list, "p3"));
    assert!(!pipeline_present(&json!({}), "p1"));
}

/// Keep-up is measured inside the window on the sink's monotonic media clock.
/// The probe's keyframe timeline is capped at 30 entries, so its media seconds
/// freeze while uptime keeps growing: a lifetime comparison fails a healthy
/// pull on a window longer than about two minutes, which is why the check
/// takes two samples instead.
#[test]
fn keep_up_is_measured_over_the_window() {
    let start = json!({"mediaProgressMs": 0});
    assert!(pull_keeps_up_over_window(
        &start,
        &json!({"mediaProgressMs": 14_000}),
        15.0
    ));
    assert!(
        !pull_keeps_up_over_window(&start, &json!({"mediaProgressMs": 6_000}), 15.0),
        "6 s of media in a 15 s window is below half"
    );
    assert!(
        !pull_keeps_up_over_window(&start, &start, 60.0),
        "a frozen media clock fails however long the window ran"
    );
    assert!(
        !pull_keeps_up_over_window(&start, &json!({"mediaProgressMs": 14_000}), 0.0),
        "a zero-length window says nothing"
    );
}
