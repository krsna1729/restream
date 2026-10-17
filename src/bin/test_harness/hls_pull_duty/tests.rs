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
fn pull_published_requires_bytes_video_and_a_keyframe_without_audio() {
    let probe = json!({
        "ingest": {"protocol": "hls-pull", "bytesReceived": 100, "uptimeSeconds": 10.0},
        "video": {"codec": "h264"},
        "audioTracks": [],
        "gop": {"keyframeCount": 3, "averageIntervalSec": 2.0}
    });
    // The pull follows one rendition, so the preview master's separate audio
    // rendition is not in the ring; the mode reports that gap as `audioGap`.
    assert!(pull_published_ok(&probe));
    assert_eq!(pull_media_summary(&probe)["mediaSeconds"], 4.0);
    assert_eq!(
        pull_media_summary(&probe)["audioTracks"],
        0,
        "the report shows the absent audio"
    );

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
fn pull_delivered_requires_monotone_video_media() {
    let delivered = json!({
        "videoCount": 10,
        "keyframeCount": 2,
        "bytes": 4096,
        "dtsMonotone": true
    });
    assert!(pull_delivered_ok(&delivered));

    for broken in [
        ("videoCount", json!(0)),
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
fn pull_keeps_up_compares_the_media_clock_to_the_wall_clock() {
    let healthy = json!({
        "ingest": {"uptimeSeconds": 12.0},
        "gop": {"keyframeCount": 7, "averageIntervalSec": 2.0}
    });
    // 6 intervals * 2 s = 12 s of media against 12 s of wall time.
    assert!(pull_keeps_up(&healthy));

    // A stalled pull: the wall clock advances while the media clock freezes.
    let stalled = json!({
        "ingest": {"uptimeSeconds": 60.0},
        "gop": {"keyframeCount": 7, "averageIntervalSec": 2.0}
    });
    assert!(!pull_keeps_up(&stalled));

    // No media probe at all cannot keep up.
    let none = json!({"ingest": {"uptimeSeconds": 60.0}});
    assert!(!pull_keeps_up(&none));
}

#[test]
fn log_reports_pull_failure_needs_the_input_and_the_message() {
    let log = "2026-10-17T00:00:00Z WARN restream: pipeline=pipe input=input_abc error=\"dns\" HLS pull failed; retrying\n";
    assert!(log_reports_pull_failure(log, "input_abc"));
    assert!(!log_reports_pull_failure(log, "input_other"));
    assert!(!log_reports_pull_failure("nothing here", "input_abc"));
}

#[test]
fn audio_rendition_gap_reads_the_pulls_own_line() {
    let log = "2026-10-17T00:00:00Z WARN restream: pipeline=pipe input=input_abc \
               group=\"audio\" name=\"Track 1 (en)\" uri=\"http://origin/audio/0/index.m3u8\" \
               HLS pull follows the video variant only: the audio is the separate \
               EXT-X-MEDIA rendition \"Track 1 (en)\" of group \"audio\", which this puller \
               does not follow yet; the pulled stream is video-only\n";
    let found = audio_rendition_gap(log, "input_abc").expect("the line is found");
    assert!(found.contains("EXT-X-MEDIA rendition"), "{found}");
    assert!(
        audio_rendition_gap(log, "input_other").is_none(),
        "another input's line is not this pull's"
    );
    assert!(
        audio_rendition_gap("nothing here", "input_abc").is_none(),
        "a log without the line reports no gap"
    );
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
