//! Unit tests for the `hls-push-duty` mode's pure logic: option parsing, the
//! FFmpeg argv builders, and the result-JSON joins.

use super::*;
use serde_json::json;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

#[test]
fn defaults_are_a_short_baseline_smoke() {
    let options = HlsPushOptions::from_args(&[]).expect("defaults parse");
    assert_eq!(options.base_publishers, 2);
    assert_eq!(options.hls_publishers, 0);
    assert_eq!(options.segment_type, HlsSegmentType::Ts);
    assert_eq!(options.duration_secs, 12);
    assert_eq!(options.segment_secs, 2);
    assert_eq!(options.total_publishers(), 2);
    assert_eq!(options.hls_publishers, 0);
}

#[test]
fn flags_parse_in_both_forms() {
    let spaced = HlsPushOptions::from_args(&args(&[
        "--base-publishers",
        "3",
        "--hls-publishers",
        "4",
        "--segment-type",
        "fmp4",
        "--duration-secs",
        "20",
        "--segment-secs",
        "1",
    ]))
    .expect("spaced flags parse");
    assert_eq!(spaced.base_publishers, 3);
    assert_eq!(spaced.hls_publishers, 4);
    assert_eq!(spaced.segment_type, HlsSegmentType::Fmp4);
    assert_eq!(spaced.duration_secs, 20);
    assert_eq!(spaced.segment_secs, 1);
    assert_eq!(spaced.total_publishers(), 7);

    let inline = HlsPushOptions::from_args(&args(&["--hls-publishers=2", "--segment-type=ts"]))
        .expect("inline flags parse");
    assert_eq!(inline.hls_publishers, 2);
    assert_eq!(inline.segment_type, HlsSegmentType::Ts);
}

#[test]
fn unknown_and_malformed_options_are_rejected() {
    for raw in [
        vec!["--nope"],
        vec!["--hls-publishers"],
        vec!["--segment-type", "mkv"],
        vec!["--hls-publishers", "x"],
        vec!["--segment-secs", "0"],
        vec!["--duration-secs", "0"],
    ] {
        let parsed = HlsPushOptions::from_args(&args(&raw));
        assert!(parsed.is_err(), "{raw:?} should be rejected");
    }
}

#[test]
fn zero_and_oversized_publisher_counts_are_rejected() {
    let none = HlsPushOptions::from_args(&args(&["--base-publishers", "0"]));
    assert!(none.is_err());
    let too_many = HlsPushOptions::from_args(&args(&["--hls-publishers", "65"]));
    assert!(too_many.is_err());
}

#[test]
fn segment_type_selects_the_container_arguments() {
    assert!(HlsSegmentType::Ts.ffmpeg_args().is_empty());
    let fmp4 = HlsSegmentType::Fmp4.ffmpeg_args();
    assert!(fmp4.contains(&"-hls_segment_type"));
    assert!(fmp4.contains(&"fmp4"));
    assert!(fmp4.contains(&"-bsf:a"));
    assert!(fmp4.contains(&"aac_adtstoasc"));
}

#[test]
fn hls_publisher_argv_pushes_over_put() {
    let argv = hls_publisher_args(
        std::path::Path::new("/fixtures/correctness-h264.ts"),
        "http://127.0.0.1:3030/ingest/hls/key/index.m3u8",
        2,
        HlsSegmentType::Fmp4,
    );
    let text = argv.join(" ");
    assert!(text.contains("-f hls"));
    assert!(text.contains("-method PUT"));
    assert!(text.contains("-hls_time 2"));
    assert!(text.contains("-hls_segment_type fmp4"));
    assert!(text.contains("-re -stream_loop -1"));
    assert_eq!(
        argv.last().map(String::as_str),
        Some("http://127.0.0.1:3030/ingest/hls/key/index.m3u8")
    );

    let ts = hls_publisher_args(
        std::path::Path::new("f.ts"),
        "http://h/ingest/hls/k/i.m3u8",
        1,
        HlsSegmentType::Ts,
    );
    assert!(!ts.join(" ").contains("-hls_segment_type"));
}

#[test]
fn rtmp_publisher_argv_publishes_flv() {
    let argv = rtmp_publisher_args(
        std::path::Path::new("/fixtures/correctness-h264.ts"),
        "rtmp://127.0.0.1:1935/live/key",
    );
    let text = argv.join(" ");
    assert!(text.contains("-f flv"));
    assert!(text.contains("-re -stream_loop -1"));
    assert_eq!(
        argv.last().map(String::as_str),
        Some("rtmp://127.0.0.1:1935/live/key")
    );
}

#[test]
fn egress_shard_lookup_indexes_by_protocol_and_index() {
    let shards = vec![
        json!({"protocol": "sink", "shardIndex": 0, "state": "healthy", "resyncCount": 0}),
        json!({"protocol": "rtmp", "shardIndex": 0, "state": "stalled", "resyncCount": 2}),
        json!({"protocol": "sink", "shardIndex": 1, "state": "healthy"}),
    ];
    let lookup = egress_shard_lookup(&shards);
    assert_eq!(lookup.len(), 3);
    assert_eq!(
        lookup
            .get(&("rtmp".to_string(), 0))
            .and_then(|shard| shard.get("state"))
            .and_then(serde_json::Value::as_str),
        Some("stalled")
    );
    assert!(lookup.contains_key(&("sink".to_string(), 1)));
}

#[test]
fn pipeline_egress_summary_reports_stall_resync_and_drop() {
    let egress = json!({
        "outputId": "out-1",
        "resyncCount": 3,
        "feedLagUnits": 7,
        "shardId": 0,
        "fabricFeed": "sink:feed"
    });
    let shard = json!({
        "protocol": "sink",
        "shardIndex": 0,
        "state": "stalled",
        "queueOverflows": 4,
        "budgetExhaustions": 2,
        "srtOwners": [
            {"present": true, "rxRingDropped": 5, "txFailuresDropped": 6},
            {"present": false, "rxRingDropped": 100, "txFailuresDropped": 100}
        ]
    });
    let summary = pipeline_egress_summary(Some(&egress), Some(&shard));
    assert_eq!(summary["resyncCount"], 3);
    assert_eq!(summary["feedLagUnits"], 7);
    assert_eq!(summary["stalled"], true);
    assert_eq!(summary["shardState"], "stalled");
    assert_eq!(summary["queueOverflows"], 4);
    assert_eq!(summary["rxRingDropped"], 5);
    assert_eq!(summary["txFailuresDropped"], 6);
}

#[test]
fn pipeline_egress_summary_nulls_missing_counters() {
    let summary = pipeline_egress_summary(None, None);
    assert!(summary["resyncCount"].is_null());
    assert!(summary["queueOverflows"].is_null());
    assert_eq!(summary["stalled"], false);
}

#[test]
fn input_and_gop_helpers_read_the_runtime_fields() {
    let probe = json!({
        "ingest": {"bytesReceived": 4096, "bitrateKbps": 1200.0},
        "video": {"codec": "h264"},
        "audioTracks": [{"codec": "aac"}],
        "gop": {"keyframeCount": 3, "averageIntervalSec": 2.0}
    });
    let input = input_published_summary(&probe);
    assert_eq!(input["bytesReceived"], 4096);
    assert_eq!(input["videoPresent"], true);
    assert_eq!(input["audioTracks"], 1);
    assert_eq!(input["keyframeCount"], 3);
    assert_eq!(gop_keyframe_count(&probe), Some(3));

    let no_gop = json!({"ingest": {"bytesReceived": 0}});
    assert_eq!(gop_keyframe_count(&no_gop), None);
    assert_eq!(input_published_summary(&no_gop)["videoPresent"], false);
}

#[test]
fn published_and_delivered_checks_require_media() {
    let probe = json!({
        "ingest": {"bytesReceived": 100},
        "video": {"codec": "h264"},
        "audioTracks": [{"codec": "aac"}]
    });
    assert!(published_ok(&probe, Some(1)));
    assert!(!published_ok(&probe, None));
    assert!(!published_ok(&probe, Some(0)));
    let mut no_bytes = probe.clone();
    no_bytes["ingest"]["bytesReceived"] = json!(0);
    assert!(!published_ok(&no_bytes, Some(1)));
    // Every publisher maps the fixture's AAC: an ingest that reports no
    // audio tracks lost them, whatever it says about its source.
    let mut no_audio_tracks = probe.clone();
    no_audio_tracks["audioTracks"] = json!([]);
    assert!(!published_ok(&no_audio_tracks, Some(1)));

    let delivered = json!({
        "videoCount": 10,
        "audioCount": 20,
        "keyframeCount": 1,
        "bytes": 4096,
        "dtsMonotone": true
    });
    assert!(delivered_ok(&delivered));
    let mut non_monotone = delivered.clone();
    non_monotone["dtsMonotone"] = json!(false);
    assert!(!delivered_ok(&non_monotone));
    // Several AAC frames packed per audio message: audio falls far below
    // half a message per video frame.
    let mut packed_audio = delivered.clone();
    packed_audio["videoCount"] = json!(160);
    packed_audio["audioCount"] = json!(20);
    assert!(!delivered_ok(&packed_audio));
    let mut no_keyframe = delivered.clone();
    no_keyframe["keyframeCount"] = json!(0);
    assert!(!delivered_ok(&no_keyframe));
    let mut no_video = delivered.clone();
    no_video["videoCount"] = json!(0);
    assert!(!delivered_ok(&no_video));
    let mut no_bytes = delivered.clone();
    no_bytes["bytes"] = json!(0);
    assert!(!delivered_ok(&no_bytes));

    // Video delivered with `audioCount: 0` is the HLS push failure the mode
    // catches; the source always carries audio, so it is always a failure.
    let mut no_audio = delivered.clone();
    no_audio["audioCount"] = json!(0);
    assert!(!delivered_ok(&no_audio));
}

/// The rated window is what this mode measures, so its pass checks require
/// progress *inside* the window: a sink whose counters are unchanged from the
/// baseline delivered nothing during it, however large its lifetime totals are.
/// A mutation that drops the baseline comparison (or the media-clock clause)
/// fails here.
#[test]
fn progress_checks_require_movement_since_the_baseline() {
    let baseline_probe = json!({"ingest": {"bytesReceived": 4096}});
    let moved = json!({"ingest": {"bytesReceived": 8192}});
    assert!(published_progress_ok(&baseline_probe, &moved));
    assert!(
        !published_progress_ok(&baseline_probe, &baseline_probe),
        "an unchanged byte counter received nothing"
    );
    assert!(!published_progress_ok(
        &baseline_probe,
        &json!({"ingest": {}})
    ));

    let baseline_sink = json!({"videoCount": 40, "audioCount": 40, "mediaProgressMs": 20_000});
    let delivered = json!({"videoCount": 45, "audioCount": 45, "mediaProgressMs": 22_000});
    assert!(delivered_progress_ok(&baseline_sink, &delivered));
    assert!(
        !delivered_progress_ok(&baseline_sink, &baseline_sink),
        "an unchanged sink delivered nothing"
    );
    // Counts that moved while the media clock froze is a stall, not progress:
    // the sink received messages, not media.
    let stalled = json!({"videoCount": 45, "audioCount": 45, "mediaProgressMs": 20_000});
    assert!(!delivered_progress_ok(&baseline_sink, &stalled));
}
