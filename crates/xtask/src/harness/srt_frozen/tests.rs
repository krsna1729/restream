use super::sampler::{Probe, Sampler, listening_inodes, log_http_port, parse_response};
use super::*;

fn sample(t: f64) -> Value {
    json!({"t": t, "proc": {"rss_kb": 100000, "pss_kb": 90000, "threads": 20, "fds": 40,
                            "cpu_jiffies": (t * 50.0) as i64}})
}

fn errors(samples: &[Value], attribution: bool) -> Vec<Value> {
    frozen_sample_errors(&samples.iter().collect::<Vec<_>>(), attribution)
}

#[test]
fn missing_or_zero_rss_is_not_a_valid_measurement() {
    assert!(!errors(&[], false).is_empty());
    for proc in [json!({}), json!({"rss_kb": 0})] {
        assert!(!errors(&[json!({"t": 1, "proc": proc})], false).is_empty());
    }
    assert!(errors(&[json!({"t": 1, "proc": {"rss_kb": 123}})], false).is_empty());
}

#[test]
fn requested_attribution_cannot_silently_lose_evidence() {
    let mut sample = json!({"t": 1, "proc": {"rss_kb": 123}});
    assert!(!errors(&[sample.clone()], true).is_empty());
    sample["attribution"] = json!({"smaps": {}, "smaps_rollup": {}, "threads": {},
        "socket_fds": {}, "udp": {}, "udp6": {}, "telemetry": {}});
    assert!(!errors(&[sample.clone()], true).is_empty());
    sample["attribution"] = json!({
        "smaps": "Rss: 123 kB", "smaps_rollup": "Rss: 123 kB",
        "threads": {"1": "restream"}, "socket_fds": {},
        "udp": "header", "udp6": "header",
        "telemetry": {"memoryAccounting": {"retainedPayloadBytes": 0}},
    });
    assert!(errors(&[sample.clone()], true).is_empty());
    sample["attribution"]["smaps_error"] = json!("permission denied");
    assert!(!errors(&[sample], true).is_empty());
}

struct Run {
    report: Value,
    samples: Vec<Value>,
    phases: Vec<Value>,
    sampler: SamplerOutcome,
    harness_exit: Option<i32>,
    processes_after: Vec<String>,
}

impl Run {
    fn new() -> Self {
        let report = json!({
            "mode": "fault.srt-output-stall", "passed": true,
            "tests": [{
                "test": FROZEN_TEST, "passed": true,
                "sigstopOk": true, "enteredRetryCycle": true,
                "rssBeforeStallKb": 100000, "rssAfterStallKb": 105000, "rssGrowthKb": 5000,
                "rssMeasurement": {"available": true, "bounded": true,
                                   "beforeKb": 100000, "afterKb": 105000, "growthKb": 5000,
                                   "limitKb": 65536, "beforeError": null, "afterError": null},
            }],
        });
        let phases = FROZEN_PHASES
            .iter()
            .zip([2, 10, 15, 135, 138, 141])
            .map(|(phase, t)| json!({"phase": phase, "t": t}))
            .collect();
        Self {
            report,
            samples: (0..142).map(|t| sample(f64::from(t))).collect(),
            phases,
            sampler: SamplerOutcome {
                process_gone_at: Some(142.0),
                finished_at: Some(143.0),
                failure: None,
                stop_requested_at: 150.0,
                joined: true,
            },
            harness_exit: Some(0),
            processes_after: Vec::new(),
        }
    }

    fn summarize(&self) -> Map<String, Value> {
        summarize_frozen_run(
            &self.report,
            &self.samples,
            &self.phases,
            &self.sampler,
            false,
            self.harness_exit,
            &self.processes_after,
        )
    }

    fn passed(&self) -> bool {
        self.summarize()["passed"] == json!(true)
    }

    fn with_report(edit: impl FnOnce(&mut Value)) -> bool {
        let mut run = Self::new();
        edit(&mut run.report);
        run.passed()
    }

    fn with_frozen(edit: impl FnOnce(&mut Map<String, Value>)) -> bool {
        Self::with_report(|report| edit(report["tests"][0].as_object_mut().unwrap()))
    }
}

#[test]
fn corrected_harness_report_passes_and_projects_the_real_endpoint_keys() {
    let mut run = Run::new();
    let result = run.summarize();
    assert_eq!(result["passed"], json!(true), "{result:?}");
    assert_eq!(result["gate_rssBeforeKb"], json!(100000));
    assert_eq!(result["gate_rssAfterKb"], json!(105000));
    run.harness_exit = Some(1);
    assert!(!run.passed());
    run.harness_exit = None;
    assert!(!run.passed());
    run.harness_exit = Some(0);
    run.processes_after = vec!["owned task process".into()];
    assert!(!run.passed());
}

#[test]
fn missing_or_failed_named_artifact_cannot_pass_on_exit_zero() {
    for report in [
        Value::Null,
        json!({}),
        json!({"mode": "fault.srt-output-stall", "passed": true, "tests": []}),
        json!({"loadError": "No such file or directory"}),
    ] {
        assert!(!Run::with_report(|r| *r = report.clone()), "{report}");
    }
    assert!(!Run::with_report(|r| r["passed"] = json!(false)));
    assert!(!Run::with_frozen(|f| {
        f.insert("passed".into(), json!(false));
    }));
    assert!(!Run::with_report(|r| {
        let duplicate = r["tests"][0].clone();
        r["tests"].as_array_mut().unwrap().push(duplicate);
    }));
    for flag in ["sigstopOk", "enteredRetryCycle"] {
        assert!(!Run::with_frozen(|f| {
            f.insert(flag.into(), json!(false));
        }));
    }
    assert!(!Run::with_frozen(|f| {
        f.insert("test".into(), json!("different-subtest"));
    }));
}

#[test]
fn null_zero_missing_or_legacy_endpoints_are_rejected() {
    for key in ["rssBeforeStallKb", "rssAfterStallKb"] {
        for value in [
            Value::Null,
            json!(0),
            json!(-1),
            json!(true),
            json!("100000"),
            json!(100000.0),
        ] {
            assert!(
                !Run::with_frozen(|f| {
                    f.insert(key.into(), value.clone());
                }),
                "{key}={value}"
            );
        }
        assert!(!Run::with_frozen(|f| {
            f.remove(key);
        }));
    }
    assert!(!Run::with_frozen(|f| {
        let before = f.remove("rssBeforeStallKb").unwrap();
        let after = f.remove("rssAfterStallKb").unwrap();
        f.insert("rssBeforeKb".into(), before);
        f.insert("rssAfterKb".into(), after);
    }));
}

#[test]
fn measurement_must_be_available_bounded_and_consistent() {
    for (key, value) in [
        ("available", Value::Null),
        ("available", json!(false)),
        ("bounded", json!(false)),
        ("beforeKb", json!(0)),
        ("afterKb", Value::Null),
        ("growthKb", json!(1)),
        ("limitKb", json!(100000)),
        ("beforeError", json!("unreadable")),
    ] {
        assert!(
            !Run::with_frozen(|f| f["rssMeasurement"][key] = value.clone()),
            "{key}={value}"
        );
    }
    for measurement in [Value::Null, json!({})] {
        assert!(!Run::with_frozen(|f| {
            f.insert("rssMeasurement".into(), measurement.clone());
        }));
    }
}

#[test]
fn each_phase_and_the_full_retry_window_must_be_captured() {
    for missing in FROZEN_PHASES {
        let mut run = Run::new();
        run.phases.retain(|phase| phase["phase"] != missing);
        assert!(!run.passed(), "{missing}");
    }
    let mut run = Run::new();
    run.phases[3]["t"] = json!(134);
    assert!(!run.passed());
    let mut run = Run::new();
    run.phases[1]["t"] = json!(1);
    assert!(!run.passed());
    let mut run = Run::new();
    run.phases.reverse();
    assert!(!run.passed());
}

#[test]
fn late_discovery_and_truncated_capture_do_not_pass() {
    let all = Run::new().samples;
    let at = |sample: &Value| sample["t"].as_f64().unwrap();
    let cases: [Vec<Value>; 5] = [
        all[all.len() - 1..].to_vec(),
        all[10..].to_vec(),
        all[..100].to_vec(),
        all.iter()
            .filter(|s| !(45.0..=50.0).contains(&at(s)))
            .cloned()
            .collect(),
        all.iter()
            .filter(|s| !(7.0..=13.0).contains(&at(s)))
            .cloned()
            .collect(),
    ];
    for samples in cases {
        let count = samples.len();
        let mut run = Run::new();
        run.samples = samples;
        assert!(!run.passed(), "sample_count={count}");
    }
}

#[test]
fn sampler_must_finish_and_join_after_capture() {
    let edits: [fn(&mut SamplerOutcome); 5] = [
        |s| s.joined = false,
        |s| s.finished_at = None,
        |s| s.finished_at = Some(140.0),
        |s| s.stop_requested_at = 140.0,
        |s| s.failure = Some("sampler crashed".into()),
    ];
    for (index, edit) in edits.into_iter().enumerate() {
        let mut run = Run::new();
        edit(&mut run.sampler);
        assert!(!run.passed(), "case {index}");
    }
}

#[test]
fn process_disappearance_before_intentional_stop_fails() {
    let mut run = Run::new();
    run.sampler.process_gone_at = Some(140.0);
    let result = run.summarize();
    assert_eq!(result["passed"], json!(false), "{result:?}");
    assert!(
        result["coverage_errors"]
            .to_string()
            .contains("disappeared")
    );
}

#[test]
fn expected_shutdown_after_capture_is_not_a_measurement_failure() {
    let mut run = Run::new();
    run.samples
        .push(json!({"t": 142, "error": "process exited"}));
    assert!(run.passed());
    run.sampler.failure = Some("sampler crashed".into());
    assert!(!run.passed());
}

/// The harness stops Restream right after capture-complete (141 here). A
/// sample that began before the marker but failed after it observed that
/// shutdown; one that failed before the marker is a capture failure.
#[test]
fn a_sample_failing_after_capture_complete_is_shutdown_not_a_capture_error() {
    let mut run = Run::new();
    run.samples[141]["error"] = json!("/metrics/system: truncated HTTP response");

    run.samples[141]["errorAt"] = json!(141.2);
    assert!(run.passed());

    run.samples[141]["errorAt"] = json!(140.9);
    assert!(!run.passed());

    run.samples[141].as_object_mut().unwrap().remove("errorAt");
    assert!(
        !run.passed(),
        "an error without its failure time stays a capture error"
    );
}

#[test]
fn rss_summary_reports_fifth_medians_and_ten_sample_series() {
    let mut samples: Vec<Value> = (0..10).map(|t| sample(f64::from(t))).collect();
    for (index, sample) in samples.iter_mut().enumerate() {
        sample["proc"]["rss_kb"] = json!(1024 * (index as i64 + 1));
    }
    let summary = rss_summary(&samples);
    assert_eq!(
        summary["rss_mb_median_by_fifth"],
        json!([1.5, 3.5, 5.5, 7.5, 9.5])
    );
    assert_eq!(summary["rss_mb_max"], json!(10));
    assert_eq!(summary["rss_series_mb_every_10s"], json!([[0, 1, 20, 40]]));
    assert_eq!(rss_summary(&[])["rss_mb_max"], Value::Null);
}

struct GoneProcess;

impl Probe for GoneProcess {
    fn discover(&mut self) -> bool {
        true
    }
    fn record(&mut self, _: &mut Map<String, Value>) -> Result<(), String> {
        Err("process exited".into())
    }
    fn process_exists(&self) -> bool {
        false
    }
}

#[test]
fn sampler_preserves_process_disappearance_in_its_outcome() {
    let dir = workflow::TempDir::new("srt-frozen-sampler").unwrap();
    let sampler = Sampler::start(
        GoneProcess,
        dir.path().join("samples.jsonl"),
        SAMPLE_INTERVAL,
    )
    .unwrap();
    // The sampler ends by itself before any stop is requested.
    let until = std::time::Instant::now() + Duration::from_secs(5);
    while !sampler.is_finished() && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    let outcome = sampler.finish(now(), SAMPLER_JOIN);
    assert!(outcome.process_gone_at.is_some());
    assert!(outcome.finished_at.is_some());
    assert_eq!(outcome.failure, None);
    assert!(outcome.joined);
}

#[test]
fn timeout_kills_the_wrapper_and_its_signal_resistant_and_stopped_children() {
    let dir = workflow::TempDir::new("srt-frozen-timeout").unwrap();
    let pids = dir.path().join("pids");
    // A TERM-ignoring child, and a SIGSTOPped one like the frozen MediaMTX
    // destination: a stopped process never acts on SIGTERM, only SIGKILL.
    let script = format!(
        "(trap '' TERM; echo ready; exec sleep 60) & r=$!; sleep 60 & s=$!; kill -STOP $s; echo $$ $r $s > '{}'; wait",
        pids.display()
    );
    let (stdout, stderr) = (dir.path().join("out"), dir.path().join("err"));
    let result = run_harness(
        Command::new("sh").args(["-c", &script]),
        &stdout,
        &stderr,
        Duration::from_secs(1),
        Duration::from_millis(200),
    );
    let recorded = fs::read_to_string(&pids).unwrap_or_default();
    let recorded: Vec<i32> = recorded
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect();
    // SIGKILL delivery to the reparented child is asynchronous.
    let alive = || -> Vec<i32> {
        recorded
            .iter()
            .copied()
            .filter(|pid| {
                fs::read_to_string(format!("/proc/{pid}/status"))
                    .is_ok_and(|status| !status.contains("State:\tZ"))
            })
            .collect()
    };
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !alive().is_empty() && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    let alive = alive();
    if let Some(leader) = recorded.first() {
        // SAFETY: SIGKILL only to the group this test spawned (cleanup).
        unsafe { libc::kill(-leader, libc::SIGKILL) };
    }
    let run = result.unwrap();
    assert!(run.timed_out);
    assert_ne!(run.returncode, Some(0));
    assert!(fs::read_to_string(&stdout).unwrap().contains("ready"));
    assert_eq!(recorded.len(), 3);
    assert!(alive.is_empty(), "survivors: {alive:?}");
}

#[test]
fn engine_discovery_reads_the_log_port_and_listening_socket() {
    assert_eq!(
        log_http_port("\x1b[2mINFO\x1b[0m listening \x1b[3mhttp_port\x1b[0m=18080 rtmp_port=1935"),
        Some(18080)
    );
    assert_eq!(log_http_port("no port yet"), None);
    let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:46A0 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0 100 0 0 10 0
   1: 0100007F:46A0 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 4343 1 0 20 4 30 10 -1
   2: 0100007F:0050 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4444 1 0 100 0 0 10 0";
    assert_eq!(listening_inodes(table, 18080), vec![4242]);
}

#[test]
fn loopback_http_keeps_the_session_cookie_and_decodes_chunks() {
    let login = parse_response(
        b"HTTP/1.1 200 OK\r\nset-cookie: restream_session=abc; HttpOnly; Path=/\r\ncontent-length: 2\r\n\r\n{}",
    )
    .unwrap();
    assert_eq!(login.cookie.as_deref(), Some("restream_session=abc"));
    assert_eq!(login.body, b"{}");
    let chunked = parse_response(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n",
    )
    .unwrap();
    assert_eq!(chunked.body, b"{\"a\":1}");
    assert!(parse_response(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n").is_err());
    assert!(parse_response(b"HTTP/1.1 303 See Other\r\nlocation: /login\r\n\r\n").is_err());
}
