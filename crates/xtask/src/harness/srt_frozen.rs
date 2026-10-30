//! `cargo xtask srt-frozen`: one `fault.srt-output-stall` run (SIGSTOPped SRT
//! destination) with a 1 s process sampler, judged from the harness artifact,
//! phase markers and sample coverage (Q-026 RSS attribution).

use std::env;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::workflow::{self, Error, WorkflowResult};

mod sampler;
#[cfg(test)]
mod tests;

use sampler::{Engine, Sampler, SamplerOutcome};

const USAGE: &str =
    "usage: cargo xtask srt-frozen --out <dir> --tag <tag> --variant baseline|candidate
                              [--restream-bin <path>] [--harness-bin <path>] [--attribution]

Runs `cargo xtask harness fault.srt-output-stall -- --no-netns` once (900 s
limit) with a 1 s RSS/thread/fd sampler of the Restream process, then writes
<out>/<tag>.json. Work directory: .local/artifacts/srt-final/<tag> (must be new;
holds samples.jsonl, harness.stdout, harness.stderr).
Binaries default to target/qual-release/{restream,test_harness}
(cargo xtask build-release). --variant is a report label only.
--attribution also samples smaps, thread/socket census and engine telemetry.
RESTREAM_INITIAL_ADMIN_PASSWORD is passed through, else a random one is used.

Exit: 0 pass, 1 fail, 2 usage, 3 stray restream/mediamtx/ffmpeg/test_harness.";

const FROZEN_TEST: &str = "srt-egress-bounded-rss-under-frozen-destination";
const FROZEN_PHASES: [&str; 6] = [
    "live-progress",
    "sigstop",
    "retry-observed",
    "output-stopped",
    "pipeline-deleted",
    "capture-complete",
];
const LIMIT_KB: i64 = 64 * 1024;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const HARNESS_TIMEOUT: Duration = Duration::from_secs(900);
const TERMINATE_GRACE: Duration = Duration::from_secs(5);
const SAMPLER_JOIN: Duration = Duration::from_secs(15);
const STRAY_NAMES: [&str; 4] = ["restream", "mediamtx", "ffmpeg", "test_harness"];

struct Options {
    out: String,
    tag: String,
    variant: String,
    restream_bin: Option<String>,
    harness_bin: Option<String>,
    attribution: bool,
}

/// `None` when help was printed.
fn parse(args: &[&str]) -> Result<Option<Options>, Error> {
    let usage = |message: String| Error::usage(format!("srt-frozen: {message}\n\n{USAGE}"));
    let (mut out, mut tag, mut variant) = (None, None, None);
    let (mut restream_bin, mut harness_bin, mut attribution) = (None, None, false);
    let mut index = 0;
    while index < args.len() {
        let arg = args[index];
        index += 1;
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (arg, None),
        };
        let mut value = || -> Result<String, Error> {
            match inline {
                Some(value) => Ok(value.to_owned()),
                None => {
                    let value = args
                        .get(index)
                        .ok_or_else(|| usage(format!("{flag} needs a value")))?;
                    index += 1;
                    Ok((*value).to_owned())
                }
            }
        };
        match flag {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "--out" => out = Some(value()?),
            "--tag" => tag = Some(value()?),
            "--variant" => variant = Some(value()?),
            "--restream-bin" => restream_bin = Some(value()?),
            "--harness-bin" => harness_bin = Some(value()?),
            "--attribution" if inline.is_none() => attribution = true,
            _ => return Err(usage(format!("unexpected argument '{arg}'"))),
        }
    }
    let required = |value: Option<String>, flag: &str| {
        value.ok_or_else(|| usage(format!("{flag} is required")))
    };
    let variant = required(variant, "--variant")?;
    if !matches!(variant.as_str(), "baseline" | "candidate") {
        return Err(usage(format!(
            "--variant must be baseline or candidate, not '{variant}'"
        )));
    }
    Ok(Some(Options {
        out: required(out, "--out")?,
        tag: required(tag, "--tag")?,
        variant,
        restream_bin,
        harness_bin,
        attribution,
    }))
}

pub(crate) fn srt_frozen(args: &[&str]) -> WorkflowResult {
    let Some(options) = parse(args)? else {
        return Ok(());
    };
    let root = crate::repo_root();
    let out = crate::user_path(&options.out);
    fs::create_dir_all(&out)?;
    let strays = stray_processes();
    if !strays.is_empty() {
        return Err(Error::exit(
            3,
            format!("ABORT: stray processes: {strays:?}"),
        ));
    }
    let work = root.join(".local/artifacts/srt-final").join(&options.tag);
    if work.exists() {
        return Err(format!(
            "refusing to overwrite existing campaign: {}",
            work.display()
        )
        .into());
    }
    let stall_dir = work.join("fault.srt-output-stall");
    fs::create_dir_all(&stall_dir)?;
    let restream = binary(options.restream_bin.as_deref(), &root, "restream");
    let harness = binary(options.harness_bin.as_deref(), &root, "test_harness");
    // The sampler logs in with the same credential the harness hands Restream;
    // it is never printed.
    let password = admin_password()?;

    if !crate::workflow::executable(&harness) {
        return Err(format!("missing harness binary at {}", harness.display()).into());
    }
    let probe = Engine::new(
        stall_dir.join("restream-frozen-destination.log"),
        password.clone(),
        options.attribution,
    );
    let sampler = Sampler::start(probe, work.join("samples.jsonl"), SAMPLE_INTERVAL)?;
    // Launched directly (no intermediate xtask supervisor): `run_harness`
    // owns one process group holding test_harness and every media child, so a
    // timeout or interrupt reaches the SIGSTOPped destination too.
    let mut command =
        super::harness_command(&root, &harness, "fault.srt-output-stall", &["--no-netns"])?;
    command
        .env("RESTREAM_BIN", &restream)
        .env("HARNESS_BIN", &harness)
        .env("BENCH_BUILD", "never")
        .env("WORK_DIR", &work)
        .env("RESTREAM_INITIAL_ADMIN_PASSWORD", &password)
        .stdin(Stdio::null());
    let run = run_harness(
        &mut command,
        &work.join("harness.stdout"),
        &work.join("harness.stderr"),
        HARNESS_TIMEOUT,
        TERMINATE_GRACE,
    );
    let stop_requested_at = now();
    let outcome = sampler.finish(stop_requested_at, SAMPLER_JOIN);
    let run = run?;
    let processes_after = stray_processes();

    let samples_path = work.join("samples.jsonl");
    let samples = if samples_path.exists() {
        load_jsonl(&samples_path)?
    } else {
        Vec::new()
    };
    let report = fs::read_to_string(stall_dir.join("fault.srt-output-stall.json"))
        .map_err(|error| error.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|error| error.to_string()))
        .unwrap_or_else(|error| json!({ "loadError": error }));
    let phases_path = stall_dir.join("frozen-phases.jsonl");
    let phases = if phases_path.exists() {
        load_jsonl(&phases_path)?
    } else {
        Vec::new()
    };

    let mut result = rss_summary(&samples);
    result.insert("variant".into(), json!(options.variant));
    result.insert("tag".into(), json!(options.tag));
    result.extend(summarize_frozen_run(
        &report,
        &samples,
        &phases,
        &outcome,
        options.attribution,
        run.returncode,
        &processes_after,
    ));
    result.insert("harness_timed_out".into(), json!(run.timed_out));
    if run.timed_out {
        result.insert("passed".into(), json!(false));
    }
    result.insert("attribution_requested".into(), json!(options.attribution));
    result.insert(
        "binaries".into(),
        json!({
            "restream": restream.to_string_lossy(),
            "harness": harness.to_string_lossy(),
        }),
    );
    let passed = result["passed"] == json!(true);
    let result_path = out.join(format!("{}.json", options.tag));
    let pretty = |value: &Map<String, Value>| {
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())
    };
    fs::write(&result_path, pretty(&result)? + "\n")?;
    result.remove("rss_series_mb_every_10s");
    println!("{}", pretty(&result)?);
    if passed {
        Ok(())
    } else {
        Err(Error::exit(
            1,
            format!("srt-frozen: {} did not pass", result_path.display()),
        ))
    }
}

/// An explicit binary resolved like the caller's shell sees it, else the
/// release default.
fn binary(explicit: Option<&str>, root: &Path, name: &str) -> PathBuf {
    match explicit {
        Some(path) => {
            let path = crate::user_path(path);
            fs::canonicalize(&path).unwrap_or(path)
        }
        None => root.join("target/qual-release").join(name),
    }
}

fn admin_password() -> Result<String, Error> {
    if let Ok(password) = env::var("RESTREAM_INITIAL_ADMIN_PASSWORD") {
        return Ok(password);
    }
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut password = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(password, "{byte:02x}");
    }
    Ok(password)
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// `pgrep -a -x` for each media/harness name: `"<pid> <cmdline>"`.
fn stray_processes() -> Vec<String> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(comm) = fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        let comm = comm.trim_end_matches('\n');
        let Some(rank) = STRAY_NAMES.iter().position(|name| *name == comm) else {
            continue;
        };
        let cmdline = fs::read(entry.path().join("cmdline")).unwrap_or_default();
        let args: Vec<_> = cmdline
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(String::from_utf8_lossy)
            .collect();
        let shown = if args.is_empty() {
            comm.to_owned()
        } else {
            args.join(" ")
        };
        found.push((rank, pid, format!("{pid} {shown}")));
    }
    found.sort();
    found.into_iter().map(|(_, _, line)| line).collect()
}

struct HarnessRun {
    /// Exit code, `-signal` when killed, `None` when it never started.
    returncode: Option<i32>,
    timed_out: bool,
}

/// Runs `command` in an owned group with its streams in two files; after
/// `limit` the group gets SIGTERM, then SIGKILL after `grace`.
fn run_harness(
    command: &mut Command,
    stdout: &Path,
    stderr: &Path,
    limit: Duration,
    grace: Duration,
) -> Result<HarnessRun, Error> {
    use std::os::unix::process::ExitStatusExt;
    command
        .stdout(File::create(stdout)?)
        .stderr(File::create(stderr)?);
    let mut child = match workflow::spawn(command) {
        Ok(child) => child,
        Err(error) if workflow::interrupted().is_none() => {
            fs::write(stderr, error.to_string())?;
            return Ok(HarnessRun {
                returncode: None,
                timed_out: false,
            });
        }
        Err(error) => return Err(error),
    };
    let (status, timed_out) = workflow::wait_timeout(&mut child, limit, grace)?;
    Ok(HarnessRun {
        returncode: status
            .code()
            .or_else(|| status.signal().map(|signal| -signal)),
        timed_out,
    })
}

fn load_jsonl(path: &Path) -> Result<Vec<Value>, Error> {
    let text = fs::read_to_string(path)?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .map_err(|error| format!("{}: {error}", path.display()).into())
        })
        .collect()
}

/// Python truthiness of an optional JSON value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(fields)) => !fields.is_empty(),
    }
}

/// A JSON integer (never a float or bool) above zero.
fn positive_kb(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64).filter(|kb| *kb > 0)
}

fn timestamp(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64).filter(|t| t.is_finite())
}

fn is_true(value: Option<&Value>) -> bool {
    value == Some(&Value::Bool(true))
}

fn text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn frozen_sample_errors(samples: &[&Value], attribution: bool) -> Vec<Value> {
    const REQUIRED: [&str; 7] = [
        "smaps",
        "smaps_rollup",
        "threads",
        "socket_fds",
        "udp",
        "udp6",
        "telemetry",
    ];
    let mut errors = Vec::new();
    if samples.is_empty() {
        errors.push(json!("no process samples captured"));
    }
    let empty = Map::new();
    for sample in samples {
        let t = sample.get("t");
        if let Some(error) = sample.get("error") {
            errors.push(json!({ "t": t, "error": error }));
        }
        let rss = sample.get("proc").and_then(|proc| proc.get("rss_kb"));
        if !rss.and_then(Value::as_f64).is_some_and(|kb| kb > 0.0) {
            errors.push(json!({ "t": t, "error": "RSS unavailable or zero" }));
        }
        if attribution {
            let facts = sample
                .get("attribution")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            let missing: Vec<&str> = REQUIRED
                .into_iter()
                .filter(|key| !facts.contains_key(*key))
                .collect();
            let mut failures: Map<String, Value> = facts
                .iter()
                .filter(|(key, _)| key.ends_with("_error"))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            let accounting = facts
                .get("telemetry")
                .and_then(Value::as_object)
                .is_some_and(|telemetry| telemetry.contains_key("memoryAccounting"));
            if !accounting {
                failures.insert(
                    "telemetry".into(),
                    json!("retained-ring accounting unavailable"),
                );
            }
            for key in ["smaps", "smaps_rollup", "threads", "udp", "udp6"] {
                if facts.contains_key(key) && !truthy(facts.get(key)) {
                    failures.insert(key.into(), json!("empty evidence"));
                }
            }
            if !missing.is_empty() || !failures.is_empty() {
                errors.push(json!({ "t": t, "missing": missing, "errors": failures }));
            }
        }
    }
    errors
}

/// Accepts only the current harness artifact, never an exit code or legacy keys.
fn parse_frozen_report(report: &Value) -> Map<String, Value> {
    let empty = Map::new();
    let report = report.as_object().unwrap_or(&empty);
    let mut errors: Vec<String> = Vec::new();
    if let Some(error) = report.get("loadError").filter(|e| truthy(Some(e))) {
        errors.push(format!("frozen artifact unavailable: {}", text(error)));
    }
    if report.get("mode").and_then(Value::as_str) != Some("fault.srt-output-stall")
        || !is_true(report.get("passed"))
    {
        errors.push("missing or failed fault.srt-output-stall report".into());
    }
    let matches: Vec<&Map<String, Value>> = report
        .get("tests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter(|test| test.get("test").and_then(Value::as_str) == Some(FROZEN_TEST))
        .collect();
    let frozen = match matches[..] {
        [frozen] => frozen,
        _ => &empty,
    };
    if matches.len() != 1 || !is_true(frozen.get("passed")) {
        errors.push("missing, duplicate or failed named frozen subtest".into());
    }
    if !is_true(frozen.get("sigstopOk")) || !is_true(frozen.get("enteredRetryCycle")) {
        errors.push("SIGSTOP and retry-cycle observations required".into());
    }
    let measurement = frozen
        .get("rssMeasurement")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    if !is_true(measurement.get("available")) || !is_true(measurement.get("bounded")) {
        errors.push("frozen RSS measurement unavailable or unbounded".into());
    }
    let (before, after) = (
        frozen.get("rssBeforeStallKb"),
        frozen.get("rssAfterStallKb"),
    );
    match (
        positive_kb(before),
        positive_kb(after),
        positive_kb(measurement.get("beforeKb")),
        positive_kb(measurement.get("afterKb")),
    ) {
        (Some(before), Some(after), Some(measured_before), Some(measured_after)) => {
            if measured_before != before || measured_after != after {
                errors.push("RSS measurement endpoints disagree with the frozen subtest".into());
            } else {
                let growth = (after - before).max(0);
                let limit = measurement.get("limitKb").and_then(Value::as_f64);
                if measurement.get("growthKb").and_then(Value::as_i64) != Some(growth)
                    || frozen.get("rssGrowthKb").and_then(Value::as_i64) != Some(growth)
                    || limit != Some(LIMIT_KB as f64)
                    || growth > LIMIT_KB
                {
                    errors.push("RSS growth or fixed 64 MiB bound disagrees with endpoints".into());
                }
            }
        }
        _ => errors.push("two positive rssBeforeStallKb/rssAfterStallKb endpoints required".into()),
    }
    let present = |key| measurement.get(key).is_some_and(|value| !value.is_null());
    if present("beforeError") || present("afterError") {
        errors.push("RSS endpoint read errors present".into());
    }
    let mut result = Map::new();
    result.insert("report_errors".into(), json!(errors));
    result.insert("gate_rssBeforeKb".into(), json!(before));
    result.insert("gate_rssAfterKb".into(), json!(after));
    result.insert("gate_rssGrowthKb".into(), json!(frozen.get("rssGrowthKb")));
    result.insert("gate_limit_kb".into(), json!(LIMIT_KB));
    result.insert("rss_measurement".into(), json!(measurement));
    result.insert(
        "enteredRetryCycle".into(),
        json!(frozen.get("enteredRetryCycle")),
    );
    result.insert("sigstopOk".into(), json!(frozen.get("sigstopOk")));
    result
}

fn phase_name(phase: &Value) -> Option<&str> {
    phase.get("phase").and_then(Value::as_str)
}

/// Two sampling intervals is the maximum gap, including phase boundaries.
fn frozen_capture_errors(
    samples: &[Value],
    phases: &[Value],
    sampler: &SamplerOutcome,
) -> Vec<String> {
    let mut errors = Vec::new();
    let mut markers = Vec::with_capacity(FROZEN_PHASES.len());
    for name in FROZEN_PHASES {
        let found: Vec<Option<&Value>> = phases
            .iter()
            .filter(|phase| phase_name(phase) == Some(name))
            .map(|phase| phase.get("t"))
            .collect();
        match found[..] {
            [t] if timestamp(t).is_some() => markers.push((name, timestamp(t).unwrap_or(0.0))),
            _ => errors.push(format!("missing, duplicate or invalid phase: {name}")),
        }
    }
    if markers.len() != FROZEN_PHASES.len() {
        return errors;
    }
    let order: Vec<&str> = phases
        .iter()
        .filter_map(phase_name)
        .filter(|name| FROZEN_PHASES.contains(name))
        .collect();
    if order != FROZEN_PHASES {
        errors.push("frozen phase records are out of order".into());
    }
    let marks: Vec<f64> = markers.iter().map(|(_, t)| *t).collect();
    if marks.windows(2).any(|pair| pair[0] >= pair[1]) {
        errors.push("frozen phases are not strictly ordered".into());
    }
    if marks[3] - marks[2] < 120.0 {
        errors.push("retry observation window shorter than 120 seconds".into());
    }
    let times: Option<Vec<f64>> = samples
        .iter()
        .filter(|sample| truthy(sample.get("proc")))
        .map(|sample| timestamp(sample.get("t")))
        .collect();
    let Some(times) = times.filter(|times| !times.is_empty()) else {
        errors.push("no valid timestamped process samples".into());
        return errors;
    };
    if times.windows(2).any(|pair| pair[0] >= pair[1]) {
        errors.push("process sample timestamps are not strictly ordered".into());
    }
    let max_gap = 2.0 * SAMPLE_INTERVAL.as_secs_f64();
    for (name, mark) in &markers {
        let nearest = times
            .iter()
            .map(|t| (t - mark).abs())
            .fold(f64::INFINITY, f64::min);
        if nearest > max_gap {
            errors.push(format!("no process sample within {max_gap}s of {name}"));
        }
    }
    if !times.iter().any(|t| marks[0] <= *t && *t < marks[1]) {
        errors.push("no live-progress process sample before SIGSTOP".into());
    }
    let capture: Vec<f64> = times
        .iter()
        .copied()
        .filter(|t| marks[0] - max_gap <= *t && *t <= marks[5] + max_gap)
        .collect();
    if capture.windows(2).any(|pair| pair[1] - pair[0] > max_gap) {
        errors.push(format!(
            "process sampling gap exceeds {max_gap}s during capture"
        ));
    }
    let end = marks[5];
    if sampler
        .process_gone_at
        .is_some_and(|gone| !gone.is_finite() || gone < end)
    {
        errors.push("sampled process disappeared before intentional capture completion".into());
    }
    let finished_ok = sampler
        .finished_at
        .is_some_and(|finished| finished.is_finite() && finished >= end);
    let stopped = sampler.stop_requested_at;
    if !sampler.joined
        || sampler.failure.is_some()
        || !finished_ok
        || !stopped.is_finite()
        || stopped < end
    {
        errors.push("sampler failed or stopped before capture completed".into());
    }
    errors
}

fn summarize_frozen_run(
    report: &Value,
    samples: &[Value],
    phases: &[Value],
    sampler: &SamplerOutcome,
    attribution: bool,
    harness_exit: Option<i32>,
    processes_after: &[String],
) -> Map<String, Value> {
    let mut result = parse_frozen_report(report);
    // Errors from an expected process shutdown after this marker are not
    // capture failures.
    let complete: Vec<Option<&Value>> = phases
        .iter()
        .filter(|phase| phase_name(phase) == Some("capture-complete"))
        .map(|phase| phase.get("t"))
        .collect();
    let end = match complete[..] {
        [t] => timestamp(t).unwrap_or(f64::INFINITY),
        _ => f64::INFINITY,
    };
    // A sample that began before the marker but failed after it hit the
    // intentional engine shutdown that follows capture-complete.
    let capture: Vec<&Value> = samples
        .iter()
        .filter(|sample| timestamp(sample.get("t")).is_some_and(|t| t <= end))
        .filter(|sample| {
            sample.get("error").is_none()
                || !timestamp(sample.get("errorAt")).is_some_and(|at| at > end)
        })
        .collect();
    let sample_errors = frozen_sample_errors(&capture, attribution);
    let coverage_errors = frozen_capture_errors(samples, phases, sampler);
    let passed = harness_exit == Some(0)
        && result["report_errors"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && sample_errors.is_empty()
        && coverage_errors.is_empty()
        && processes_after.is_empty();
    result.insert("sample_count".into(), json!(samples.len()));
    result.insert("sample_errors".into(), json!(sample_errors));
    result.insert("coverage_errors".into(), json!(coverage_errors));
    result.insert("sampler".into(), sampler.to_json());
    result.insert("phases".into(), json!(phases));
    result.insert("harness_exit".into(), json!(harness_exit));
    result.insert("processes_after".into(), json!(processes_after));
    result.insert("passed".into(), json!(passed));
    result
}

/// `statistics.median`: the middle value, or the mean of the middle two.
fn median(values: &[i64]) -> Value {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    match sorted.len() {
        0 => Value::Null,
        len if len % 2 == 1 => json!(sorted[middle]),
        _ => json!((sorted[middle - 1] + sorted[middle]) as f64 / 2.0),
    }
}

/// RSS (MiB), thread and fd series over every process sample: medians by
/// fifth of the run, maxima and one row every 10 samples.
fn rss_summary(samples: &[Value]) -> Map<String, Value> {
    let start = samples
        .first()
        .and_then(|sample| timestamp(sample.get("t")));
    let series: Vec<[i64; 4]> = samples
        .iter()
        .filter(|sample| truthy(sample.get("proc")))
        .filter_map(|sample| {
            let proc = &sample["proc"];
            let offset = timestamp(sample.get("t"))? - start?;
            Some([
                offset.round_ties_even() as i64,
                proc["rss_kb"].as_i64()? / 1024,
                proc["threads"].as_i64()?,
                proc["fds"].as_i64()?,
            ])
        })
        .collect();
    let rss: Vec<i64> = series.iter().map(|row| row[1]).collect();
    let n = rss.len();
    let fifths: Vec<Value> = (0..5)
        .map(|i| median(&rss[n * i / 5..n * (i + 1) / 5]))
        .collect();
    let column_max = |column: usize| series.iter().map(|row| row[column]).max();
    let every_10s: Vec<&[i64; 4]> = series.iter().step_by(10).collect();
    let mut summary = Map::new();
    summary.insert("rss_mb_median_by_fifth".into(), json!(fifths));
    summary.insert("rss_mb_max".into(), json!(column_max(1)));
    summary.insert("threads_max".into(), json!(column_max(2)));
    summary.insert("fds_max".into(), json!(column_max(3)));
    summary.insert("rss_series_mb_every_10s".into(), json!(every_10s));
    summary
}
