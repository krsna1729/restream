//! Concurrency proof gates (`docs/concurrency-proofing.md`): `fast` runs the
//! loom models; `contract` runs live fault and recovery harness modes with a
//! guard that no media process outlives them. Ordinary unit, integration and
//! property tests run in CI's workspace test job, not here.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use crate::{StepResult, capture_into, cargo, child_output, run};

/// `tests/<target>.rs` loom models, each built with `--cfg loom`.
const LOOM_TARGETS: &[&str] = &[
    "byte_queue_loom",
    "egress_feed_wake_loom",
    "input_selection_loom",
    "keyed_lock_loom",
    "moq_origin_loom",
    "ring_frame_unit_loom",
    "ring_migration_loom",
    "ts_chunk_ring_loom",
    "ts_muxer_stage_loom",
    "transcoder_stage_loom",
];

/// Live harness modes run by `contract`, each with its own work directory.
const CONTRACT_MODES: &[(&str, &str)] = &[
    ("fault.resilience", ".local/artifacts/concurrency-contract"),
    (
        "fault.egress-retry",
        ".local/artifacts/concurrency-fault-egress-retry",
    ),
    (
        "fault.output-stall",
        ".local/artifacts/concurrency-fault-output-stall",
    ),
    (
        "fault.srt-output-stall",
        ".local/artifacts/concurrency-fault-srt-output-stall",
    ),
    (
        "fault.sibling-isolation",
        ".local/artifacts/concurrency-fault-sibling-isolation",
    ),
    ("recovery", ".local/artifacts/concurrency-recovery"),
];

/// Process names a harness run may start; none may survive it.
const RUNTIME_PROCESSES: &[&str] = &[
    "restream",
    "restream-fault",
    "mediamtx",
    "ffmpeg",
    "ffprobe",
    "test_harness",
];

const LOG_DIR: &str = ".local/artifacts/concurrency-contract-logs";

pub(crate) fn fast() -> StepResult {
    for target in LOOM_TARGETS {
        loom(target)?;
    }
    Ok(())
}

pub(crate) fn contract() -> StepResult {
    fs::create_dir_all(LOG_DIR).map_err(|error| format!("cannot create {LOG_DIR}: {error}"))?;
    let baseline: HashSet<u32> = runtime_processes().into_iter().map(|row| row.pid).collect();
    let result = contract_steps(&baseline);
    // Like the shell trap on EXIT: clean up after success and failure alike.
    stop_new_processes(&baseline);
    result
}

fn contract_steps(baseline: &HashSet<u32>) -> StepResult {
    logged("process-lifecycle-guards", || {
        process_lifecycle_guards(Path::new("crates/test_harness/src/main.rs"))
    })?;
    logged("build-harness-bins", || {
        cargo(&[
            "build",
            "-p",
            "restream",
            "--bin",
            "restream",
            "-p",
            "test_harness",
            "--bin",
            "test_harness",
        ])
    })?;
    let fault_tests = prepare_live_fault_tests(None, "target/debug")?;
    let extra = std::env::var("CONCURRENCY_HARNESS_ARGS").unwrap_or_default();
    for (mode, work_dir) in CONTRACT_MODES {
        stop_new_processes(baseline);
        let mut args = vec![*mode];
        args.extend(extra.split_whitespace());
        let outcome = logged(mode, || {
            run(
                "target/debug/test_harness",
                &args,
                &[
                    ("RESTREAM_BIN", "target/debug/restream"),
                    ("WORK_DIR", work_dir),
                    ("RESTREAM_FAULT_TEST_BIN", &fault_tests),
                ],
            )
        });
        stop_new_processes(baseline);
        let label = if outcome.is_ok() {
            mode.to_string()
        } else {
            format!("{mode} failure cleanup")
        };
        assert_no_new_processes(baseline, &label)?;
        outcome?;
    }
    Ok(())
}

/// Runs `step` with every child's stdout and stderr in
/// `LOG_DIR/<label>.log`, printing the log only when the step fails.
fn logged(label: &str, step: impl FnOnce() -> StepResult) -> StepResult {
    let path = PathBuf::from(LOG_DIR).join(format!("{label}.log"));
    let log = fs::File::create(&path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    let result = capture_into(log, step);
    if result.is_err()
        && let Ok(text) = fs::read_to_string(&path)
    {
        print!("{text}");
    }
    result.map_err(|error| format!("{label}: {error}"))
}

/// Every child spawn in the harness must arm `kill_on_drop(true)` first, and
/// the shared `kill_and_wait_child` teardown helper must exist.
fn process_lifecycle_guards(harness: &Path) -> StepResult {
    let text = fs::read_to_string(harness)
        .map_err(|error| format!("cannot read {}: {error}", harness.display()))?;
    let unarmed = unarmed_spawns(&text);
    if let Some(line) = unarmed.first() {
        return Err(format!(
            "process lifecycle guard failed: spawn without preceding kill_on_drop(true) near {}:{line}",
            harness.display()
        ));
    }
    if !text.contains("async fn kill_and_wait_child") {
        return Err(format!(
            "process lifecycle guard failed: missing kill_and_wait_child helper in {}",
            harness.display()
        ));
    }
    Ok(())
}

/// 1-based lines with a `.spawn()` not preceded by `kill_on_drop(true)` since
/// the previous spawn (the arming check also counts the spawn line itself).
fn unarmed_spawns(text: &str) -> Vec<usize> {
    let mut armed = false;
    let mut unarmed = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.contains("kill_on_drop(true)") {
            armed = true;
        }
        if line.contains(".spawn()") {
            if !armed {
                unarmed.push(index + 1);
            }
            armed = false;
        }
    }
    unarmed
}

pub(crate) fn loom(target: &str) -> StepResult {
    eprintln!("==> loom {target}");
    let (_, stderr) = child_output()?;
    let output = Command::new("cargo")
        .args([
            "rustc",
            "--test",
            target,
            "--message-format=json-render-diagnostics",
            "--",
            "--cfg",
            "loom",
        ])
        .stderr(stderr)
        .output()
        .map_err(|error| format!("cannot start cargo rustc: {error}"))?;
    if !output.status.success() {
        return Err(format!("loom build of {target} failed ({})", output.status));
    }
    let binary = loom_executable(&String::from_utf8_lossy(&output.stdout), target)
        .ok_or_else(|| format!("failed to locate compiled loom binary for {target}"))?;
    run(&binary, &["--nocapture"], &[])
}

/// The last `compiler-artifact` executable built for `target`.
fn loom_executable(messages: &str, target: &str) -> Option<String> {
    messages
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| {
            message["reason"] == "compiler-artifact" && message["target"]["name"] == target
        })
        .filter_map(|message| message["executable"].as_str().map(str::to_owned))
        .next_back()
}

/// Compile cfg(test) fault seams before any media starts, then run only the
/// named live cases from the executable (never cargo during media).
pub(crate) fn prepare_live_fault_tests(profile: Option<&str>, dir: &str) -> Result<String, String> {
    let mut command = Command::new("cargo");
    command.args([
        "test",
        "-p",
        "restream-engine",
        "--lib",
        "--no-run",
        "--message-format=json",
    ]);
    if let Some(profile) = profile {
        command.args(["--profile", profile]);
    }
    let output = command
        .output()
        .map_err(|error| format!("build live fault test target: {error}"))?;
    if !output.status.success() {
        let diagnostics: String = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|message| message["message"]["rendered"].as_str().map(str::to_owned))
            .collect();
        return Err(format!(
            "live fault test target build failed: {}{diagnostics}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let executable = loom_executable(&String::from_utf8_lossy(&output.stdout), "restream_engine")
        .ok_or("engine lib-test executable missing from Cargo artifacts")?;
    fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    // Fits Linux's 15-byte comm limit, so survivor cleanup can identify it.
    let destination = format!("{dir}/restream-fault");
    fs::copy(&executable, &destination)
        .map_err(|error| format!("copy live fault executable: {error}"))?;
    Ok(destination)
}

struct RuntimeProcess {
    pid: u32,
    row: String,
}

fn runtime_processes() -> Vec<RuntimeProcess> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(comm) = fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        let comm = comm.trim_end();
        if !RUNTIME_PROCESSES.contains(&comm) {
            continue;
        }
        let args = fs::read(entry.path().join("cmdline"))
            .map(|raw| {
                String::from_utf8_lossy(&raw)
                    .replace('\0', " ")
                    .trim_end()
                    .to_owned()
            })
            .unwrap_or_default();
        rows.push(RuntimeProcess {
            pid,
            row: format!("{pid} {comm} {args}"),
        });
    }
    rows
}

fn new_processes(baseline: &HashSet<u32>) -> Vec<RuntimeProcess> {
    runtime_processes()
        .into_iter()
        .filter(|process| !baseline.contains(&process.pid))
        .collect()
}

/// TERM, then KILL, any runtime process this run started; never touches a
/// process that was already running when the gate began.
fn stop_new_processes(baseline: &HashSet<u32>) {
    for signal in ["-TERM", "-KILL"] {
        let pids: Vec<String> = new_processes(baseline)
            .iter()
            .map(|process| process.pid.to_string())
            .collect();
        if pids.is_empty() {
            return;
        }
        let _ = Command::new("kill")
            .arg(signal)
            .arg("--")
            .args(&pids)
            .stderr(Stdio::null())
            .status();
        for _ in 0..10 {
            if new_processes(baseline).is_empty() {
                return;
            }
            sleep(Duration::from_millis(500));
        }
    }
}

fn assert_no_new_processes(baseline: &HashSet<u32>, label: &str) -> StepResult {
    for _ in 0..10 {
        let survivors = new_processes(baseline);
        if survivors.is_empty() {
            return Ok(());
        }
        sleep(Duration::from_millis(500));
    }
    let rows: Vec<String> = new_processes(baseline)
        .into_iter()
        .map(|process| process.row)
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    Err(format!(
        "runtime cleanup guard failed after {label}:\n{}",
        rows.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_must_be_armed_since_the_previous_spawn() {
        let text = "\
let a = cmd.kill_on_drop(true);
a.spawn()?;
b.spawn()?;
c.kill_on_drop(true).spawn()?;
";
        assert_eq!(unarmed_spawns(text), vec![3]);
    }

    #[test]
    fn loom_executable_takes_last_matching_artifact() {
        let messages = r#"{"reason":"compiler-artifact","target":{"name":"other"},"executable":"/t/other"}
{"reason":"compiler-artifact","target":{"name":"byte_queue_loom"},"executable":null}
{"reason":"build-finished","success":true}
{"reason":"compiler-artifact","target":{"name":"byte_queue_loom"},"executable":"/t/byte_queue_loom-1"}"#;
        assert_eq!(
            loom_executable(messages, "byte_queue_loom").as_deref(),
            Some("/t/byte_queue_loom-1")
        );
        assert_eq!(loom_executable(messages, "missing"), None);
    }
}
