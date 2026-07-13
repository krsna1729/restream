//! Suite and preflight orchestration for the test harness.

use super::*;

const DEFAULT_SUITE_MODE_TIMEOUT_SECS: u64 = 15 * 60;

pub(crate) fn suite_mode_timeout_secs(mode: &str, default_secs: u64) -> u64 {
    mode_spec(mode)
        .and_then(|spec| spec.suite_timeout_secs)
        .map(|mode_secs| {
            // A mode catalog timeout is a safety minimum for heavyweight
            // release evidence modes. Keep it as a floor so a global suite
            // default change cannot silently reintroduce artificial timeouts
            // for modes that are still making expected progress.
            default_secs.max(mode_secs)
        })
        .unwrap_or(default_secs)
}

pub(crate) fn suite_mode_is_parallelizable(mode: &str, preflight_only: bool) -> bool {
    !preflight_only && !measurement_mode_requires_bench_profile(mode)
}

/// Result summary for one child mode launched by the aggregate suite runner.
struct SuiteModeOutcome {
    mode: String,
    mode_dir: PathBuf,
    started_at: String,
    finished_at: String,
    exit_ok: bool,
    timed_out: bool,
    timeout_secs: u64,
}

struct SuiteSpawnOutcome {
    exit_ok: bool,
    timed_out: bool,
}

#[allow(clippy::too_many_arguments)]
async fn suite_run_mode(
    exe: PathBuf,
    mode: String,
    mode_dir: PathBuf,
    command: String,
    has_unshare: bool,
    use_host_net: bool,
    timeout: Duration,
) -> Result<SuiteModeOutcome, String> {
    let started_at = Utc::now().to_rfc3339();
    let spawn_mode_dir = mode_dir.clone();
    let spawn_mode = mode.clone();
    let exit_ok = tokio::task::spawn_blocking(move || {
        suite_spawn_mode(
            &exe,
            &spawn_mode,
            &command,
            &spawn_mode_dir,
            has_unshare,
            use_host_net,
            timeout,
        )
    })
    .await
    .map_err(|e| format!("suite worker join failed for {mode}: {e}"))??;
    let finished_at = Utc::now().to_rfc3339();
    Ok(SuiteModeOutcome {
        mode,
        mode_dir,
        started_at,
        finished_at,
        exit_ok: exit_ok.exit_ok,
        timed_out: exit_ok.timed_out,
        timeout_secs: timeout.as_secs(),
    })
}

async fn suite_run_parallel_batch(
    exe: &Path,
    modes: &[String],
    work_root: &Path,
    preflight_only: bool,
    has_unshare: bool,
    use_host_net: bool,
    default_timeout_secs: u64,
) -> Result<Vec<SuiteModeOutcome>, String> {
    let mut join_set = tokio::task::JoinSet::new();
    for mode in modes {
        let mode_dir = work_root.join(mode);
        std::fs::create_dir_all(&mode_dir).map_err(|e| e.to_string())?;
        let command = if preflight_only {
            "preflight".to_string()
        } else {
            mode.clone()
        };
        let timeout = Duration::from_secs(suite_mode_timeout_secs(mode, default_timeout_secs));
        println!(
            "[suite] {} {mode}",
            if preflight_only { "preflight" } else { "run" }
        );
        join_set.spawn(suite_run_mode(
            exe.to_path_buf(),
            mode.clone(),
            mode_dir,
            command,
            has_unshare,
            use_host_net,
            timeout,
        ));
    }

    let mut outcomes = Vec::with_capacity(modes.len());
    while let Some(result) = join_set.join_next().await {
        let outcome = result.map_err(|e| format!("suite batch join failed: {e}"))??;
        // Keep completed outcomes in finish order so the aggregate suite writes
        // early evidence for fast siblings instead of hiding them behind the
        // slowest parallel child. This is especially useful in CI release runs,
        // where the first actionable signal should arrive within seconds.
        outcomes.push(outcome);
    }

    Ok(outcomes)
}

pub(crate) async fn suite_run() -> Result<Value, String> {
    let raw: Vec<String> = std::env::args().skip(2).collect();
    let mut modes = suite_default_modes();
    let mut continue_on_fail = false;
    let mut preflight_only = false;
    let mut use_host_net = std::env::var("TEST_HARNESS_USE_HOST_NET")
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let mut run_id = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut work_root: Option<PathBuf> = std::env::var_os("WORK_ROOT").map(PathBuf::from);
    let mut mode_timeout_secs = std::env::var("TEST_HARNESS_SUITE_MODE_TIMEOUT_SECS")
        .ok()
        .map(|value| {
            value.parse::<u64>().map_err(|_| {
                "TEST_HARNESS_SUITE_MODE_TIMEOUT_SECS must be a positive integer".to_string()
            })
        })
        .transpose()?
        .unwrap_or(DEFAULT_SUITE_MODE_TIMEOUT_SECS);

    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--only-modes" => {
                i += 1;
                modes = raw
                    .get(i)
                    .ok_or("--only-modes requires a value")?
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "--run-id" => {
                i += 1;
                run_id = raw.get(i).ok_or("--run-id requires a value")?.clone();
            }
            "--work-root" => {
                i += 1;
                work_root = Some(PathBuf::from(
                    raw.get(i).ok_or("--work-root requires a value")?,
                ));
            }
            "--mode-timeout-secs" => {
                i += 1;
                mode_timeout_secs = raw
                    .get(i)
                    .ok_or("--mode-timeout-secs requires a value")?
                    .parse()
                    .map_err(|_| "--mode-timeout-secs must be a positive integer".to_string())?;
            }
            "--no-netns" => use_host_net = true,
            "--continue-on-fail" => continue_on_fail = true,
            "--preflight-only" => preflight_only = true,
            other => return Err(format!("unknown suite option: {other}")),
        }
        i += 1;
    }

    if modes.is_empty() {
        return Err("--only-modes produced an empty mode list".to_string());
    }
    if mode_timeout_secs == 0 {
        return Err("suite mode timeout must be greater than zero".to_string());
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let work_root = {
        let r = work_root.unwrap_or_else(|| cwd.join(".local/artifacts").join(&run_id));
        if r.is_absolute() { r } else { cwd.join(r) }
    };
    std::fs::create_dir_all(&work_root).map_err(|e| e.to_string())?;

    let results_jsonl = work_root.join("results.jsonl");
    let manifest_path = work_root.join("manifest.json");
    std::fs::File::create(&results_jsonl).map_err(|e| e.to_string())?;

    let started_at = Utc::now().to_rfc3339();
    suite_write_manifest(
        &manifest_path,
        "RUNNING",
        &started_at,
        None,
        &run_id,
        &modes,
        &work_root,
        &results_jsonl,
        preflight_only,
        mode_timeout_secs,
    )?;

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let has_unshare = !use_host_net && netns_available();
    let mut overall_ok = true;

    let mut index = 0usize;
    while index < modes.len() {
        if suite_mode_is_parallelizable(&modes[index], preflight_only) && has_unshare {
            let batch_end = modes[index..]
                .iter()
                .take_while(|mode| suite_mode_is_parallelizable(mode, preflight_only))
                .count()
                + index;
            let outcomes = suite_run_parallel_batch(
                &exe,
                &modes[index..batch_end],
                &work_root,
                preflight_only,
                has_unshare,
                use_host_net,
                mode_timeout_secs,
            )
            .await?;
            for outcome in outcomes {
                let mode_status = suite_mode_status(&outcome);
                if !outcome.exit_ok {
                    overall_ok = false;
                }
                suite_append_result(
                    &results_jsonl,
                    &outcome.mode,
                    mode_status,
                    &outcome.started_at,
                    &outcome.finished_at,
                    &outcome.mode_dir,
                    preflight_only,
                    outcome.timed_out,
                    outcome.timeout_secs,
                )?;
                println!("[suite] {}: {mode_status}", outcome.mode);
            }
            index = batch_end;
        } else {
            let mode = &modes[index];
            let mode_dir = work_root.join(mode);
            std::fs::create_dir_all(&mode_dir).map_err(|e| e.to_string())?;
            let mode_started = Utc::now().to_rfc3339();

            let command = if preflight_only {
                "preflight"
            } else {
                mode.as_str()
            };
            println!(
                "[suite] {} {mode}",
                if preflight_only { "preflight" } else { "run" }
            );
            let mode_timeout_secs = suite_mode_timeout_secs(mode, mode_timeout_secs);
            let mode_timeout = Duration::from_secs(mode_timeout_secs);

            let outcome = suite_spawn_mode(
                &exe,
                mode,
                command,
                &mode_dir,
                has_unshare,
                use_host_net,
                mode_timeout,
            )?;
            let mode_status = if outcome.timed_out {
                "TIMEOUT"
            } else if outcome.exit_ok {
                "PASS"
            } else {
                "FAIL"
            };
            if !outcome.exit_ok {
                overall_ok = false;
            }

            let mode_finished = Utc::now().to_rfc3339();
            suite_append_result(
                &results_jsonl,
                mode,
                mode_status,
                &mode_started,
                &mode_finished,
                &mode_dir,
                preflight_only,
                outcome.timed_out,
                mode_timeout_secs,
            )?;
            println!("[suite] {mode}: {mode_status}");
            index += 1;
        }

        if !overall_ok && !continue_on_fail {
            break;
        }
    }

    let finished_at = Utc::now().to_rfc3339();
    let final_status = if overall_ok { "PASS" } else { "FAIL" };
    suite_write_manifest(
        &manifest_path,
        final_status,
        &started_at,
        Some(&finished_at),
        &run_id,
        &modes,
        &work_root,
        &results_jsonl,
        preflight_only,
        mode_timeout_secs,
    )?;
    println!("[suite] manifest={}", manifest_path.display());

    if overall_ok {
        Ok(json!({ "status": "PASS", "manifest": manifest_path }))
    } else {
        Err("suite failed".to_string())
    }
}

fn suite_spawn_mode(
    exe: &Path,
    mode: &str,
    command: &str,
    mode_dir: &Path,
    has_unshare: bool,
    use_host_net: bool,
    timeout: Duration,
) -> Result<SuiteSpawnOutcome, String> {
    use std::os::unix::process::CommandExt;

    let log_path = mode_dir.join("run.log");
    let log_file = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let log_copy = log_file.try_clone().map_err(|e| e.to_string())?;

    let mut child_command = if has_unshare {
        let mut child = std::process::Command::new("unshare");
        child
            .args(["--net", "--user", "--map-root-user"])
            .arg(exe)
            .arg(command)
            .env("WORK_DIR", mode_dir)
            .env("RESTREAM_HARNESS_IN_NETNS", "1");
        child
    } else {
        let mut child = std::process::Command::new(exe);
        child.arg(command).env("WORK_DIR", mode_dir);
        if use_host_net {
            child.env("TEST_HARNESS_USE_HOST_NET", "1");
        }
        child
    };
    // Each suite child gets a process group so a timeout also terminates the
    // MediaMTX, restream, and publisher descendants that it owns. Leaving
    // those behind contaminates later modes and turns one timeout into a chain
    // of misleading port conflicts.
    child_command
        .process_group(0)
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(log_copy));
    let mut child = child_command
        .spawn()
        .map_err(|e| format!("failed to spawn {command}: {e}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("failed to wait for {command}: {e}"))?
        {
            return Ok(SuiteSpawnOutcome {
                exit_ok: status.success(),
                timed_out: false,
            });
        }
        if Instant::now() >= deadline {
            terminate_suite_process_group(&mut child);
            let timeout_path = mode_dir.join("timeout.json");
            let artifact = json!({
                "kind": "suite-mode-timeout",
                "status": "TIMEOUT",
                "mode": mode,
                "command": command,
                "timeoutSeconds": timeout.as_secs(),
                "log": log_path,
                "hint": "inspect run.log and child artifacts; increase --mode-timeout-secs or TEST_HARNESS_SUITE_MODE_TIMEOUT_SECS only when the mode is making expected progress"
            });
            std::fs::write(
                &timeout_path,
                serde_json::to_vec_pretty(&artifact).map_err(|e| e.to_string())?,
            )
            .map_err(|e| format!("failed to write {}: {e}", timeout_path.display()))?;
            return Ok(SuiteSpawnOutcome {
                exit_ok: false,
                timed_out: true,
            });
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn terminate_suite_process_group(child: &mut std::process::Child) {
    let process_group = -(child.id() as i32);
    // SAFETY: the child was spawned as the leader of its own process group;
    // negative PID signalling is the POSIX API for that group only.
    unsafe {
        libc::kill(process_group, libc::SIGTERM);
    }
    let grace_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < grace_deadline {
        let _ = child.try_wait();
        // SAFETY: signal 0 only probes whether this dedicated process group
        // still has members; it does not deliver a signal.
        if unsafe { libc::kill(process_group, 0) } != 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // SAFETY: same dedicated process group as above; SIGKILL bounds cleanup
    // when a subprocess ignores graceful termination.
    unsafe {
        libc::kill(process_group, libc::SIGKILL);
    }
    let _ = child.wait();
}

fn suite_mode_status(outcome: &SuiteModeOutcome) -> &'static str {
    if outcome.timed_out {
        "TIMEOUT"
    } else if outcome.exit_ok {
        "PASS"
    } else {
        "FAIL"
    }
}

#[allow(clippy::too_many_arguments)]
fn suite_write_manifest(
    path: &Path,
    status: &str,
    started_at: &str,
    finished_at: Option<&str>,
    run_id: &str,
    modes: &[String],
    work_root: &Path,
    results_jsonl: &Path,
    preflight_only: bool,
    mode_timeout_secs: u64,
) -> Result<(), String> {
    let manifest = json!({
        "kind": "suite",
        "status": status,
        "runId": run_id,
        "startedAt": started_at,
        "finishedAt": finished_at,
        "workRoot": work_root,
        "modes": modes,
        "resultsJsonl": results_jsonl,
        "evidenceKind": if preflight_only { "preflight" } else { "execution" },
        "preflightOnly": preflight_only,
        "modeTimeoutSeconds": mode_timeout_secs,
    });
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
fn suite_append_result(
    path: &Path,
    mode: &str,
    status: &str,
    started_at: &str,
    finished_at: &str,
    mode_dir: &Path,
    preflight_only: bool,
    timed_out: bool,
    mode_timeout_secs: u64,
) -> Result<(), String> {
    let line = json!({
        "mode": mode,
        "status": status,
        "startedAt": started_at,
        "finishedAt": finished_at,
        "workDir": mode_dir,
        "log": mode_dir.join("run.log"),
        "evidenceKind": if preflight_only { "preflight" } else { "execution" },
        "executed": !preflight_only,
        "command": if preflight_only { "preflight" } else { mode },
        "timedOut": timed_out,
        "timeoutSeconds": mode_timeout_secs,
        "timeoutArtifact": if timed_out { Value::String(mode_dir.join("timeout.json").display().to_string()) } else { Value::Null },
    });
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(
        file,
        "{}",
        serde_json::to_string(&line).map_err(|e| e.to_string())?
    )
    .map_err(|e| e.to_string())
}

// ── Preflight check ───────────────────────────────────────────────────────────
//
// `preflight` validates the local environment before a suite run:
// binary exists and is executable, required tools are in PATH, and the
// artifact directory has enough free space.  Outputs one JSON object per check.

pub(crate) async fn preflight_check() -> Result<Value, String> {
    let restream_bin = default_restream_bin();
    let harness_bin = std::env::current_exe().map_err(|e| e.to_string())?;

    let binary_check = if std::fs::metadata(&restream_bin)
        .map(|m| {
            use std::os::unix::fs::PermissionsExt;
            m.permissions().mode() & 0o111 != 0
        })
        .unwrap_or(false)
    {
        json!({ "check": "binary", "path": restream_bin.display().to_string(), "status": "ok" })
    } else {
        json!({
            "check": "binary",
            "path": restream_bin.display().to_string(),
            "status": "fail",
            "hint": "build restream in target/debug or target/release, or set RESTREAM_BIN"
        })
    };

    let required_tools = ["ffmpeg", "ffprobe", "mediamtx", "curl"];
    let missing: Vec<&str> = required_tools
        .iter()
        .copied()
        .filter(|tool| {
            std::process::Command::new("which")
                .arg(tool)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| !s.success())
                .unwrap_or(true)
        })
        .collect();
    let deps_check = if missing.is_empty() {
        json!({ "check": "deps", "missing": [], "status": "ok" })
    } else {
        json!({ "check": "deps", "missing": missing, "status": "fail" })
    };

    let artifact_root = PathBuf::from(".local/artifacts");
    let min_free_mb: u64 = std::env::var("RESTREAM_ARTIFACT_MIN_FREE_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2048);
    let disk_check = match artifact_disk_free_mb(&artifact_root) {
        Ok(free_mb) => {
            if free_mb >= min_free_mb {
                json!({ "check": "artifact-disk", "freeMb": free_mb, "minFreeMb": min_free_mb, "status": "ok" })
            } else {
                json!({ "check": "artifact-disk", "freeMb": free_mb, "minFreeMb": min_free_mb, "status": "fail",
                         "hint": "prune .local/artifacts or lower RESTREAM_ARTIFACT_MIN_FREE_MB" })
            }
        }
        Err(_) => {
            json!({ "check": "artifact-disk", "status": "skip", "hint": "could not stat artifact directory" })
        }
    };

    let profile_check = if is_bench_profile(&harness_bin) && is_bench_profile(&restream_bin) {
        json!({
            "check": "profile",
            "harness": harness_bin.display().to_string(),
            "restream": restream_bin.display().to_string(),
            "required": "bench",
            "status": "ok"
        })
    } else {
        json!({
            "check": "profile",
            "harness": harness_bin.display().to_string(),
            "restream": restream_bin.display().to_string(),
            "required": "bench",
            "status": "fail",
            "hint": "measurement modes require bench-profile binaries; run `scripts/build/bench-harness.sh` and use `target/bench/test_harness`"
        })
    };

    let all_ok = binary_check["status"] == "ok"
        && deps_check["status"] == "ok"
        && disk_check["status"] != "fail"
        && profile_check["status"] == "ok";

    let result = json!({
        "checks": [binary_check, deps_check, disk_check, profile_check],
        "status": if all_ok { "ok" } else { "fail" },
    });

    if all_ok {
        Ok(result)
    } else {
        Err(format!(
            "preflight failed: {}",
            serde_json::to_string_pretty(&result).unwrap_or_default()
        ))
    }
}

#[cfg(unix)]
fn artifact_disk_free_mb(path: &Path) -> std::io::Result<u64> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "artifact path contains an interior NUL byte",
        )
    })?;
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: c_path is a valid NUL-terminated path and stat points to writable
    // memory for libc to initialize.
    let result = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: statvfs returned success, so the struct was initialized.
    let stat = unsafe { stat.assume_init() };
    Ok(stat.f_bsize * stat.f_bavail / 1_048_576)
}

#[cfg(not(unix))]
fn artifact_disk_free_mb(_path: &Path) -> std::io::Result<u64> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "artifact disk check is only implemented on Unix",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "restream-{label}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    #[test]
    fn suite_evidence_marks_preflight_without_claiming_execution() {
        let root = unique_test_root("suite-evidence");
        let mode_dir = root.join("api-smoke");
        std::fs::create_dir_all(&mode_dir).expect("create suite evidence directory");
        let manifest = root.join("manifest.json");
        let results = root.join("results.jsonl");
        std::fs::File::create(&results).expect("create results file");

        suite_write_manifest(
            &manifest,
            "PASS",
            "2026-01-01T00:00:00Z",
            Some("2026-01-01T00:00:01Z"),
            "preflight-proof",
            &["api-smoke".to_string()],
            &root,
            &results,
            true,
            120,
        )
        .expect("write manifest");
        suite_append_result(
            &results,
            "api-smoke",
            "PASS",
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:01Z",
            &mode_dir,
            true,
            false,
            120,
        )
        .expect("append result");

        let manifest_json: Value =
            serde_json::from_slice(&std::fs::read(&manifest).expect("read suite manifest"))
                .expect("parse suite manifest");
        assert_eq!(manifest_json["evidenceKind"], "preflight");
        assert_eq!(manifest_json["preflightOnly"], true);
        assert_eq!(manifest_json["modeTimeoutSeconds"], 120);

        let result_line = std::fs::read_to_string(&results).expect("read suite result");
        let result_json: Value =
            serde_json::from_str(result_line.trim()).expect("parse suite result");
        assert_eq!(result_json["evidenceKind"], "preflight");
        assert_eq!(result_json["executed"], false);
        assert_eq!(result_json["command"], "preflight");
        assert_eq!(result_json["timedOut"], false);

        std::fs::remove_dir_all(root).expect("remove suite evidence directory");
    }

    #[test]
    fn suite_timeout_is_bounded_and_writes_actionable_artifact() {
        let root = unique_test_root("suite-timeout");
        std::fs::create_dir_all(&root).expect("create timeout evidence directory");

        let outcome = suite_spawn_mode(
            Path::new("/bin/sleep"),
            "timeout-test",
            "30",
            &root,
            false,
            false,
            Duration::from_millis(50),
        )
        .expect("timeout should be a recorded suite outcome");

        assert!(!outcome.exit_ok);
        assert!(outcome.timed_out);
        let artifact: Value = serde_json::from_slice(
            &std::fs::read(root.join("timeout.json")).expect("read timeout artifact"),
        )
        .expect("parse timeout artifact");
        assert_eq!(artifact["status"], "TIMEOUT");
        assert_eq!(artifact["mode"], "timeout-test");
        assert_eq!(artifact["log"], root.join("run.log").display().to_string());
        assert!(
            artifact["hint"]
                .as_str()
                .is_some_and(|hint| hint.contains("run.log"))
        );

        std::fs::remove_dir_all(root).expect("remove timeout evidence directory");
    }
}
