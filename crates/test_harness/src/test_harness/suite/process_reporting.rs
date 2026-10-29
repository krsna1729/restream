use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

pub(super) struct SuiteSpawnOutcome {
    pub(super) exit_ok: bool,
    pub(super) timed_out: bool,
    pub(super) child_reported_timeout: bool,
}

pub(super) fn suite_spawn_mode(
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
                child_reported_timeout: !status.success()
                    && suite_child_log_reports_timeout(&log_path),
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
                child_reported_timeout: true,
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

pub(super) fn suite_mode_status(
    exit_ok: bool,
    suite_timed_out: bool,
    child_reported_timeout: bool,
) -> &'static str {
    if suite_timed_out || child_reported_timeout {
        "TIMEOUT"
    } else if exit_ok {
        "PASS"
    } else {
        "FAIL"
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn suite_write_manifest(
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
pub(super) fn suite_append_result(
    path: &Path,
    mode: &str,
    status: &str,
    started_at: &str,
    finished_at: &str,
    mode_dir: &Path,
    preflight_only: bool,
    timed_out: bool,
    child_reported_timeout: bool,
    mode_timeout_secs: u64,
    elapsed: Duration,
) -> Result<(), String> {
    let elapsed_seconds = elapsed.as_secs_f64();
    let line = json!({
        "mode": mode,
        "status": status,
        "startedAt": started_at,
        "finishedAt": finished_at,
        "elapsedSeconds": elapsed_seconds,
        "workDir": mode_dir,
        "log": mode_dir.join("run.log"),
        "evidenceKind": if preflight_only { "preflight" } else { "execution" },
        "executed": !preflight_only,
        "command": if preflight_only { "preflight" } else { mode },
        "timedOut": timed_out || child_reported_timeout,
        "suiteTimedOut": timed_out,
        "childReportedTimeout": child_reported_timeout,
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

pub(crate) fn suite_format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs_f64();
    if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else {
        let total_seconds = elapsed.as_secs();
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        format!("{minutes}m{seconds:02}s")
    }
}

pub(super) fn suite_status_line(
    mode: &str,
    status: &str,
    elapsed: Duration,
    suite_timed_out: bool,
    child_reported_timeout: bool,
    timeout_secs: u64,
) -> String {
    if status == "TIMEOUT" {
        let reason = if suite_timed_out {
            "suite limit reached"
        } else if child_reported_timeout {
            "child reported timeout"
        } else {
            "timeout"
        };
        format!(
            "[suite] {mode}: TIMEOUT after {} ({reason}; limit {})",
            suite_format_elapsed(elapsed),
            suite_format_elapsed(Duration::from_secs(timeout_secs))
        )
    } else {
        format!(
            "[suite] {mode}: {status} ({})",
            suite_format_elapsed(elapsed)
        )
    }
}

fn suite_child_log_reports_timeout(log_path: &Path) -> bool {
    let Ok(log) = std::fs::read_to_string(log_path) else {
        return false;
    };
    log.lines().rev().take(80).any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("test harness failed:")
            && (lower.contains("timed out")
                || lower.contains("timeout")
                || lower.contains("did not make progress"))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        suite_append_result, suite_child_log_reports_timeout, suite_spawn_mode, suite_status_line,
        suite_write_manifest,
    };
    use chrono::Utc;
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

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
            false,
            120,
            Duration::from_secs(1),
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
        assert_eq!(result_json["elapsedSeconds"], 1.0);
        assert_eq!(result_json["timedOut"], false);
        assert_eq!(result_json["suiteTimedOut"], false);
        assert_eq!(result_json["childReportedTimeout"], false);

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
        assert!(outcome.child_reported_timeout);
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

    #[test]
    fn suite_child_timeout_failure_is_reported_as_timeout() {
        let root = unique_test_root("suite-child-timeout");
        std::fs::create_dir_all(&root).expect("create child timeout evidence directory");
        std::fs::write(
            root.join("run.log"),
            "test harness failed: signal sink timed out\n",
        )
        .expect("write child timeout log");

        assert!(suite_child_log_reports_timeout(&root.join("run.log")));
        assert_eq!(
            suite_status_line(
                "resource-sweep",
                "TIMEOUT",
                Duration::from_secs(568),
                false,
                true,
                2400,
            ),
            "[suite] resource-sweep: TIMEOUT after 9m28s (child reported timeout; limit 40m00s)"
        );

        std::fs::remove_dir_all(root).expect("remove child timeout evidence directory");
    }
}
