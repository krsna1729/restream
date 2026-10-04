//! `cargo xtask capacity-ramp`: egress capacity ramp. RTMP, RTMPS, SRT, HLS
//! PUT and transcode fan-out from one ingest into the harness's in-process
//! sinks (`MSR_PEER=sink`), with Restream and the receiver on disjoint CPU
//! sets. Per rung it records receiver delivery (floor 0.95; HLS PUT per
//! segment), Jain fairness, Restream's own delivery view, CPU and RSS, and
//! reports the highest rung where every repeat delivered to every
//! destination. `docs/capacity-ramp.md` describes the method.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use crate::bins::{self, git, utc_timestamp};
use crate::{StepResult, capture_into};

const USAGE: &str = "usage: cargo xtask capacity-ramp

Environment (all optional):
  CAPACITY_PROTOCOLS       comma list of rtmp,rtmps,srt,hls,transcode
                           (default rtmp,rtmps,srt,hls)
  CAPACITY_RTMP_OUTPUTS    RTMP ladder   (default 100,250,500,1000)
  CAPACITY_RTMPS_OUTPUTS   RTMPS ladder  (default 100,250,500,1000)
  CAPACITY_SRT_OUTPUTS     SRT ladder    (default 50,100,150,200,300,500,1000)
  CAPACITY_HLS_OUTPUTS     HLS PUT ladder (default 50,100,250,500,1000); an output
                           passes when it received every segment of the window,
                           each within 3 s of the first output to receive it
  CAPACITY_TRANSCODE_OUTPUTS transcode ladder, FFmpeg renditions fanned out (default 10,25,50,100)
  CAPACITY_MALLOC_ARENA_MAX  Restream's RESTREAM_MALLOC_ARENA_MAX for the run: `default`
                           (glibc policy) or a count; unset = Restream's provisional default
  CAPACITY_BITRATE         publisher fixture bitrate label (default 8M)
  CAPACITY_WINDOW_SECS     rated window per rung (default 30)
  CAPACITY_SETTLE_SECS     settle before the window (default 10)
  CAPACITY_REPEATS         repeats per rung (default 3)
  CAPACITY_RESTREAM_CPUS   cpuset for Restream (default: first half of online CPUs)
  CAPACITY_HARNESS_CPUS    cpuset for harness, publisher and sinks (default: the rest)
  CAPACITY_EGRESS_SHARDS   RESTREAM_EGRESS_SHARDS override (default: product default,
                           CPU-derived and clamped 2..=8)
  CAPACITY_SINK_THREADS    SRT sink thread budget (default: CPUs in the harness set)
  CAPACITY_PEER_COUNT      sink instances / ports (default: the SRT sink thread count, so
                           each sink thread owns one port; a single port funnels a shard's
                           SRT flows onto one SO_REUSEPORT socket)
  CAPACITY_STOP_AFTER_FAIL stop a protocol's ladder after a rung where no repeat
                           passed (default 1)
  CAPACITY_ARTIFACT_ROOT   output root (default .local/artifacts/capacity-ramp/<utc stamp>)
  CAPACITY_SKIP_BUILD      1 to reuse the existing binaries
  CAPACITY_JITTER_SECS     seconds of host-jitter probing per Restream CPU before the
                           ramp (default 10; 0 skips it)
  CAPACITY_BUILD_PROFILE   release (default: target/qual-release via
                           `cargo xtask build-release`) or bench (inner-loop
                           target/bench via `cargo xtask build-bench`)
  CAPACITY_ALLOW_DIRTY     1 to run on a dirty worktree (recorded in provenance)

A rung passes when the receiver saw every destination at >= 0.95 of the
offered rate over the window. Results: <root>/summary.md, summary.csv,
provenance.json, and one resource-sweep directory per rung and repeat.";

const PROTOCOLS: &[(&str, &str, &str, &str)] = &[
    // (protocol, ladder variable, default ladder, resource-sweep scenario)
    (
        "rtmp",
        "CAPACITY_RTMP_OUTPUTS",
        "100,250,500,1000",
        "egress-growth-source-same",
    ),
    (
        "rtmps",
        "CAPACITY_RTMPS_OUTPUTS",
        "100,250,500,1000",
        "egress-growth-source-rtmps",
    ),
    (
        "srt",
        "CAPACITY_SRT_OUTPUTS",
        "50,100,150,200,300,500,1000",
        "egress-growth-source-srt",
    ),
    (
        "hls",
        "CAPACITY_HLS_OUTPUTS",
        "50,100,250,500,1000",
        "egress-growth-source-hls",
    ),
    (
        "transcode",
        "CAPACITY_TRANSCODE_OUTPUTS",
        "10,25,50,100",
        "egress-growth-transcode-mixed",
    ),
];

const MIN_NOFILE: u64 = 65_536;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.into())
}

fn env_number(key: &str, default: u64) -> Result<u64, String> {
    env_or(key, &default.to_string())
        .parse()
        .map_err(|_| format!("capacity-ramp: {key} must be a whole number"))
}

pub(crate) fn capacity_ramp(args: &[&str]) -> StepResult {
    match args {
        [] => {}
        ["-h" | "--help"] => {
            eprintln!("{USAGE}");
            return Ok(());
        }
        _ => return Err(USAGE.into()),
    }
    let protocols: Vec<String> = env_or("CAPACITY_PROTOCOLS", "rtmp,rtmps,srt,hls")
        .split(',')
        .map(str::to_owned)
        .collect();
    let mut ladders = BTreeMap::new();
    for (protocol, variable, default, _) in PROTOCOLS {
        ladders.insert(*protocol, env_or(variable, default));
    }
    for protocol in &protocols {
        if !ladders.contains_key(protocol.as_str()) {
            return Err(format!("capacity-ramp: unknown protocol {protocol}"));
        }
    }
    let arena_max = std::env::var("CAPACITY_MALLOC_ARENA_MAX")
        .ok()
        .filter(|v| !v.is_empty());
    let egress_shards = std::env::var("CAPACITY_EGRESS_SHARDS")
        .ok()
        .filter(|v| !v.is_empty());
    let bitrate = env_or("CAPACITY_BITRATE", "8M");
    let window_secs = env_number("CAPACITY_WINDOW_SECS", 30)?;
    let settle_secs = env_number("CAPACITY_SETTLE_SECS", 10)?;
    let repeats = env_number("CAPACITY_REPEATS", 3)?;
    let stop_after_fail = env_or("CAPACITY_STOP_AFTER_FAIL", "1") == "1";
    let root = PathBuf::from(env_or(
        "CAPACITY_ARTIFACT_ROOT",
        &format!(
            ".local/artifacts/capacity-ramp/{}",
            compact_stamp(SystemTime::now())
        ),
    ));

    // --- CPU split ---------------------------------------------------------
    let online = fs::read_to_string("/sys/devices/system/cpu/online")
        .map_err(|error| format!("capacity-ramp: cannot read online CPUs: {error}"))?;
    let cpus = expand_cpu_list(online.trim())?;
    if cpus.len() < 4 {
        return Err(format!(
            "capacity-ramp: need at least 4 online CPUs (have {})",
            cpus.len()
        ));
    }
    let half = cpus.len() / 2;
    let join = |list: &[u32]| {
        list.iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };
    let restream_cpus = env_or("CAPACITY_RESTREAM_CPUS", &join(&cpus[..half]));
    let harness_cpus = env_or("CAPACITY_HARNESS_CPUS", &join(&cpus[half..]));
    let sink_threads = env_number(
        "CAPACITY_SINK_THREADS",
        expand_cpu_list(&harness_cpus)?.len() as u64,
    )?;
    let peer_count = env_number("CAPACITY_PEER_COUNT", sink_threads)?;

    // --- Preflight ---------------------------------------------------------
    for process in ["restream", "mediamtx", "ffmpeg"] {
        if process_running(process) {
            return Err(format!(
                "capacity-ramp: a '{process}' process is running; stop it first (measurements must not share the host)"
            ));
        }
    }
    let dirty_tree = !Command::new("git")
        .args(["diff", "--quiet", "HEAD", "--"])
        .status()
        .is_ok_and(|status| status.success());
    if env_or("CAPACITY_ALLOW_DIRTY", "0") != "1" && dirty_tree {
        return Err(
            "capacity-ramp: worktree is dirty; commit, stash, or set CAPACITY_ALLOW_DIRTY=1".into(),
        );
    }
    ensure_nofile()?;
    for (key, want) in [
        ("net/core/somaxconn", 4096),
        ("net/core/rmem_max", 8_388_608),
        ("net/core/wmem_max", 8_388_608),
    ] {
        let have = sysctl_first(key).unwrap_or(0);
        if have < want {
            eprintln!(
                "capacity-ramp: warning: {}={have} (< {want} recommended)",
                key.replace('/', ".")
            );
        }
    }

    // Listener ports below the ephemeral range, so no rung's outbound sockets
    // can collide with a sink or Restream listener.
    let ephemeral_low = sysctl_first("net/ipv4/ip_local_port_range").unwrap_or(32_768);
    let port_base = if ephemeral_low > 26_000 {
        21_000
    } else {
        11_000
    };
    let mut rung_env: Vec<(String, String)> = [
        ("RESTREAM_HTTP", 0),
        ("RESTREAM_RTMP", 1),
        ("RESTREAM_SRT", 2),
        ("MTX_API", 10),
        ("MTX_HLS", 11),
        ("MTX_RTMP", 100),
        ("MTX_RTMPS", 200),
        ("MTX_SRT", 300),
        ("HLS_PUT_PORT", 400),
    ]
    .into_iter()
    .map(|(key, offset)| (key.to_owned(), (port_base + offset).to_string()))
    .collect();

    fs::create_dir_all(&root)
        .map_err(|error| format!("cannot create {}: {error}", root.display()))?;
    let build_profile = env_or("CAPACITY_BUILD_PROFILE", "release");
    let (bin_dir, build, build_command): (&str, fn() -> StepResult, &str) = match build_profile
        .as_str()
    {
        "release" => (
            "target/qual-release",
            bins::build_release,
            "cargo xtask build-release",
        ),
        "bench" => ("target/bench", bins::build_bench, "cargo xtask build-bench"),
        _ => return Err("capacity-ramp: CAPACITY_BUILD_PROFILE must be release or bench".into()),
    };
    if env_or("CAPACITY_SKIP_BUILD", "0") != "1" {
        let log_path = root.join("build.log");
        let log =
            File::create(&log_path).map_err(|error| format!("cannot create build log: {error}"))?;
        capture_into(log, build)
            .map_err(|_| format!("capacity-ramp: build failed; see {}", log_path.display()))?;
    }
    let bin_dir = std::env::current_dir()
        .map_err(|error| format!("cannot resolve working directory: {error}"))?
        .join(bin_dir);
    if !bin_dir.join("restream").is_file() || !bin_dir.join("test_harness").is_file() {
        return Err(format!(
            "capacity-ramp: {} binaries missing (run {build_command})",
            bin_dir.display()
        ));
    }

    // --- Host jitter -------------------------------------------------------
    // Gaps a CPU loses with nothing else scheduled on it (on a VM: the
    // hypervisor descheduling the vCPU, often with zero reported steal).
    let jitter_secs = env_or("CAPACITY_JITTER_SECS", "10");
    let mut host_jitter = serde_json::Map::new();
    if jitter_secs != "0" {
        for cpu in restream_cpus.split(',') {
            host_jitter.insert(cpu.to_owned(), pinned_jitter_probe(cpu, &jitter_secs)?);
        }
        println!(
            "capacity-ramp: host jitter on Restream CPUs: {}",
            Value::Object(host_jitter.clone())
        );
    }

    // --- Provenance --------------------------------------------------------
    let lscpu = lscpu_fields();
    let provenance = json!({
        "commit": git(&["rev-parse", "HEAD"]).unwrap_or_default(),
        // Tracked changes only, matching the preflight; untracked scratch
        // files do not change the build.
        "dirty": git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty()),
        "kernel": fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim(),
        "cpu_model": lscpu.get("Model name").cloned().unwrap_or_default(),
        "online_cpus": cpus.len(),
        "numa_nodes": lscpu.get("NUMA node(s)").cloned().unwrap_or_default(),
        "hypervisor": lscpu.get("Hypervisor vendor").cloned().unwrap_or_else(|| "bare-metal?".into()),
        "memory": fs::read_to_string("/proc/meminfo").unwrap_or_default().lines().next().unwrap_or_default(),
        "rustc": first_output_line("rustc", &["--version"]),
        "ffmpeg": first_output_line("ffmpeg", &["-version"]),
        "restream_cpus": restream_cpus,
        "harness_cpus": harness_cpus,
        "build_profile": build_profile,
        "egress_shards": egress_shards.clone().unwrap_or_else(|| "default".into()),
        "malloc_arena_max": arena_max.clone().unwrap_or_else(|| "restream provisional default".into()),
        "sink_threads": sink_threads,
        "host_jitter": host_jitter,
        "peer_count": peer_count,
        "bitrate": bitrate,
        "window_secs": window_secs,
        "settle_secs": settle_secs,
        "repeats": repeats,
        "ladders": ladders,
    });
    fs::write(root.join("provenance.json"), format!("{provenance:#}\n"))
        .map_err(|error| format!("cannot write provenance: {error}"))?;
    println!(
        "capacity-ramp: artifacts in {} (restream cpus {restream_cpus}, harness cpus {harness_cpus})",
        root.display()
    );

    // --- Ramp --------------------------------------------------------------
    if let Some(shards) = &egress_shards {
        rung_env.push(("RESTREAM_EGRESS_SHARDS".into(), shards.clone()));
    }
    if let Some(arena_max) = &arena_max {
        rung_env.push(("RESTREAM_MALLOC_ARENA_MAX".into(), arena_max.clone()));
    }
    let fixtures = std::env::current_dir()
        .unwrap_or_default()
        .join("test/fixtures/tls");
    let tls_cert = fixtures
        .join("mediamtx-rtmps-cert.pem")
        .display()
        .to_string();
    let tls_key = fixtures
        .join("mediamtx-rtmps-key.pem")
        .display()
        .to_string();
    for (key, value) in [
        ("RESTREAM_CPUSET", restream_cpus.clone()),
        ("MSR_PEER", "sink".into()),
        ("PEER_COUNT", peer_count.to_string()),
        (
            "RESTREAM_BIN",
            bin_dir.join("restream").display().to_string(),
        ),
        ("RESOURCE_SWEEP_INGEST_COUNTS", "1".into()),
        ("RESOURCE_SWEEP_BITRATE", bitrate.clone()),
        ("RESOURCE_SWEEP_RTMPS_CERT", tls_cert.clone()),
        ("RESOURCE_SWEEP_RTMPS_KEY", tls_key),
        ("RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM", tls_cert),
        ("RESOURCE_SWEEP_SETTLE_SECS", settle_secs.to_string()),
        ("RESOURCE_SWEEP_SAMPLE_SECS", window_secs.to_string()),
    ] {
        rung_env.push((key.into(), value));
    }
    for protocol in &protocols {
        let (_, _, _, scenario) = PROTOCOLS
            .iter()
            .find(|(name, ..)| name == protocol)
            .expect("validated above");
        for outputs in ladders[protocol.as_str()].split(',') {
            let outputs: u64 = outputs
                .trim()
                .parse()
                .map_err(|_| format!("capacity-ramp: bad {protocol} ladder entry '{outputs}'"))?;
            let mut passes = 0;
            for rep in 1..=repeats {
                let dir = root
                    .join(protocol)
                    .join(outputs.to_string())
                    .join(format!("rep{rep}"));
                println!(
                    "[capacity-ramp] {protocol} outputs={outputs} rep={rep} start {}",
                    &utc_timestamp(SystemTime::now())[11..19]
                );
                let ran = run_rung(&bin_dir, &harness_cpus, &rung_env, scenario, outputs, &dir);
                if !ran {
                    println!(
                        "[capacity-ramp] {protocol} outputs={outputs} rep={rep} harness failed (see {}/harness.log)",
                        dir.display()
                    );
                }
                if rung_passed(&dir.join("resource-sweep-results.csv"), outputs) {
                    passes += 1;
                }
                sleep(Duration::from_secs(5)); // let this rung's sockets drain before the next binds
            }
            println!("[capacity-ramp] {protocol} outputs={outputs} passed {passes}/{repeats}");
            if stop_after_fail && passes == 0 {
                println!(
                    "[capacity-ramp] {protocol}: stopping ladder after a rung with no passing repeat"
                );
                break;
            }
        }
    }

    summarize(&root)?;
    println!(
        "capacity-ramp: summary at {}",
        root.join("summary.md").display()
    );
    Ok(())
}

/// One rung: the harness's resource-sweep on the harness CPU set, its output
/// in `<dir>/harness.log`. Returns whether the harness exited successfully.
fn run_rung(
    bin_dir: &Path,
    harness_cpus: &str,
    env: &[(String, String)],
    scenario: &str,
    outputs: u64,
    dir: &Path,
) -> bool {
    let Ok(()) = fs::create_dir_all(dir) else {
        return false;
    };
    let Ok(log) = File::create(dir.join("harness.log")) else {
        return false;
    };
    let Ok(log_err) = log.try_clone() else {
        return false;
    };
    Command::new("taskset")
        .args(["-c", harness_cpus])
        .arg(bin_dir.join("test_harness"))
        .args(["resource-sweep", "--no-netns"])
        .envs(env.iter().map(|(key, value)| (key, value)))
        .env("WORK_DIR", dir)
        .env("RESOURCE_SWEEP_SCENARIOS", scenario)
        .env("RESOURCE_SWEEP_EGRESS_COUNTS", outputs.to_string())
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .status()
        .is_ok_and(|status| status.success())
}

/// A rung passes when some sample row delivered to every destination.
fn rung_passed(csv: &Path, outputs: u64) -> bool {
    fs::read_to_string(csv).is_ok_and(|text| {
        csv_rows(&text)
            .iter()
            .any(|row| number(row.get("delivery_delivered")).max(0.0) as u64 >= outputs)
    })
}

/// Raises the open-file soft limit to 65,536 when it is lower; children
/// (the harness and Restream) inherit it.
fn ensure_nofile() -> StepResult {
    let limits = fs::read_to_string("/proc/self/limits").unwrap_or_default();
    let soft = limits
        .lines()
        .find(|line| line.starts_with("Max open files"))
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    if soft >= MIN_NOFILE {
        return Ok(());
    }
    let raised = Command::new("prlimit")
        .arg(format!("--pid={}", std::process::id()))
        .arg(format!("--nofile={MIN_NOFILE}:"))
        .status()
        .is_ok_and(|status| status.success());
    if raised {
        Ok(())
    } else {
        Err(format!(
            "capacity-ramp: open-file limit {soft} < {MIN_NOFILE} and cannot be raised"
        ))
    }
}

/// First number of `/proc/sys/<key>`.
fn sysctl_first(key: &str) -> Option<u64> {
    fs::read_to_string(Path::new("/proc/sys").join(key))
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn process_running(name: &str) -> bool {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|pid| pid.bytes().all(|b| b.is_ascii_digit()))
                && fs::read_to_string(entry.path().join("comm"))
                    .is_ok_and(|comm| comm.trim_end() == name)
        })
}

/// `0-3,8,10-11` -> `[0, 1, 2, 3, 8, 10, 11]`.
fn expand_cpu_list(list: &str) -> Result<Vec<u32>, String> {
    let mut cpus = Vec::new();
    for part in list
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let parse = |text: &str| {
            text.parse::<u32>()
                .map_err(|_| format!("bad CPU list '{list}'"))
        };
        match part.split_once('-') {
            Some((low, high)) => cpus.extend(parse(low)?..=parse(high)?),
            None => cpus.push(parse(part)?),
        }
    }
    Ok(cpus)
}

fn lscpu_fields() -> BTreeMap<String, String> {
    let output = Command::new("lscpu")
        .output()
        .map(|out| out.stdout)
        .unwrap_or_default();
    String::from_utf8_lossy(&output)
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

fn first_output_line(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .unwrap_or_default()
}

/// `YYYYMMDDTHHMMSSZ`, as `date -u +%Y%m%dT%H%M%SZ` prints it.
fn compact_stamp(time: SystemTime) -> String {
    utc_timestamp(time).replace(['-', ':'], "")
}

/// Runs the jitter probe pinned to `cpu` in a child xtask, so only the probe
/// is on that CPU.
fn pinned_jitter_probe(cpu: &str, seconds: &str) -> Result<Value, String> {
    let exe = std::env::current_exe().map_err(|error| format!("cannot locate xtask: {error}"))?;
    let output = Command::new("taskset")
        .args(["-c", cpu])
        .arg(exe)
        .args(["host-jitter", seconds, "5"])
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot start jitter probe: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "jitter probe on CPU {cpu} failed ({})",
            output.status
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("bad jitter probe output: {error}"))
}

/// `cargo xtask host-jitter [seconds] [threshold_ms]`: spin on a monotonic
/// clock on the current CPU and count gaps between consecutive reads longer
/// than the threshold. On a VM, gaps with no competing work come from the
/// hypervisor descheduling the vCPU; every thread on that CPU loses the same
/// wall time. Prints one JSON object. Pin it with `taskset -c <cpu>`.
pub(crate) fn host_jitter(args: &[&str]) -> StepResult {
    let parse = |index: usize, default: f64| -> Result<f64, String> {
        args.get(index).map_or(Ok(default), |value| {
            value
                .parse()
                .map_err(|_| format!("host-jitter: '{value}' is not a number"))
        })
    };
    let seconds = parse(0, 20.0)?;
    let threshold = Duration::from_secs_f64(parse(1, 5.0)? / 1000.0);
    let start = Instant::now();
    let mut previous = start;
    let (mut gaps, mut worst, mut lost) = (0u64, Duration::ZERO, Duration::ZERO);
    loop {
        let now = Instant::now();
        let gap = now - previous;
        if gap > threshold {
            gaps += 1;
            lost += gap;
            worst = worst.max(gap);
        }
        previous = now;
        if (now - start).as_secs_f64() >= seconds {
            break;
        }
    }
    let round = |value: f64, places: i32| (value * 10f64.powi(places)).round() / 10f64.powi(places);
    let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;
    println!(
        "{}",
        json!({
            "seconds": seconds,
            "threshold_ms": ms(threshold),
            "gaps": gaps,
            "worst_ms": round(ms(worst), 1),
            "lost_ms": round(ms(lost), 1),
            "lost_pct": round(100.0 * lost.as_secs_f64() / seconds, 2),
        })
    );
    Ok(())
}

// --- Summary ---------------------------------------------------------------

/// One resource-sweep CSV row by column name.
type Row = BTreeMap<String, String>;

/// Rows of a CSV whose fields may be double-quoted (`""` escapes a quote).
fn csv_rows(text: &str) -> Vec<Row> {
    let mut lines = text.lines().filter(|line| !line.is_empty());
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let header = csv_fields(header);
    lines
        .map(|line| header.iter().cloned().zip(csv_fields(line)).collect())
        .collect()
}

fn csv_fields(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        let field = fields.last_mut().expect("starts with one field");
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => fields.push(String::new()),
            _ => field.push(c),
        }
    }
    fields
}

/// Python's `float(value)` with NaN for missing or malformed input.
fn number(value: Option<&String>) -> f64 {
    value
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(f64::NAN)
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    match values.len() {
        0 => f64::NAN,
        n if n % 2 == 1 => values[n / 2],
        n => (values[n / 2 - 1] + values[n / 2]) / 2.0,
    }
}

struct Rung {
    protocol: String,
    outputs: u64,
    repeats: usize,
    passed: usize,
    rx_delivered_min: u64,
    rx_ratio_min: f64,
    rx_ratio_median_med: f64,
    rx_interval_min: f64,
    rx_jain_min: f64,
    restream_delivered_min: u64,
    restream_ratio_min: f64,
    cpu_avg_median: f64,
    cpu_peak_max: f64,
    rss_peak_mb_max: f64,
    threads_peak_max: f64,
}

fn rung_summary(protocol: &str, outputs: u64, runs: &[Row]) -> Rung {
    let column = |name: &str| {
        runs.iter()
            .map(|row| number(row.get(name)))
            .collect::<Vec<_>>()
    };
    let min = |name: &str| column(name).into_iter().fold(f64::NAN, f64::min);
    let max = |name: &str| column(name).into_iter().fold(f64::NAN, f64::max);
    let count = |name: &str| -> Vec<u64> {
        column(name)
            .into_iter()
            .map(|value| if value.is_nan() { 0 } else { value as u64 })
            .collect()
    };
    let delivered = count("delivery_delivered");
    Rung {
        protocol: protocol.to_owned(),
        outputs,
        repeats: runs.len(),
        passed: delivered.iter().filter(|&&d| d >= outputs).count(),
        rx_delivered_min: delivered.iter().copied().min().unwrap_or(0),
        rx_ratio_min: min("delivery_ratio_min"),
        rx_ratio_median_med: median(column("delivery_ratio_median")),
        rx_interval_min: min("delivery_interval_ratio_min"),
        rx_jain_min: min("delivery_jain"),
        restream_delivered_min: count("restream_delivery_delivered")
            .into_iter()
            .min()
            .unwrap_or(0),
        restream_ratio_min: min("restream_delivery_ratio_min"),
        cpu_avg_median: median(column("restream_cpu_avg_pct")),
        cpu_peak_max: max("restream_cpu_peak_pct"),
        rss_peak_mb_max: max("rss_peak_kb") / 1024.0,
        threads_peak_max: max("thread_count_peak"),
    }
}

/// Python `repr` of a float: `12.0`, `0.95`, `nan`.
fn py_float(value: f64) -> String {
    if value.is_nan() {
        "nan".into()
    } else if value.is_finite() && value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }
}

/// Writes `<root>/summary.csv` and `<root>/summary.md` and prints the latter.
fn summarize(root: &Path) -> StepResult {
    let mut rungs: BTreeMap<(String, u64), Vec<Row>> = BTreeMap::new();
    for protocol_dir in subdirs(root) {
        for outputs_dir in subdirs(&protocol_dir) {
            let (Some(protocol), Some(outputs)) = (
                protocol_dir.file_name().and_then(|name| name.to_str()),
                outputs_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.parse().ok()),
            ) else {
                continue;
            };
            for rep_dir in subdirs(&outputs_dir) {
                if !rep_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("rep"))
                {
                    continue;
                }
                if let Ok(text) = fs::read_to_string(rep_dir.join("resource-sweep-results.csv")) {
                    rungs
                        .entry((protocol.to_owned(), outputs))
                        .or_default()
                        .extend(csv_rows(&text));
                }
            }
        }
    }
    let rows: Vec<Rung> = rungs
        .iter()
        .map(|((protocol, outputs), runs)| rung_summary(protocol, *outputs, runs))
        .collect();

    let mut csv = String::from(
        "protocol,outputs,repeats,passed,rx_delivered_min,rx_ratio_min,rx_ratio_median_med,rx_interval_min,\
         rx_jain_min,restream_delivered_min,restream_ratio_min,cpu_avg_median,cpu_peak_max,rss_peak_mb_max,threads_peak_max\n",
    );
    for r in &rows {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            r.protocol,
            r.outputs,
            r.repeats,
            r.passed,
            r.rx_delivered_min,
            py_float(r.rx_ratio_min),
            py_float(r.rx_ratio_median_med),
            py_float(r.rx_interval_min),
            py_float(r.rx_jain_min),
            r.restream_delivered_min,
            py_float(r.restream_ratio_min),
            py_float(r.cpu_avg_median),
            py_float(r.cpu_peak_max),
            py_float(r.rss_peak_mb_max),
            py_float(r.threads_peak_max),
        ));
    }
    fs::write(root.join("summary.csv"), csv)
        .map_err(|error| format!("cannot write summary.csv: {error}"))?;

    let provenance: Value = fs::read_to_string(root.join("provenance.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    let markdown = summary_markdown(&provenance, &rows);
    fs::write(root.join("summary.md"), &markdown)
        .map_err(|error| format!("cannot write summary.md: {error}"))?;
    println!("{markdown}");
    Ok(())
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn summary_markdown(provenance: &Value, rows: &[Rung]) -> String {
    // Strings print bare; anything else prints as JSON (numbers, null).
    let field = |key: &str, default: &str| match provenance.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => default.to_owned(),
    };
    let mut lines = vec!["# Capacity ramp".to_owned(), String::new()];
    if provenance.as_object().is_some_and(|map| !map.is_empty()) {
        let dirty = if provenance["dirty"].as_bool() == Some(true) {
            " (dirty)"
        } else {
            ""
        };
        let jitter = provenance["host_jitter"]
            .as_object()
            .map(|cpus| {
                cpus.iter()
                    .map(|(cpu, result)| {
                        format!(
                            "cpu{cpu} {}% lost, worst {} ms",
                            result["lost_pct"], result["worst_ms"]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "not measured".into());
        lines.extend([
            format!("- commit `{}`{dirty}", field("commit", "?")),
            format!("- build profile: {}", field("build_profile", "bench")),
            format!(
                "- CPU: {}, {} online, NUMA nodes {}, hypervisor {}",
                field("cpu_model", "?"),
                field("online_cpus", "?"),
                field("numa_nodes", "?"),
                field("hypervisor", "?")
            ),
            format!("- kernel {}; {}", field("kernel", "?"), field("memory", "")),
            format!(
                "- Restream CPUs `{}`, harness/sink CPUs `{}`, egress shards {}, SRT sink threads {}, malloc arenas {}",
                field("restream_cpus", "None"),
                field("harness_cpus", "None"),
                field("egress_shards", "None"),
                field("sink_threads", "None"),
                field("malloc_arena_max", "restream provisional default")
            ),
            format!("- host jitter (gaps > 5 ms on each Restream CPU, nothing else scheduled): {jitter}"),
            format!(
                "- one ingest at {}, window {} s, {} repeats per rung; pass = every destination >= 0.95",
                field("bitrate", "None"),
                field("window_secs", "None"),
                field("repeats", "None")
            ),
            String::new(),
        ]);
    }
    let mut protocols: Vec<&str> = rows.iter().map(|row| row.protocol.as_str()).collect();
    protocols.dedup();
    for protocol in protocols {
        let protocol_rows: Vec<&Rung> =
            rows.iter().filter(|row| row.protocol == protocol).collect();
        let capacity = protocol_rows
            .iter()
            .filter(|row| row.passed == row.repeats)
            .map(|row| row.outputs)
            .max()
            .unwrap_or(0);
        // HLS PUT is graded per segment (ratio = due segments received); the
        // byte-interval ratio and Restream's fabric-leaf view do not apply.
        let hls = protocol == "hls";
        lines.push(format!(
            "## {}: capacity {capacity} outputs (all repeats passed)",
            protocol.to_uppercase()
        ));
        lines.push(String::new());
        if hls {
            lines.push(
                "HLS PUT: rx ratio is the share of due segments received; an output passes with every \
                 due segment, each within 3 s of the first output."
                    .into(),
            );
            lines.push(String::new());
        }
        lines.push(
            "| outputs | passed | rx delivered min | rx ratio min | rx interval min | rx Jain min \
             | Restream delivered min | CPU avg (median) | CPU peak | RSS MB |"
                .into(),
        );
        lines.push("|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|".into());
        for row in protocol_rows {
            let interval = if hls {
                "—".to_owned()
            } else {
                format!("{:.3}", row.rx_interval_min)
            };
            let restream = if hls {
                "—".to_owned()
            } else {
                row.restream_delivered_min.to_string()
            };
            lines.push(format!(
                "| {} | {}/{} | {} | {:.3} | {interval} | {:.5} | {restream} | {:.1}% | {:.1}% | {:.0} |",
                row.outputs,
                row.passed,
                row.repeats,
                row.rx_delivered_min,
                row.rx_ratio_min,
                row.rx_jain_min,
                row.cpu_avg_median,
                row.cpu_peak_max,
                row.rss_peak_mb_max,
            ));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_fields_honor_quotes_and_escaped_quotes() {
        assert_eq!(
            csv_fields(r#"a,"b,c","say ""hi""",,d"#),
            vec!["a", "b,c", r#"say "hi""#, "", "d"]
        );
    }

    #[test]
    fn cpu_lists_expand_ranges() {
        assert_eq!(
            expand_cpu_list("0-3,8,10-11").unwrap(),
            vec![0, 1, 2, 3, 8, 10, 11]
        );
        assert!(expand_cpu_list("0-x").is_err());
    }

    #[test]
    fn rung_summary_counts_passes_and_takes_worst_case() {
        let csv = "delivery_delivered,delivery_ratio_min,delivery_ratio_median,restream_cpu_avg_pct,rss_peak_kb,egress_mix\n\
                   100,0.97,0.99,40,2048,\"rtmp:50,srt:50\"\n\
                   99,0.91,0.98,50,4096,x\n\
                   ,0.99,0.97,60,1024,x\n";
        let rows = csv_rows(csv);
        assert_eq!(rows[0]["egress_mix"], "rtmp:50,srt:50");
        let rung = rung_summary("rtmp", 100, &rows);
        assert_eq!(
            (rung.repeats, rung.passed, rung.rx_delivered_min),
            (3, 1, 0)
        );
        assert_eq!(rung.rx_ratio_min, 0.91);
        assert_eq!(rung.rx_ratio_median_med, 0.98);
        assert_eq!(rung.cpu_avg_median, 50.0);
        assert_eq!(rung.rss_peak_mb_max, 4.0);
        assert!(rung.rx_jain_min.is_nan(), "missing column stays NaN");
    }

    #[test]
    fn capacity_is_the_highest_rung_where_every_repeat_passed() {
        let row = |outputs, repeats, passed| Rung {
            outputs,
            repeats,
            passed,
            ..rung_summary("srt", 0, &[])
        };
        let rows = [
            row(50, 3, 3),
            row(100, 3, 3),
            row(150, 2, 1),
            row(200, 3, 3),
        ];
        let markdown = summary_markdown(&Value::Null, &rows);
        assert!(
            markdown.contains("## SRT: capacity 200 outputs (all repeats passed)"),
            "{markdown}"
        );
        assert!(markdown.contains("| 150 | 1/2 |"), "{markdown}");
        let failing = [row(50, 3, 2)];
        assert!(summary_markdown(&Value::Null, &failing).contains("capacity 0 outputs"));
    }

    #[test]
    fn python_float_repr() {
        assert_eq!(py_float(12.0), "12.0");
        assert_eq!(py_float(0.95), "0.95");
        assert_eq!(py_float(f64::NAN), "nan");
    }
}
