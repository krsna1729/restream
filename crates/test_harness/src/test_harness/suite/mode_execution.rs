use chrono::Utc;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::super::{
    measurement_mode_requires_bench_profile, mode_spec, netns_available, suite_default_modes,
};
use super::process_reporting::{
    suite_append_result, suite_mode_status, suite_spawn_mode, suite_status_line,
    suite_write_manifest,
};

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

struct SuiteModeOutcome {
    mode: String,
    mode_dir: PathBuf,
    started_at: String,
    finished_at: String,
    elapsed: Duration,
    exit_ok: bool,
    suite_timed_out: bool,
    child_reported_timeout: bool,
    timeout_secs: u64,
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
    let started = Instant::now();
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
    let elapsed = started.elapsed();
    let finished_at = Utc::now().to_rfc3339();
    Ok(SuiteModeOutcome {
        mode,
        mode_dir,
        started_at,
        finished_at,
        elapsed,
        exit_ok: exit_ok.exit_ok,
        suite_timed_out: exit_ok.timed_out,
        child_reported_timeout: exit_ok.child_reported_timeout,
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
        let root = work_root.unwrap_or_else(|| cwd.join(".local/artifacts").join(&run_id));
        if root.is_absolute() {
            root
        } else {
            cwd.join(root)
        }
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
                let mode_status = suite_mode_status(
                    outcome.exit_ok,
                    outcome.suite_timed_out,
                    outcome.child_reported_timeout,
                );
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
                    outcome.suite_timed_out,
                    outcome.child_reported_timeout,
                    outcome.timeout_secs,
                    outcome.elapsed,
                )?;
                println!(
                    "{}",
                    suite_status_line(
                        &outcome.mode,
                        mode_status,
                        outcome.elapsed,
                        outcome.suite_timed_out,
                        outcome.child_reported_timeout,
                        outcome.timeout_secs,
                    )
                );
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

            let mode_started_instant = Instant::now();
            let outcome = suite_spawn_mode(
                &exe,
                mode,
                command,
                &mode_dir,
                has_unshare,
                use_host_net,
                mode_timeout,
            )?;
            let mode_status = suite_mode_status(
                outcome.exit_ok,
                outcome.timed_out,
                outcome.child_reported_timeout,
            );
            if !outcome.exit_ok {
                overall_ok = false;
            }

            let mode_elapsed = mode_started_instant.elapsed();
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
                outcome.child_reported_timeout,
                mode_timeout_secs,
                mode_elapsed,
            )?;
            println!(
                "{}",
                suite_status_line(
                    mode,
                    mode_status,
                    mode_elapsed,
                    outcome.timed_out,
                    outcome.child_reported_timeout,
                    mode_timeout_secs,
                )
            );
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
