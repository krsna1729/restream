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
    assert_eq!(inline.method, HlsMethod::Put, "PUT by default");

    let post = HlsPushOptions::from_args(&args(&["--method", "post"])).expect("--method parses");
    assert_eq!(post.method, HlsMethod::Post);
    assert!(HlsPushOptions::from_args(&args(&["--method", "patch"])).is_err());
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
        HlsMethod::Put,
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
        HlsMethod::Post,
    );
    assert!(!ts.join(" ").contains("-hls_segment_type"));
    assert!(ts.join(" ").contains("-method POST"));
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

#[test]
fn pull_sessions_add_a_source_and_their_own_pipelines() {
    let pulls = HlsPushOptions::from_args(&args(&["--pull-sessions", "4", "--poll-usage"]))
        .expect("pull arm parses");
    assert_eq!(pulls.pull_sessions, 4);
    assert!(pulls.poll_usage);
    assert_eq!(pulls.total_publishers(), 3, "2 base + the pull source");
    assert_eq!(pulls.total_pipelines(), 7, "plus one pipeline per pull");

    let base = HlsPushOptions::from_args(&[]).expect("defaults parse");
    assert!(!base.poll_usage, "per-poll getrusage is opt-in");
    assert!(!base.host_probe, "the host stall probe is opt-in");
    assert_eq!(base.total_pipelines(), 2, "no pull, no pull source");

    let probed = HlsPushOptions::from_args(&args(&["--host-probe"])).expect("probe arm parses");
    assert!(probed.host_probe);
    assert!(HlsPushOptions::from_args(&args(&["--host-probe=1"])).is_err());

    assert!(HlsPushOptions::from_args(&args(&["--poll-usage=1"])).is_err());
    assert!(
        HlsPushOptions::from_args(&args(&["--base-publishers", "0", "--pull-sessions", "2"]))
            .is_err(),
        "pulls alone are not a base arm"
    );
    assert!(
        HlsPushOptions::from_args(&args(&["--pull-sessions", "62"])).is_err(),
        "2 base + source + 62 pulls is over the 64-pipeline cap"
    );
    assert!(HlsPushOptions::from_args(&args(&["--pull-sessions", "61"])).is_ok());
}

#[test]
fn host_memory_counters_parse_and_subtract() {
    let vmstat = "nr_free_pages 100\ncompact_stall 7\nallocstall_normal 3\n\
                  allocstall_movable 5\nthp_fault_alloc 0\npgmajfault 9\ncompact_fail 2\nbroken\n";
    let kept = host_memory::parse_counters(vmstat, host_memory::vmstat_key_kept);
    assert_eq!(
        kept.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "allocstall_movable",
            "allocstall_normal",
            "compact_stall",
            "pgmajfault",
            "thp_fault_alloc"
        ]
    );
    let later = host_memory::parse_counters(
        "compact_stall 10\nallocstall_normal 3\nallocstall_movable 4\npgmajfault 12\nthp_fault_alloc 1\nallocstall_dma 2\n",
        host_memory::vmstat_key_kept,
    );
    let deltas = host_memory::counter_deltas(&kept, &later);
    assert_eq!(deltas["compact_stall"], 3);
    assert_eq!(deltas["allocstall_normal"], 0);
    assert_eq!(
        deltas["allocstall_movable"], 0,
        "a counter that went back reads 0"
    );
    assert_eq!(
        deltas["allocstall_dma"], 2,
        "a counter absent before counts from 0"
    );
    assert_eq!(deltas["pgmajfault"], 3);

    let events = host_memory::parse_counters("low 0\nhigh 2\nmax 1\noom 0\n", |_| true);
    assert_eq!(events.len(), 4);
    assert_eq!(
        host_memory::cgroup_v2_path("0::/user.slice/session-3.scope\n").as_deref(),
        Some("/user.slice/session-3.scope")
    );
    assert_eq!(host_memory::cgroup_v2_path("1:name=systemd:/x\n"), None);
    assert_eq!(
        host_memory::thp_mode("always [madvise] never\n").as_deref(),
        Some("madvise")
    );
    assert_eq!(host_memory::thp_mode("always madvise never"), None);
}

fn executor(classes: Value, services: Value) -> Value {
    json!({"classes": classes, "services": services})
}

/// The Q-074 gate is counted in the window and per HLS session: a mutation
/// that reports lifetime counts (no subtraction) or divides by the window
/// in seconds rather than minutes, or forgets the session count, fails here.
#[test]
fn class_windows_subtract_the_start_and_rate_slow_polls_per_session_minute() {
    let start = executor(
        json!([
            {"class": "hlsIngest", "active": 8, "polls": 1000, "busyUs": 90_000, "cpuUs": 80_000,
             "maxCpuUs": 2_000, "maxPollUs": 2_500,
             "pollsOverCpu1Ms": 4, "pollsOverCpu4Ms": 1, "pollsOverCpu16Ms": 0,
             "usage": {"all": {"userUs": 50_000, "sysUs": 30_000, "minorFaults": 700, "majorFaults": 0},
                       "overCpu1Ms": {"userUs": 2_000, "sysUs": 3_000, "minorFaults": 300, "majorFaults": 0}}},
            {"class": "recording", "active": 0, "polls": 5, "busyUs": 10, "cpuUs": 9,
             "maxCpuUs": 3, "maxPollUs": 4,
             "pollsOverCpu1Ms": 0, "pollsOverCpu4Ms": 0, "pollsOverCpu16Ms": 0}
        ]),
        json!([]),
    );
    let end = executor(
        json!([
            {"class": "hlsIngest", "active": 8, "polls": 5000, "busyUs": 450_000, "cpuUs": 400_000,
             "maxCpuUs": 9_000, "maxPollUs": 9_500,
             "pollsOverCpu1Ms": 28, "pollsOverCpu4Ms": 9, "pollsOverCpu16Ms": 0,
             "usage": {"all": {"userUs": 250_000, "sysUs": 150_000, "minorFaults": 9_700, "majorFaults": 1},
                       "overCpu1Ms": {"userUs": 12_000, "sysUs": 23_000, "minorFaults": 4_300, "majorFaults": 0}}},
            {"class": "recording", "active": 0, "polls": 5, "busyUs": 10, "cpuUs": 9,
             "maxCpuUs": 3, "maxPollUs": 4,
             "pollsOverCpu1Ms": 0, "pollsOverCpu4Ms": 0, "pollsOverCpu16Ms": 0}
        ]),
        json!([]),
    );
    let windows = executor_class_windows(&start, &end, 120.0);
    assert_eq!(windows.len(), 1, "a class that did not poll is left out");
    let hls = &windows[0];
    assert_eq!(hls["class"], "hlsIngest");
    assert_eq!(hls["polls"], 4000);
    assert_eq!(hls["cpuUs"], 320_000);
    assert_eq!(hls["cpuUsPerPoll"], 80.0);
    assert_eq!(hls["pollsOverCpu4Ms"], 8);
    assert_eq!(hls["maxCpuUs"], 9_000, "the maximum stays the lifetime one");
    // 8 polls over 4 ms in 2 minutes across 8 sessions: 0.5 per minute each.
    assert_eq!(hls["perMinutePerSession"]["over4Ms"], 0.5);
    assert_eq!(hls["perMinutePerSession"]["over1Ms"], 1.5);
    assert_eq!(hls["perMinutePerSession"]["over16Ms"], 0.0);
    assert_eq!(hls["usage"]["all"]["sysUs"], 120_000);
    assert_eq!(hls["usage"]["overCpu1Ms"]["minorFaults"], 4_000);
    assert_eq!(hls["usage"]["all"]["majorFaults"], 1);

    let without_usage = executor(
        json!([{"class": "hlsIngest", "active": 0, "polls": 3, "cpuUs": 30}]),
        json!([]),
    );
    let rows = executor_class_windows(&Value::Null, &without_usage, 60.0);
    assert!(rows[0].get("usage").is_none(), "no usage, no usage block");
    assert!(
        rows[0]["perMinutePerSession"]["over4Ms"].is_null(),
        "no session to divide by"
    );
}

/// A pipeline's executor demand is the sum over its own services, matched by
/// `serviceId`: a service that started inside the window counts from zero,
/// and another pipeline's services are not charged to it.
#[test]
fn pipeline_window_sums_its_own_services_by_service_id() {
    let service = |id: u64, pipeline: &str, polls: u64, cpu: u64, max: u64, over4: u64| {
        json!({"serviceId": id, "class": "hlsFmp4", "pipelineId": pipeline, "stage": "s",
               "polls": polls, "busyUs": cpu, "cpuUs": cpu, "maxCpuUs": max, "maxPollUs": max,
               "pollsOverCpu1Ms": over4, "pollsOverCpu4Ms": over4, "pollsOverCpu16Ms": 0})
    };
    let start = executor(
        json!([]),
        json!([
            service(1, "p1", 100, 1_000, 50, 0),
            service(3, "p2", 10, 10, 5, 0)
        ]),
    );
    let end = executor(
        json!([]),
        json!([
            service(1, "p1", 300, 5_000, 60, 1),
            service(2, "p1", 100, 1_000, 70, 0),
            service(3, "p2", 900, 9_000, 9_999, 3),
        ]),
    );
    let p1 = pipeline_poll_window(&start, &end, "p1");
    assert_eq!(p1["services"], 2);
    assert_eq!(
        p1["polls"], 300,
        "200 from service 1, 100 from new service 2"
    );
    assert_eq!(p1["cpuUs"], 5_000);
    assert_eq!(p1["cpuUsPerPoll"], 16.7);
    assert_eq!(p1["maxCpuUs"], 70);
    assert_eq!(p1["pollsOverCpu4Ms"], 1);
    assert_eq!(p1["classes"], json!(["hlsFmp4"]));
    let none = pipeline_poll_window(&start, &end, "p3");
    assert_eq!(none["polls"], 0);
    assert!(none["cpuUsPerPoll"].is_null());
}

/// The pull arm's comparison is the base outputs' delivery inside the
/// window, so it subtracts the baseline: lifetime totals would credit a
/// stalled window with its readiness media.
#[test]
fn window_delivery_counts_only_media_inside_the_window() {
    let baseline = json!({"videoCount": 100, "audioCount": 80, "mediaProgressMs": 4_000});
    let end = json!({"videoCount": 3_700, "audioCount": 2_900, "mediaProgressMs": 64_000});
    let window = window_delivery(&baseline, &end, 60.0);
    assert_eq!(window["video"], 3_600);
    assert_eq!(window["audio"], 2_820);
    assert_eq!(window["mediaProgressMs"], 60_000);
    assert_eq!(window["mediaRatio"], 1.0);
    let stalled = window_delivery(&end, &end, 60.0);
    assert_eq!(stalled["mediaRatio"], 0.0);
    assert!(window_delivery(&baseline, &end, 0.0)["mediaRatio"].is_null());
}

#[test]
fn mean_cpu_gate_rejects_missing_empty_and_reset_measurements() {
    let start = executor(json!([]), json!([]));
    for rows in [
        json!([]),
        json!([{"serviceId": 1, "pipelineId": "p", "polls": 1}]),
        json!([{"serviceId": 1, "pipelineId": "p", "cpuUs": 10}]),
        json!([{"serviceId": 1, "pipelineId": "p", "polls": 0, "cpuUs": 0}]),
    ] {
        let end = executor(json!([]), rows);
        let window = pipeline_poll_window(&start, &end, "p");
        assert!(check_mean_poll_cpu(&window, 250.0).is_err(), "{window}");
    }
    let start = executor(
        json!([]),
        json!([
            {"serviceId": 1, "pipelineId": "p", "polls": 20, "cpuUs": 1000}
        ]),
    );
    let end = executor(
        json!([]),
        json!([
            {"serviceId": 1, "pipelineId": "p", "polls": 30, "cpuUs": 100}
        ]),
    );
    let window = pipeline_poll_window(&start, &end, "p");
    assert!(check_mean_poll_cpu(&window, 250.0).is_err(), "{window}");
    assert!(check_mean_poll_cpu(&pipeline_poll_window(&Value::Null, &end, "p"), 250.0).is_err());
}

#[test]
fn mean_cpu_gate_checks_the_measured_threshold() {
    let start = executor(json!([]), json!([]));
    for (cpu, passes) in [(500, true), (501, false)] {
        let end = executor(
            json!([]),
            json!([
                {"serviceId": 1, "pipelineId": "p", "polls": 2, "cpuUs": cpu}
            ]),
        );
        let window = pipeline_poll_window(&start, &end, "p");
        assert_eq!(
            check_mean_poll_cpu(&window, 250.0).is_ok(),
            passes,
            "{window}"
        );
    }
}
