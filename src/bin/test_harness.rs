//! End-to-end integration harness that drives RTMP, SRT, HLS, and API flows
//! against a running restream instance for higher-level verification.

use axum::Router;
use axum::extract::{DefaultBodyLimit, OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, put};
use bytes::Bytes;
use chrono::Utc;
use restream::domain::output_spec::OutputConfig;
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

#[path = "test_harness/catalog.rs"]
#[allow(dead_code)]
mod catalog;
#[path = "test_harness/core.rs"]
mod core;
#[path = "test_harness/fault_manifest.rs"]
mod fault_manifest;
#[path = "test_harness/fault_recovery.rs"]
mod fault_recovery;
#[path = "test_harness/fault_runner.rs"]
mod fault_runner;
#[path = "test_harness/hls_put.rs"]
mod hls_put;
#[path = "test_harness/live_modes.rs"]
mod live_modes;
#[path = "test_harness/media_probes.rs"]
mod media_probes;
#[path = "test_harness/mixed_adaptive_ring.rs"]
mod mixed_adaptive_ring;
#[path = "test_harness/mixed_manifest.rs"]
mod mixed_manifest;
#[path = "test_harness/mixed_runner.rs"]
mod mixed_runner;
#[path = "test_harness/mode_specs.rs"]
mod mode_specs;
#[path = "test_harness/output_progress.rs"]
mod output_progress;
#[path = "test_harness/resource_sweep.rs"]
mod resource_sweep;
#[path = "test_harness/sinks.rs"]
mod sinks;
#[path = "test_harness/workflow_exec.rs"]
mod workflow_exec;

use core::*;
use fault_manifest::*;
use fault_recovery::*;
use fault_runner::*;
use hls_put::*;
use live_modes::*;
use media_probes::*;
pub(crate) use mixed_adaptive_ring::*;
use mixed_manifest::*;
use mixed_runner::*;
pub(crate) use mode_specs::*;
use output_progress::*;
use resource_sweep::*;
use sinks::*;
use workflow_exec::*;

fn mixed_scenario_check_id(scenario: &str, check: &str) -> String {
    format!("{scenario}.{check}")
}

fn mixed_output_check_id(scenario: &str, row_id: &str, check: &str) -> String {
    format!("{scenario}.output.{row_id}.{check}")
}

fn mixed_output_instance_name(scenario: &str, row_id: &str, index: usize) -> String {
    format!("{scenario}-{row_id}-{index}")
}

#[cfg(test)]
fn planned_mixed_stage_count(
    case: MixedInputCase,
    duplicates_per_output: usize,
) -> MixedStageCount {
    use restream::domain::output_spec::OutputConfig;
    use restream::domain::stage::StageKind;
    use restream::domain::state::DesiredOutputState;
    use restream::planner::backend_policy::BackendPolicy;
    use restream::planner::graph_plan::plan_pipeline_graph;
    use restream::types::Output;

    let outputs = mixed_output_cases_for_input(case)
        .iter()
        .flat_map(|output_case| {
            (0..duplicates_per_output).map(move |duplicate| {
                let url = match output_case.protocol() {
                    MixedOutputProtocol::Rtmp => "rtmp://example/live/out",
                    MixedOutputProtocol::Srt => "srt://example:9000?streamid=publish:live/out",
                };
                Output {
                    id: format!("{}-{duplicate}", output_case.id()),
                    pipeline_id: "pipe".to_string(),
                    name: output_case.id().to_string(),
                    url: url.to_string(),
                    monitoring_url: None,
                    desired_state: DesiredOutputState::Running,
                    config: OutputConfig::parse(output_case.encoding()),
                }
            })
        })
        .collect::<Vec<_>>();
    let plan = plan_pipeline_graph(
        "pipe",
        Some(case.expected_video_codec()),
        &outputs,
        false,
        &BackendPolicy::default(),
    );

    let mut counts = MixedStageCount {
        video: 0,
        audio: 0,
        codec_edge: 0,
    };
    for stage in plan.stages {
        match stage.kind {
            StageKind::VideoPreset { .. } => counts.video += 1,
            StageKind::AudioRoute { .. } => counts.audio += 1,
            StageKind::CodecEdge { .. } => counts.codec_edge += 1,
            StageKind::Source
            | StageKind::Hls
            | StageKind::HlsSegmenter { .. }
            | StageKind::Recording
            | StageKind::Preview { .. } => {}
        }
    }
    counts
}

const SINK_PORT: u16 = 12935;

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(harness_runtime_worker_threads())
        .max_blocking_threads(harness_runtime_max_blocking_threads())
        .enable_all()
        .build()
        .expect("create test harness tokio runtime");

    runtime.block_on(async {
        if let Err(error) = maybe_reexec_in_port_namespace() {
            eprintln!("test harness failed: {error}");
            unsafe { libc::_exit(1) };
        }
        if let Err(error) = run().await {
            eprintln!("test harness failed: {error}");
            // Native FFmpeg/libsrt worker threads can still be alive on a failed
            // test. Avoid process-global C teardown while those threads exist.
            unsafe { libc::_exit(1) };
        }
    });
}

fn ensure_loopback() {
    let _ = std::process::Command::new("ip")
        .args(["link", "set", "lo", "up"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

async fn run() -> Result<(), String> {
    ensure_loopback();
    maybe_prune_old_artifacts()?;
    maybe_global_process_cleanup();
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let command = raw.first().cloned().unwrap_or_else(|| "suite".to_string());
    ensure_measurement_profile(&command, &raw[1..])?;
    let result = if command == MIXED_MATRIX_MODE {
        mixed_input_matrix_correctness().await
    } else if command == MIXED_SIGNAL_MODE {
        mixed_signal_correctness().await
    } else if command == MIXED_FAST_BREADTH_MODE {
        mixed_fast_breadth_correctness().await
    } else if let Some(case) = mixed_input_case_for_command(&command) {
        mixed_input_case_correctness(case).await
    } else {
        match command.as_str() {
            "api-smoke" => api_smoke().await,
            "srt.policy" => srt_policy_correctness().await,
            "timestamp.bframe" => bframe_rtmp_correctness().await,
            "ramp-family" => ramp_family_correctness().await,
            "suite" => suite_run().await,
            "preflight" => preflight_check().await,
            "fault.egress-retry" => fault_egress_retry().await,
            "fault.output-stall" => fault_output_stall().await,
            "fault.resilience" => fault_resilience().await,
            "file.live-edge" => file_live_edge().await,
            "signal.control" => signal_control().await,
            "recovery" => recovery().await,
            "resource-sweep" => resource_sweep().await,
            "bitrate-sweep" => bitrate_sweep().await,
            "branch-matrix" => branch_matrix().await,
            "srt-crypto-matrix" => srt_crypto_matrix().await,
            other => Err(unknown_command_error(other)),
        }
    };

    match result {
        Ok(value) => {
            let path = mixed_command_artifact_path(&command)
                .unwrap_or_else(|| artifact_path(&format!("{command}.json")));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap())
                .map_err(|e| e.to_string())?;
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
            println!("artifact={}", path.display());
            // Skip runtime teardown — OS threads holding FFmpeg/SRT C contexts
            // race with global cleanup and cause spurious segfaults on exit.
            // Use _exit to also skip atexit handlers (FFmpeg codec deregistration
            // can deadlock with OS threads).
            unsafe { libc::_exit(0) };
        }
        Err(error) => Err(error),
    }
}

async fn api_smoke() -> Result<Value, String> {
    let work_dir = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("test/artifacts/api-smoke"));
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("api-smoke.sqlite");
    let log_path = work_dir.join("restream.log");
    let ports = TestPorts::from_env();

    // ── First boot: CRUD ────────────────────────────────────────────
    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;
    println!("[api-smoke] authenticated");

    // Health endpoint
    let health = api.get_json("/healthz").await?;
    if health.is_null() {
        return Err("healthz returned null".to_string());
    }
    println!("[api-smoke] healthz ok");

    // Create pipeline
    let pipeline = api
        .post_json(
            "/api/v1/pipelines",
            json!({"name": "smoke-test", "streamKey": "sk-smoke"}),
        )
        .await?;
    let pipeline_id = pipeline["pipeline"]["id"]
        .as_str()
        .ok_or("pipeline create missing id")?
        .to_string();
    println!("[api-smoke] created pipeline {pipeline_id}");

    // Create output
    let output = api
        .post_json(
            &format!("/api/v1/pipelines/{pipeline_id}/outputs"),
            output_create_payload("smoke-out", "rtmp://127.0.0.1:19350/live/nowhere", "source"),
        )
        .await?;
    let output_id = output["output"]["id"]
        .as_str()
        .ok_or("output create missing id")?
        .to_string();
    println!("[api-smoke] created output {output_id}");

    // Read back pipeline list
    let pipelines = api.get_json("/api/v1/pipelines").await?;
    let list = pipelines["pipelines"]
        .as_array()
        .ok_or("pipelines list not an array")?;
    if !list.iter().any(|p| p["id"] == pipeline_id.as_str()) {
        return Err(format!("created pipeline {pipeline_id} not found in list"));
    }
    println!("[api-smoke] pipeline appears in list");

    // Health shows pipeline
    let health = api.get_json("/api/v1/engine/health").await?;
    if health["pipelines"][&pipeline_id].is_null() {
        return Err("pipeline not in health snapshot".to_string());
    }
    println!("[api-smoke] pipeline in health snapshot");

    // ── Restart: DB persistence ─────────────────────────────────────
    stop_child(&mut child).await;
    println!("[api-smoke] stopped first instance");

    let log2_path = work_dir.join("restream-2.log");
    let mut child2 = start_restream_child_opts(
        &restream_bin,
        &ports,
        &db_path,
        &log2_path,
        false,
        None,
        &[],
    )
    .await
    .map_err(|e| format!("restart failed: {e}"))?;
    let mut api2 = RampApi::new(ports.http);
    api2.login().await?;
    println!("[api-smoke] restarted and authenticated");

    let pipelines2 = api2.get_json("/api/v1/pipelines").await?;
    let list2 = pipelines2["pipelines"]
        .as_array()
        .ok_or("pipelines list after restart not an array")?;
    let survived = list2.iter().any(|p| p["id"] == pipeline_id.as_str());
    if !survived {
        stop_child(&mut child2).await;
        return Err(format!("pipeline {pipeline_id} did not survive restart"));
    }
    println!("[api-smoke] pipeline survived restart (DB persistence confirmed)");

    let history_contract = verify_api_smoke_history_contract(&api2).await?;
    println!("[api-smoke] history contract verified");

    // Cleanup
    stop_child(&mut child2).await;

    Ok(json!({
        "passed": true,
        "mode": "api-smoke",
        "pipelineId": pipeline_id,
        "outputId": output_id,
        "dbPersistence": survived,
        "historyContract": history_contract,
    }))
}

const CHILD_TERMINATION_TIMEOUT: Duration = Duration::from_secs(5);

async fn kill_and_wait_child(child: &mut Child, label: &str) -> Result<ExitStatus, String> {
    let pid = child
        .id()
        .map(|id| id.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    if let Some(status) = child
        .try_wait()
        .map_err(|e| format!("{label} pid {pid}: failed to check child status before kill: {e}"))?
    {
        return Ok(status);
    }

    if let Err(error) = child.start_kill() {
        if let Some(status) = child.try_wait().map_err(|e| {
            format!("{label} pid {pid}: failed to check child status after kill error: {e}")
        })? {
            return Ok(status);
        }
        return Err(format!("{label} pid {pid}: failed to send kill: {error}"));
    }

    tokio::time::timeout(CHILD_TERMINATION_TIMEOUT, child.wait())
        .await
        .map_err(|_| {
            format!("{label} pid {pid}: timed out waiting for child exit after kill signal")
        })?
        .map_err(|e| format!("{label} pid {pid}: failed to wait after kill: {e}"))
}

async fn stop_child(child: &mut Child) {
    if let Err(error) = kill_and_wait_child(child, "harness child").await {
        eprintln!("test harness cleanup warning: {error}");
    }
}

fn suite_mode_is_parallelizable(mode: &str, preflight_only: bool) -> bool {
    !preflight_only && !measurement_mode_requires_bench_profile(mode)
}

/// Result summary for one child mode launched by the aggregate suite runner.
struct SuiteModeOutcome {
    index: usize,
    mode: String,
    mode_dir: PathBuf,
    started_at: String,
    finished_at: String,
    exit_ok: bool,
}

async fn suite_run_mode(
    exe: PathBuf,
    mode: String,
    mode_dir: PathBuf,
    command: String,
    has_unshare: bool,
    use_host_net: bool,
    index: usize,
) -> Result<SuiteModeOutcome, String> {
    let started_at = Utc::now().to_rfc3339();
    let spawn_mode_dir = mode_dir.clone();
    let exit_ok = tokio::task::spawn_blocking(move || {
        suite_spawn_mode(&exe, &command, &spawn_mode_dir, has_unshare, use_host_net)
    })
    .await
    .map_err(|e| format!("suite worker join failed for {mode}: {e}"))??;
    let finished_at = Utc::now().to_rfc3339();
    Ok(SuiteModeOutcome {
        index,
        mode,
        mode_dir,
        started_at,
        finished_at,
        exit_ok,
    })
}

async fn suite_run_parallel_batch(
    exe: &Path,
    modes: &[String],
    work_root: &Path,
    preflight_only: bool,
    has_unshare: bool,
    use_host_net: bool,
) -> Result<Vec<SuiteModeOutcome>, String> {
    let mut join_set = tokio::task::JoinSet::new();
    for (offset, mode) in modes.iter().enumerate() {
        let mode_dir = work_root.join(mode);
        std::fs::create_dir_all(&mode_dir).map_err(|e| e.to_string())?;
        let command = if preflight_only {
            "preflight".to_string()
        } else {
            mode.clone()
        };
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
            offset,
        ));
    }

    let mut outcomes: Vec<Option<SuiteModeOutcome>> = (0..modes.len()).map(|_| None).collect();
    while let Some(result) = join_set.join_next().await {
        let outcome = result.map_err(|e| format!("suite batch join failed: {e}"))??;
        let index = outcome.index;
        outcomes[index] = Some(outcome);
    }

    outcomes
        .into_iter()
        .map(|outcome| outcome.ok_or("suite batch produced an empty result slot".to_string()))
        .collect()
}

async fn suite_run() -> Result<Value, String> {
    let raw: Vec<String> = std::env::args().skip(2).collect();
    let mut modes = suite_default_modes();
    let mut continue_on_fail = false;
    let mut preflight_only = false;
    let mut use_host_net = std::env::var("TEST_HARNESS_USE_HOST_NET")
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let mut run_id = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut work_root: Option<PathBuf> = std::env::var_os("WORK_ROOT").map(PathBuf::from);

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

    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let work_root = {
        let r = work_root.unwrap_or_else(|| cwd.join("test/artifacts").join(&run_id));
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
            )
            .await?;
            for outcome in outcomes {
                let mode_status = if outcome.exit_ok { "PASS" } else { "FAIL" };
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

            let exit_ok = suite_spawn_mode(&exe, command, &mode_dir, has_unshare, use_host_net)?;
            let mode_status = if exit_ok { "PASS" } else { "FAIL" };
            if !exit_ok {
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
    command: &str,
    mode_dir: &Path,
    has_unshare: bool,
    use_host_net: bool,
) -> Result<bool, String> {
    let log_path = mode_dir.join("run.log");
    let log_file = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let log_copy = log_file.try_clone().map_err(|e| e.to_string())?;

    let status = if has_unshare {
        std::process::Command::new("unshare")
            .args(["--net", "--user", "--map-root-user"])
            .arg(exe)
            .arg(command)
            .env("WORK_DIR", mode_dir)
            .env("RESTREAM_HARNESS_IN_NETNS", "1")
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_copy))
            .status()
            .map_err(|e| format!("failed to spawn {command}: {e}"))?
    } else {
        let mut child = std::process::Command::new(exe);
        child
            .arg(command)
            .env("WORK_DIR", mode_dir)
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_copy));
        if use_host_net {
            child.env("TEST_HARNESS_USE_HOST_NET", "1");
        }
        child
            .status()
            .map_err(|e| format!("failed to spawn {command}: {e}"))?
    };
    Ok(status.success())
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
    });
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn suite_append_result(
    path: &Path,
    mode: &str,
    status: &str,
    started_at: &str,
    finished_at: &str,
    mode_dir: &Path,
) -> Result<(), String> {
    let line = json!({
        "mode": mode,
        "status": status,
        "startedAt": started_at,
        "finishedAt": finished_at,
        "workDir": mode_dir,
        "log": mode_dir.join("run.log"),
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

async fn preflight_check() -> Result<Value, String> {
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

    let artifact_root = PathBuf::from("test/artifacts");
    let min_free_mb: u64 = std::env::var("RESTREAM_ARTIFACT_MIN_FREE_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2048);
    let disk_check = match nix::sys::statvfs::statvfs(&artifact_root) {
        Ok(stat) => {
            let free_mb = stat.block_size() * stat.blocks_available() / 1_048_576;
            if free_mb >= min_free_mb {
                json!({ "check": "artifact-disk", "freeMb": free_mb, "minFreeMb": min_free_mb, "status": "ok" })
            } else {
                json!({ "check": "artifact-disk", "freeMb": free_mb, "minFreeMb": min_free_mb, "status": "fail",
                         "hint": "prune test/artifacts or lower RESTREAM_ARTIFACT_MIN_FREE_MB" })
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
            "hint": "measurement modes require bench-profile binaries; run `scripts/build-bench-harness.sh` and use `target/bench/test_harness`"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_work_db_path_stays_under_work_dir() {
        let work_dir = Path::new("test/artifacts/example");
        assert_eq!(
            default_work_db_path(work_dir, "suite.db"),
            work_dir.join("suite.db")
        );
    }

    #[test]
    fn harness_source_does_not_use_repo_root_data_db_fallback() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness.rs"
        ));
        assert!(
            !source.contains("PathBuf::from(\"data.db\")"),
            "harness modes must keep mutable DB state under WORK_DIR"
        );
    }

    #[test]
    fn strip_netns_opt_removes_only_the_opt_out_flag() {
        let raw = vec![
            "bitrate-sweep".to_string(),
            "--no-netns".to_string(),
            "--work-root".to_string(),
            "test/artifacts/example".to_string(),
        ];
        assert_eq!(
            strip_netns_opt(&raw),
            vec![
                "bitrate-sweep".to_string(),
                "--work-root".to_string(),
                "test/artifacts/example".to_string(),
            ]
        );
    }

    #[test]
    fn only_non_measurement_modes_parallelize_in_suite() {
        assert!(suite_mode_is_parallelizable("srt.policy", false));
        assert!(suite_mode_is_parallelizable("fault.egress-retry", false));
        assert!(suite_mode_is_parallelizable("fault.output-stall", false));
        assert!(suite_mode_is_parallelizable("fault.resilience", false));
        assert!(suite_mode_is_parallelizable("recovery", false));
        assert!(!suite_mode_is_parallelizable("bitrate-sweep", false));
        assert!(!suite_mode_is_parallelizable("preflight", true));
    }

    #[test]
    fn fault_output_stall_sibling_count_honors_n_per_group_cap() {
        assert_eq!(effective_fault_output_stall_siblings(12, None), 12);
        assert_eq!(effective_fault_output_stall_siblings(12, Some(1)), 1);
        assert_eq!(effective_fault_output_stall_siblings(4, Some(8)), 4);
        assert_eq!(effective_fault_output_stall_siblings(0, Some(0)), 1);
    }

    #[test]
    fn synthesized_harness_ports_are_high_and_distinct() {
        let mut reserved = HashSet::new();
        let http = env_or_allocated_port("RESTREAM_HTTP", 3030, &mut reserved);
        let rtmp = env_or_allocated_port("RESTREAM_RTMP", 1935, &mut reserved);
        let srt = env_or_allocated_port("RESTREAM_SRT", 10080, &mut reserved);
        let mtx_api = env_or_allocated_port("MTX_API", 9997, &mut reserved);
        let unique: HashSet<u16> = [http, rtmp, srt, mtx_api].into_iter().collect();

        assert_eq!(unique.len(), 4);
        assert!(unique.iter().all(|port| *port >= 20_000));
    }

    #[test]
    fn synthesized_harness_port_ranges_do_not_overlap() {
        let mut reserved = HashSet::new();
        let sink = env_or_allocated_port_range("SINK_PORT", SINK_PORT, 256, &mut reserved);
        let hls_put = env_or_allocated_port_range("HLS_PUT_PORT", 8990, 16, &mut reserved);
        let ffmpeg_srt =
            env_or_allocated_port_range("FFMPEG_SRT_SINK_BASE", 15_000, 1024, &mut reserved);
        let ffmpeg_signal =
            env_or_allocated_port_range("FFMPEG_SIGNAL_SINK_BASE", 16_000, 1024, &mut reserved);

        let sink_end = sink as u32 + 255;
        let hls_put_end = hls_put as u32 + 15;
        let ffmpeg_srt_end = ffmpeg_srt as u32 + 1023;
        let ffmpeg_signal_end = ffmpeg_signal as u32 + 1023;

        assert!(sink >= 20_000);
        assert!(hls_put >= 20_000);
        assert!(ffmpeg_srt >= 20_000);
        assert!(ffmpeg_signal >= 20_000);
        assert!(sink_end < hls_put as u32 || hls_put_end < sink as u32);
        assert!(sink_end < ffmpeg_srt as u32 || ffmpeg_srt_end < sink as u32);
        assert!(sink_end < ffmpeg_signal as u32 || ffmpeg_signal_end < sink as u32);
        assert!(hls_put_end < ffmpeg_srt as u32 || ffmpeg_srt_end < hls_put as u32);
        assert!(hls_put_end < ffmpeg_signal as u32 || ffmpeg_signal_end < hls_put as u32);
        assert!(ffmpeg_srt_end < ffmpeg_signal as u32 || ffmpeg_signal_end < ffmpeg_srt as u32);
    }

    #[test]
    fn parse_log_fields_handles_json_string_payloads() {
        let log = json!({
            "fields": r#"{"correlation_id":"out-0001","phase":"connect"}"#
        });

        let fields = parse_log_fields(&log).expect("parsed fields");
        assert_eq!(fields["correlation_id"], "out-0001");
        assert_eq!(fields["phase"], "connect");
    }

    #[test]
    fn generalized_sink_rejects_equal_video_dts() {
        let metrics = GeneralizedSinkMetrics::default();
        metrics.packets.lock().unwrap().extend([
            SinkPacket {
                media_type: "video",
                timestamp_ms: 10,
                audio_packet_type: None,
                audio_has_adts_sync: false,
                video_is_sequence_header: false,
            },
            SinkPacket {
                media_type: "video",
                timestamp_ms: 10,
                audio_packet_type: None,
                audio_has_adts_sync: false,
                video_is_sequence_header: false,
            },
        ]);

        assert!(
            !metrics.dts_monotone(),
            "FFmpeg rejects equal DTS as non-monotonic; harness must too"
        );
    }

    #[test]
    fn generalized_sink_ignores_video_sequence_headers_for_dts() {
        let metrics = GeneralizedSinkMetrics::default();
        metrics.packets.lock().unwrap().extend([
            SinkPacket {
                media_type: "video",
                timestamp_ms: 10,
                audio_packet_type: None,
                audio_has_adts_sync: false,
                video_is_sequence_header: false,
            },
            SinkPacket {
                media_type: "video",
                timestamp_ms: 10,
                audio_packet_type: None,
                audio_has_adts_sync: false,
                video_is_sequence_header: true,
            },
            SinkPacket {
                media_type: "video",
                timestamp_ms: 11,
                audio_packet_type: None,
                audio_has_adts_sync: false,
                video_is_sequence_header: false,
            },
        ]);

        assert!(metrics.dts_monotone());
    }

    #[test]
    fn ffprobe_compact_validator_accepts_reordered_packet_dump() {
        let log = "\
packet|stream_index=1|pts_time=10.021333|dts_time=10.021333\n\
packet|stream_index=0|pts_time=10.100000|dts_time=10.100000\n\
packet|stream_index=1|pts_time=10.000000|dts_time=10.000000\n\
stream|index=0|codec_type=video|width=1920|height=1080\n\
stream|index=1|codec_type=audio\n";

        assert_eq!(
            ffprobe_compact_video_dimensions(log).as_deref(),
            Some("1920x1080")
        );
        assert_eq!(ffprobe_compact_audio_track_count(log), 1);
        assert_eq!(ffprobe_compact_validate_dts(log), Ok(3));
    }

    #[test]
    fn ffprobe_compact_validator_rejects_duplicate_dts() {
        let log = "\
packet|stream_index=1|pts_time=10.000000|dts_time=10.000000\n\
packet|stream_index=1|pts_time=10.000000|dts_time=10.000000\n\
stream|index=1|codec_type=audio\n";

        let error = ffprobe_compact_validate_dts(log).expect_err("duplicate DTS must fail");
        assert!(error.contains("duplicate DTS"));
    }

    #[test]
    fn ffprobe_compact_validator_rejects_large_dts_gap() {
        let log = "\
packet|stream_index=1|pts_time=10.000000|dts_time=10.000000\n\
packet|stream_index=1|pts_time=11.000000|dts_time=11.000000\n\
stream|index=1|codec_type=audio\n";

        let error = ffprobe_compact_validate_dts(log).expect_err("large DTS gap must fail");
        assert!(error.contains("DTS gap"));
    }

    #[test]
    fn decode_scan_video_dts_fallback_applies_to_rtmp_and_srt_muxer_warnings() {
        assert!(decode_scan_needs_video_dts_fallback(
            "rtmp://127.0.0.1/live/test",
            Some(0),
            Some("non monoton"),
        ));
        assert!(decode_scan_needs_video_dts_fallback(
            "rtmp://127.0.0.1/live/test",
            Some(0),
            Some("non-monoton"),
        ));
        assert!(decode_scan_needs_video_dts_fallback(
            "srt://127.0.0.1:9999?streamid=read:live/test",
            Some(0),
            Some("non monoton"),
        ));
        assert!(!decode_scan_needs_video_dts_fallback(
            "rtmp://127.0.0.1/live/test",
            Some(1),
            Some("non monoton"),
        ));
        assert!(!decode_scan_needs_video_dts_fallback(
            "rtmp://127.0.0.1/live/test",
            Some(0),
            Some("invalid data"),
        ));
        assert!(!decode_scan_needs_video_dts_fallback(
            "http://127.0.0.1/live/test",
            Some(0),
            Some("non monoton"),
        ));
    }

    #[test]
    fn marker_gap_parser_extracts_flash_and_beep_times() {
        let black = "\
[blackdetect @ 0x1] black_start:0 black_end:2 black_duration:2\n\
[blackdetect @ 0x1] black_start:2.2 black_end:7 black_duration:4.8\n\
[blackdetect @ 0x1] black_start:7.2 black_end:12 black_duration:4.8\n\
[blackdetect @ 0x1] black_start:12.2 black_end:17 black_duration:4.8\n\
[blackdetect @ 0x1] black_start:17.2 black_end:20 black_duration:2.8\n";
        let silence = "\
[silencedetect @ 0x1] silence_start: 0\n\
[silencedetect @ 0x1] silence_end: 2.02 | silence_duration: 2.02\n\
[silencedetect @ 0x1] silence_start: 2.22\n\
[silencedetect @ 0x1] silence_end: 7.02 | silence_duration: 4.8\n\
[silencedetect @ 0x1] silence_start: 7.22\n\
[silencedetect @ 0x1] silence_end: 12.02 | silence_duration: 4.8\n\
[silencedetect @ 0x1] silence_start: 12.22\n\
[silencedetect @ 0x1] silence_end: 17.02 | silence_duration: 4.8\n\
[silencedetect @ 0x1] silence_start: 17.22\n";

        let video = marker_gaps_from_intervals(&parse_blackdetect_intervals(black));
        let audio = marker_gaps_from_intervals(&parse_silencedetect_intervals(silence));

        assert_eq!(video.len(), 4);
        assert_eq!(audio.len(), 3);
        assert!((video[0] - 2.1).abs() < 0.001);
        assert!((audio[0] - 2.12).abs() < 0.001);
    }

    #[test]
    fn signal_quality_rejects_marker_drift() {
        let black = "\
[blackdetect @ 0x1] black_start:0 black_end:2 black_duration:2\n\
[blackdetect @ 0x1] black_start:2.2 black_end:7 black_duration:4.8\n\
[blackdetect @ 0x1] black_start:7.2 black_end:12 black_duration:4.8\n\
[blackdetect @ 0x1] black_start:12.2 black_end:17 black_duration:4.8\n\
[blackdetect @ 0x1] black_start:17.2 black_end:20 black_duration:2.8\n";
        let silence = "\
[silencedetect @ 0x1] silence_start: 0\n\
[silencedetect @ 0x1] silence_end: 2.02 | silence_duration: 2.02\n\
[silencedetect @ 0x1] silence_start: 2.22\n\
[silencedetect @ 0x1] silence_end: 7.20 | silence_duration: 4.98\n\
[silencedetect @ 0x1] silence_start: 7.40\n\
[silencedetect @ 0x1] silence_end: 12.45 | silence_duration: 5.05\n\
[silencedetect @ 0x1] silence_start: 12.65\n\
[silencedetect @ 0x1] silence_end: 17.80 | silence_duration: 5.15\n\
[silencedetect @ 0x1] silence_start: 18.00\n";
        let ashow = "\
[Parsed_ashowinfo_0 @ 0x1] n:0 pts_time:0\n\
[Parsed_ashowinfo_0 @ 0x1] n:1 pts_time:0.021333\n\
[Parsed_ashowinfo_0 @ 0x1] n:2 pts_time:0.042666\n";
        let pcm = PcmQualityReport {
            samples: 1024,
            clipping_samples: 0,
            max_step: 100,
            rms: 10.0,
        };

        let error = validate_signal_quality(black, silence, ashow, "", pcm)
            .expect_err("marker drift must fail");
        assert!(error.contains("drift") || error.contains("offset"));
    }

    #[test]
    fn nearest_marker_pairing_tolerates_live_capture_starting_mid_cycle() {
        let video = vec![6.4125, 11.4125, 16.4125];
        let audio = vec![1.396, 6.396, 11.396, 16.396];

        let offsets = nearest_marker_offsets_ms(&video, &audio, 1000.0);

        assert_eq!(offsets.len(), 3);
        assert!(offsets.iter().all(|offset| offset.abs() < 25.0));
    }

    #[test]
    fn audio_pts_gap_uses_median_frame_delta() {
        let ashow = "\
[Parsed_ashowinfo_0 @ 0x1] n:0 pts_time:0\n\
[Parsed_ashowinfo_0 @ 0x1] n:1 pts_time:0.021333\n\
[Parsed_ashowinfo_0 @ 0x1] n:2 pts_time:0.042666\n\
[Parsed_ashowinfo_0 @ 0x1] n:3 pts_time:0.200000\n";

        assert!(max_audio_pts_gap_ms(ashow) > 100.0);
    }

    #[test]
    fn pcm_quality_detects_clipping_and_impulses() {
        let mut bytes = Vec::new();
        for sample in [0i16, 10, -10, 32767, -32768] {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }

        let report = analyze_pcm_s16le(&bytes);

        assert_eq!(report.samples, 5);
        assert_eq!(report.clipping_samples, 2);
        assert!(report.max_step > 30_000);
    }

    #[test]
    fn log_has_correlation_id_detects_both_field_spellings() {
        let snake = json!({
            "fields": r#"{"correlation_id":"out-0001"}"#
        });
        let camel = json!({
            "fields": r#"{"correlationId":"stage-0002"}"#
        });
        let none = json!({
            "fields": r#"{"phase":"connect"}"#
        });

        assert!(log_has_correlation_id(&snake));
        assert!(log_has_correlation_id(&camel));
        assert!(!log_has_correlation_id(&none));
    }

    #[test]
    fn proc_net_has_listening_port_matches_ipv4_listener_entries() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:4C4F 00000000:0000 0A 00000000:00000000 00:00000000 00000000   100        0 1 1 0000000000000000 100 0 0 10 0\n";

        assert!(proc_net_has_listening_port(table, 19535));
        assert!(!proc_net_has_listening_port(table, 1935));
    }

    #[test]
    fn proc_net_has_listening_port_ignores_non_listen_states() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:078F 0100007F:9C40 01 00000000:00000000 00:00000000 00000000   100        0 1 1 0000000000000000 100 0 0 10 0\n";

        assert!(!proc_net_has_listening_port(table, 1935));
    }

    #[test]
    fn kill_and_wait_child_terminates_spawned_process() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("create tokio runtime");

        runtime.block_on(async {
            let mut child = Command::new("sleep")
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("spawn long-running child process");

            let started = Instant::now();
            let status = kill_and_wait_child(&mut child, "unit-test child")
                .await
                .expect("kill_and_wait_child should terminate child");
            let elapsed = started.elapsed();

            assert!(
                elapsed < Duration::from_secs(2),
                "kill_and_wait_child should terminate quickly, elapsed {elapsed:?}"
            );
            assert!(
                !status.success(),
                "killed process should not report a success exit status"
            );
        });
    }

    #[test]
    fn unknown_command_error_lists_every_supported_mode() {
        let message = unknown_command_error("nope-mode");
        assert!(message.contains("\"nope-mode\""));
        assert!(message.contains("suite"));
        assert!(message.contains("preflight"));
        for mode in supported_mode_names() {
            assert!(
                message.contains(mode.as_str()),
                "unknown-command help text is missing mode {mode}"
            );
        }
    }

    #[test]
    fn every_mode_spec_has_dispatch_arm() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness.rs"
        ));
        for spec in all_mode_specs() {
            if spec.name == MIXED_MATRIX_MODE
                || spec.name == MIXED_SIGNAL_MODE
                || spec.name == MIXED_FAST_BREADTH_MODE
                || spec.name == "generic-sweeps"
                || mixed_input_case_for_command(&spec.name).is_some()
            {
                // "generic-sweeps" is a manifest-only suite-composition entry
                // (folded into "mixed"); it has no standalone Rust runner.
                continue;
            }
            let arm = format!("\"{}\" =>", spec.name);
            assert!(
                source.contains(&arm),
                "mode {} is missing a run() dispatch arm",
                spec.name
            );
        }
    }

    #[test]
    fn mixed_input_matrix_names_are_explicit_and_supported() {
        let names: Vec<_> = mixed_input_cases()
            .iter()
            .map(|case| case.scenario_id())
            .collect();
        assert_eq!(
            names,
            vec![
                "mixed.asset.file.h264.a1.bf0",
                "mixed.asset.file.h264.a1.bf2",
                "mixed.asset.file.h264.a2.bf0",
                "mixed.asset.file.h264.a2.bf2",
                "mixed.asset.file.h265.a1.bf0",
                "mixed.asset.file.h265.a1.bf2",
                "mixed.asset.file.h265.a2.bf0",
                "mixed.asset.file.h265.a2.bf2",
                "mixed.live.rtmp.h264.a1.bf0",
                "mixed.live.rtmp.h264.a1.bf2",
                "mixed.live.srt.h264.a1.bf0",
                "mixed.live.srt.h264.a1.bf2",
                "mixed.live.srt.h264.a2.bf0",
                "mixed.live.srt.h264.a2.bf2",
                "mixed.live.srt.h265.a1.bf0",
                "mixed.live.srt.h265.a1.bf2",
                "mixed.live.srt.h265.a2.bf0",
                "mixed.live.srt.h265.a2.bf2",
            ]
        );
        for case in mixed_input_cases() {
            let mode = mixed_input_mode_name(*case);
            assert_eq!(mixed_input_case_for_command(&mode), Some(*case));
            assert!(
                mode_spec(&mode).is_some(),
                "{mode} must be listed in harness help/suite specs"
            );
        }
    }

    #[test]
    fn mixed_fast_breadth_is_small_but_axis_rich() {
        let names: Vec<_> = mixed_fast_breadth_cases()
            .iter()
            .map(|selected| selected.case.scenario_id())
            .collect();
        assert_eq!(
            names,
            vec![
                "mixed.asset.file.h264.a1.bf0",
                "mixed.asset.file.h265.a2.bf2",
                "mixed.live.rtmp.h264.a1.bf0",
                "mixed.live.rtmp.h264.a1.bf2",
                "mixed.live.srt.h264.a2.bf0",
                "mixed.live.srt.h265.a2.bf2",
            ]
        );

        let cases: Vec<_> = mixed_fast_breadth_cases()
            .iter()
            .map(|selected| selected.case)
            .collect();
        for protocol in [
            MixedInputProtocol::File,
            MixedInputProtocol::Rtmp,
            MixedInputProtocol::Srt,
        ] {
            assert!(
                cases.iter().any(|case| case.protocol() == protocol),
                "fast breadth must cover input protocol {protocol:?}"
            );
        }
        for codec in [MixedVideoCodec::H264, MixedVideoCodec::H265] {
            assert!(
                cases.iter().any(|case| case.codec() == codec),
                "fast breadth must cover codec {codec:?}"
            );
        }
        for audio in [MixedInputAudioLayout::A1, MixedInputAudioLayout::A2] {
            assert!(
                cases.iter().any(|case| case.audio_layout() == audio),
                "fast breadth must cover audio layout {audio:?}"
            );
        }
        for reorder in [MixedInputReorder::Bf0, MixedInputReorder::Bf2] {
            assert!(
                cases.iter().any(|case| case.reorder() == reorder),
                "fast breadth must cover reorder mode {reorder:?}"
            );
        }
        for reorder in [MixedInputReorder::Bf0, MixedInputReorder::Bf2] {
            assert!(
                cases.iter().any(|case| {
                    case.protocol() == MixedInputProtocol::Rtmp && case.reorder() == reorder
                }),
                "fast breadth must cover RTMP sender reorder mode {reorder:?}"
            );
        }
        for selected in mixed_fast_breadth_cases() {
            assert!(
                !selected.checks.contains(&MixedCheck::Recording)
                    && !selected.checks.contains(&MixedCheck::Load),
                "{} should keep fast-breadth checks short; use env overrides for depth",
                selected.case.scenario_id()
            );
        }
        assert_eq!(
            mixed_fast_breadth_cases()
                .iter()
                .filter(|selected| selected.checks.contains(&MixedCheck::Signal))
                .count(),
            1,
            "signal quality should be sampled on exactly one sentinel fast-breadth row"
        );
        assert!(
            mixed_fast_breadth_cases().iter().any(|selected| {
                selected.case.scenario_id() == "mixed.live.rtmp.h264.a1.bf0"
                    && selected.checks.contains(&MixedCheck::Signal)
            }),
            "RTMP H.264 BF0 should stay the signal-quality sentinel row"
        );
        assert_eq!(
            mixed_fast_breadth_cases()
                .iter()
                .filter(|selected| selected.checks.contains(&MixedCheck::Hls))
                .count(),
            2,
            "HLS is sampled on representative H.264 and HEVC rows, not every row"
        );

        let selected_cells: usize = cases
            .iter()
            .map(|case| mixed_output_cases_for_input(*case).len())
            .sum();
        let total_cells: usize = mixed_input_cases()
            .iter()
            .map(|case| mixed_output_cases_for_input(*case).len())
            .sum();
        assert_eq!(selected_cells, 63);
        assert_eq!(total_cells, 180);
        assert!(
            selected_cells < total_cells / 2,
            "fast breadth should stay quick enough to run before the exhaustive matrix"
        );
    }

    #[test]
    fn mixed_fast_breadth_batches_reuse_three_shared_stacks() {
        assert_eq!(mixed_fast_breadth_batches().len(), 3);
        assert_eq!(
            mixed_fast_breadth_batches()
                .iter()
                .map(|batch| batch.group.as_str())
                .collect::<Vec<_>>(),
            vec!["live-rtmp", "live-srt", "file-ingest"]
        );
        for batch in mixed_fast_breadth_batches() {
            assert!(
                !batch.cases.is_empty() && batch.cases.len() <= 2,
                "{} should stay within the two-pipeline shared-stack target",
                batch.group.as_str()
            );
            for case in &batch.cases {
                assert_eq!(
                    case.shared_batch_group(),
                    batch.group,
                    "{} should be packed into its matching shared stack family",
                    case.scenario_id()
                );
                mixed_fast_breadth_selected(*case);
            }
        }
    }

    #[test]
    fn mixed_fast_breadth_group_parser_accepts_known_groups_once() {
        assert_eq!(
            parse_mixed_fast_breadth_groups("live-srt, live-rtmp, live-srt").unwrap(),
            vec![
                MixedSharedBatchGroup::LiveSrt,
                MixedSharedBatchGroup::LiveRtmp
            ]
        );
    }

    #[test]
    fn mixed_fast_breadth_group_parser_rejects_unknown_groups() {
        let error = parse_mixed_fast_breadth_groups("live-srt,nope").unwrap_err();
        assert!(error.contains("unknown MIXED_FAST_BREADTH_GROUPS entry 'nope'"));
    }

    #[test]
    fn mixed_fast_breadth_defaults_collect_failures_for_failure_mapping() {
        let root_source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness.rs"
        ));
        let mixed_source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/mixed_runner.rs"
        ));
        let source = format!("{root_source}\n{mixed_source}");

        assert!(
            source.contains("env.collect_failures = true"),
            "mixed.fast-breadth should continue through selected rows to map failures"
        );
        assert!(
            source.contains("\"defaultCollectFailures\""),
            "mixed.fast-breadth result metadata should disclose the collection default"
        );
        assert!(
            source.contains("root.join(\"assertions.jsonl\")"),
            "mixed.fast-breadth should emit machine-readable assertion rows by default"
        );
    }

    #[test]
    fn mixed_matrix_defaults_to_shared_batch_execution() {
        let mixed_source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/mixed_runner.rs"
        ));

        assert!(
            mixed_source.contains("mixed_input_matrix_correctness_shared().await"),
            "mixed.matrix should default to the shared-batch matrix path"
        );
        assert!(
            mixed_source.contains("\"execution\": \"shared-batch\""),
            "mixed.matrix result metadata should report shared-batch execution"
        );
        assert!(
            mixed_source.contains("\"sharedBatches\""),
            "mixed.matrix metadata should report shared batch group coverage"
        );
    }

    #[test]
    fn mixed_matrix_serial_opt_out_stays_explicit() {
        let mixed_source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/mixed_runner.rs"
        ));

        assert!(
            mixed_source.contains("MIXED_MATRIX_SERIAL"),
            "mixed.matrix should expose explicit serial opt-out env"
        );
        assert!(
            mixed_source.contains("mixed_input_matrix_correctness_serial().await"),
            "mixed.matrix should keep the serial fallback path for bisecting"
        );
        assert!(
            mixed_source.contains("\"execution\": \"serial\""),
            "mixed.matrix serial fallback should report serial execution metadata"
        );
    }

    #[test]
    fn mixed_signal_group_parser_rejects_unknown_groups() {
        let error = parse_mixed_signal_groups("live-rtmp,nope").unwrap_err();
        assert!(error.contains("unknown MIXED_SIGNAL_GROUPS entry 'nope'"));
    }

    #[test]
    fn mixed_signal_defaults_to_shared_batch_execution() {
        let mixed_source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/mixed_runner.rs"
        ));

        assert!(
            mixed_source.contains("mixed_signal_correctness"),
            "mixed.signal should route through its shared-batch runner"
        );
        assert!(
            mixed_source.contains("\"mode\": MIXED_SIGNAL_MODE"),
            "mixed.signal result metadata should report its mode"
        );
        assert!(
            mixed_source.contains("\"signalRationale\""),
            "mixed.signal results should disclose why each sentinel case exists"
        );
        assert!(
            mixed_source.contains("\"sharedBatches\""),
            "mixed.signal coverage should report shared batch group coverage"
        );
        assert!(
            mixed_source.contains("root.join(\"assertions.jsonl\")"),
            "mixed.signal should emit machine-readable assertion rows by default"
        );
    }

    #[test]
    fn mixed_shared_batches_delete_finished_pipelines() {
        let mixed_source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/mixed_runner.rs"
        ));

        assert!(
            mixed_source.contains("delete_pipeline_v1(api, pipeline_id).await?"),
            "shared mixed cases should delete finished pipelines so later waves do not accumulate dead state"
        );
        assert!(
            mixed_source.contains("\"scenario.pipeline_cleanup\""),
            "pipeline cleanup should emit timing so post-run analysis can confirm the amortization step"
        );
        assert!(
            mixed_source.contains("config[\"pipelineDeleted\"] = json!(true);"),
            "mixed case results should disclose successful pipeline cleanup"
        );
    }

    #[test]
    fn mixed_matrix_defaults_exclude_signal_and_continue_on_failure() {
        let mixed_source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bin/test_harness/mixed_runner.rs"
        ));

        assert!(
            mixed_source.contains("mixed_matrix_default_check_names"),
            "mixed.matrix should derive default checks from manifest"
        );
        assert!(
            mixed_source.contains("continueOnScenarioFailure"),
            "mixed.matrix metadata should disclose continue-on-failure behavior"
        );
        assert!(
            mixed_source.contains("MIXED_MATRIX_FAIL_FAST"),
            "mixed.matrix should expose fail-fast env opt-out"
        );
        assert!(
            mixed_source.contains("\"failures\": failures"),
            "mixed.matrix should aggregate per-scenario failures in final report"
        );
        assert!(
            !mixed_default_checks().contains(&MixedCheck::Signal),
            "mixed.matrix default checks should leave signal validation to mixed.signal"
        );
        assert!(
            !mixed_default_checks().contains(&MixedCheck::SoakDrift),
            "mixed.matrix default checks should leave soak drift with signal validation"
        );
    }

    #[test]
    fn mixed_output_progress_gate_only_applies_to_external_read_checks() {
        assert!(mixed_output_checks_need_live_progress_gate(None));
        assert!(mixed_output_checks_need_live_progress_gate(Some(&[
            "ffprobe".to_string()
        ])));
        assert!(mixed_output_checks_need_live_progress_gate(Some(&[
            "ffprobe".to_string(),
            "signal".to_string()
        ])));
        assert!(!mixed_output_checks_need_live_progress_gate(Some(&[
            "signal".to_string()
        ])));
        assert!(!mixed_output_checks_need_live_progress_gate(Some(&[
            "soak-drift".to_string()
        ])));
    }

    #[test]
    fn mixed_progress_output_ids_excludes_helper_outputs() {
        let output_ids = vec![
            "helper-hls".to_string(),
            "rtmp-a".to_string(),
            "srt-a".to_string(),
        ];
        assert_eq!(
            mixed_progress_output_ids(&output_ids, "helper-hls"),
            vec!["rtmp-a".to_string(), "srt-a".to_string()]
        );
    }

    #[test]
    fn mixed_input_matrix_keeps_rtmp_ingest_single_h264_only() {
        let rtmp_cases: Vec<_> = mixed_input_cases()
            .iter()
            .filter(|case| case.protocol() == MixedInputProtocol::Rtmp)
            .collect();
        assert_eq!(rtmp_cases.len(), 2);
        assert_eq!(rtmp_cases[0].scenario_id(), "mixed.live.rtmp.h264.a1.bf0");
        assert_eq!(rtmp_cases[1].scenario_id(), "mixed.live.rtmp.h264.a1.bf2");
        assert!(
            rtmp_cases
                .iter()
                .all(|case| matches!(case.codec(), MixedVideoCodec::H264))
        );
        assert!(rtmp_cases.iter().all(|case| !case.is_multi_track()));
        assert!(
            rtmp_cases
                .iter()
                .any(|case| matches!(case.reorder(), MixedInputReorder::Bf0))
        );
        assert!(
            rtmp_cases
                .iter()
                .any(|case| matches!(case.reorder(), MixedInputReorder::Bf2))
        );
    }

    #[test]
    fn mixed_input_matrix_covers_bf0_and_bf2_for_every_supported_shape() {
        let mut grouped = HashMap::new();
        for case in mixed_input_cases() {
            grouped
                .entry((case.protocol(), case.codec(), case.audio_layout()))
                .or_insert_with(Vec::new)
                .push(case.reorder());
        }

        for ((protocol, codec, audio_layout), reorders) in grouped {
            assert!(
                reorders.contains(&MixedInputReorder::Bf0),
                "missing bf0 row for {:?}/{:?}/{:?}",
                protocol,
                codec,
                audio_layout
            );
            assert!(
                reorders.contains(&MixedInputReorder::Bf2),
                "missing bf2 row for {:?}/{:?}/{:?}",
                protocol,
                codec,
                audio_layout
            );
        }
    }

    #[test]
    fn mixed_hls_preview_expectations_match_current_hevc_preview_contract() {
        for case in mixed_input_cases() {
            let expected = case.hls_preview_expected_dimensions();
            if matches!(case.codec(), MixedVideoCodec::H265) {
                assert_eq!(
                    expected,
                    "1280x720",
                    "{} should assert HEVC preview transcode dimensions",
                    case.scenario_id()
                );
            } else {
                assert_eq!(
                    expected,
                    "1920x1080",
                    "{} should assert source-size H.264 preview",
                    case.scenario_id()
                );
            }
        }
    }

    #[test]
    fn mixed_input_recording_expectations_follow_source_tracks() {
        for case in mixed_input_cases() {
            assert_eq!(
                case.expected_audio_tracks(),
                if case.is_multi_track() { 2 } else { 1 },
                "{} should record one assertion row with the source audio-track count",
                case.scenario_id()
            );
            assert_eq!(
                case.expected_video_codec(),
                if matches!(case.codec(), MixedVideoCodec::H265) {
                    "hevc"
                } else {
                    "h264"
                },
                "{} should record the source video codec",
                case.scenario_id()
            );
        }
    }

    #[test]
    fn mixed_input_rows_select_their_output_matrix() {
        for case in mixed_input_cases() {
            let plan = MixedScenarioPlan::for_input(*case);
            let cases = plan.outputs;
            assert_eq!(plan.source.adapter, MixedSourceAdapter::for_input(*case));
            assert_eq!(plan.expected_stages, expected_mixed_stage_count(*case));
            if case.is_multi_track() {
                assert_eq!(
                    cases.len(),
                    multi_track_mixed_output_cases().len(),
                    "{} should exercise the multi-audio output matrix",
                    case.scenario_id()
                );
                assert!(cases.iter().any(|case| case.expected_audio_tracks() == 2));
            } else {
                assert_eq!(
                    cases.len(),
                    single_track_mixed_output_cases().len(),
                    "{} should exercise the single-track output matrix",
                    case.scenario_id()
                );
                assert!(cases.iter().all(|case| case.expected_audio_tracks() == 1));
            }
        }
    }

    #[test]
    fn mixed_scenario_plan_expands_without_signal_cost() {
        let plans: Vec<_> = mixed_input_cases()
            .iter()
            .copied()
            .map(MixedScenarioPlan::for_input)
            .collect();

        assert_eq!(plans.len(), 18);
        assert_eq!(
            plans.iter().map(|plan| plan.output_cells()).sum::<usize>(),
            180
        );
        assert_eq!(
            plans
                .iter()
                .filter(|plan| plan.source.adapter == MixedSourceAdapter::FileIngest)
                .count(),
            8
        );
        assert_eq!(
            plans
                .iter()
                .filter(|plan| plan.source.adapter == MixedSourceAdapter::RtmpPublisher)
                .count(),
            2
        );
        assert_eq!(
            plans
                .iter()
                .filter(|plan| plan.source.adapter == MixedSourceAdapter::SrtPublisher)
                .count(),
            8
        );

        let check_names: Vec<_> = mixed_default_checks()
            .iter()
            .map(|check| check.as_str())
            .collect();
        assert_eq!(
            check_names,
            vec![
                "ffprobe",
                "audio-route",
                "decode-scan",
                "runtime-log",
                "stage-sharing",
                "hls",
                "recording",
                "load",
                "smoke",
                "lifecycle",
                "sink-probe",
                "hls-put-probe",
                "burst-graph",
            ]
        );
    }

    #[test]
    fn mixed_json_dsl_carries_current_matrix_contract() {
        let manifest = mixed_dsl_manifest().expect("mixed DSL manifest should parse");
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.input_cases().unwrap(), mixed_input_cases());

        let dsl_fast: Vec<_> = manifest
            .mixed
            .fast_breadth
            .iter()
            .map(|row| (row.id, row.rationale.as_str(), row.check_specs().unwrap()))
            .collect();
        let rust_fast: Vec<_> = mixed_fast_breadth_cases()
            .iter()
            .map(|row| {
                (
                    row.case.scenario_id(),
                    row.rationale.as_str(),
                    row.checks.to_vec(),
                )
            })
            .collect();
        assert_eq!(dsl_fast, rust_fast);

        let dsl_batches: Vec<_> = manifest
            .mixed
            .fast_breadth_batches
            .iter()
            .map(|batch| {
                (
                    MixedSharedBatchGroup::from_str(batch.group).unwrap(),
                    batch
                        .cases
                        .iter()
                        .map(|id| mixed_input_case_for_command(id).unwrap())
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let rust_batches: Vec<_> = mixed_fast_breadth_batches()
            .iter()
            .map(|batch| (batch.group, batch.cases.to_vec()))
            .collect();
        assert_eq!(dsl_batches, rust_batches);

        let dsl_signal: Vec<_> = manifest
            .mixed
            .signal_sentinels
            .iter()
            .map(|row| (row.id, row.rationale.as_str(), row.check_specs().unwrap()))
            .collect();
        let rust_signal: Vec<_> = mixed_signal_sentinels()
            .iter()
            .map(|row| {
                (
                    row.case.scenario_id(),
                    row.rationale.as_str(),
                    row.checks.to_vec(),
                )
            })
            .collect();
        assert_eq!(dsl_signal, rust_signal);

        let dsl_signal_batches: Vec<_> = manifest
            .mixed
            .signal_batches
            .iter()
            .map(|batch| {
                (
                    MixedSharedBatchGroup::from_str(batch.group).unwrap(),
                    batch
                        .cases
                        .iter()
                        .map(|id| mixed_input_case_for_command(id).unwrap())
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let rust_signal_batches: Vec<_> = mixed_signal_batches()
            .iter()
            .map(|batch| (batch.group, batch.cases.to_vec()))
            .collect();
        assert_eq!(dsl_signal_batches, rust_signal_batches);
    }

    #[test]
    fn fault_json_dsl_carries_current_case_contract() {
        assert_eq!(
            publisher_disconnect_cases()
                .iter()
                .map(|case| case.test_name.as_str())
                .collect::<Vec<_>>(),
            ["rtmp-publisher-disconnect", "srt-publisher-disconnect"]
        );

        assert_eq!(
            retry_budget_cases()
                .iter()
                .map(|case| (
                    case.test_name.as_str(),
                    case.protocol.ffmpeg_format(),
                    case.dead_sink_offset
                ))
                .collect::<Vec<_>>(),
            [
                ("rtmp-egress-retry-budget-exhausts", "flv", 77),
                ("srt-egress-retry-budget-exhausts", "mpegts", 78),
            ]
        );

        assert_eq!(
            recovery_transient_cases()
                .iter()
                .map(|case| (
                    case.test_name.as_str(),
                    case.protocol.ffmpeg_format(),
                    case.wait_input_off_after_drop,
                    case.require_media_ready_on_resume,
                    case.second_reconnect_checks_flapping,
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "transient-rtmp-drop-preserves-egress",
                    "flv",
                    false,
                    false,
                    true,
                ),
                (
                    "transient-srt-drop-preserves-egress",
                    "mpegts",
                    true,
                    true,
                    false,
                ),
            ]
        );

        for test_name in [
            "file-ingest-stop",
            "recording-stops-after-ingest-disconnect",
            "hls-preview-stops-after-ingest-disconnect",
            "file-ingest-eof-clears-and-restarts",
        ] {
            assert_eq!(
                ingest_lifecycle_case(test_name).unwrap().test_name,
                test_name
            );
        }
    }

    #[test]
    fn output_retry_fault_phase_accepts_retry_error_or_cleanup() {
        let retrying_with_error = OutputRetryObservation {
            status_visible: true,
            has_error: true,
            ..Default::default()
        };
        let cleaned_up = OutputRetryObservation {
            cleaned_up: true,
            ..Default::default()
        };
        let retrying_without_error = OutputRetryObservation {
            status_visible: true,
            ..Default::default()
        };

        assert!(output_retry_or_cleanup_phase_ok(&retrying_with_error));
        assert!(output_retry_or_cleanup_phase_ok(&cleaned_up));
        assert!(!output_retry_or_cleanup_phase_ok(&retrying_without_error));
    }

    #[test]
    fn ramp_json_dsl_carries_current_config_contract() {
        let configs = ramp_configs();
        assert_eq!(configs.len(), 8);
        assert_eq!(configs[0].name, "rtmp-rtmp-src");
        assert_eq!(configs[7].name, "srt-srt-720p");
    }

    #[test]
    fn resource_egress_scenario_table_carries_branch_contract() {
        assert_eq!(resource_egress_scenarios().len(), 8);
        assert_eq!(
            resource_egress_scenarios()
                .iter()
                .filter(|scenario| scenario.branch_order.is_some())
                .count(),
            5
        );
        assert_eq!(
            resource_egress_scenario("egress-growth-hevc-bridge")
                .unwrap()
                .config_index,
            2
        );
        assert_eq!(
            resource_egress_scenario("egress-growth-source-plus-transcode-dual-mixed")
                .unwrap()
                .output_kinds,
            vec![
                SweepOutputKind::RtmpSource,
                SweepOutputKind::SrtSource,
                SweepOutputKind::Rtmp720p,
                SweepOutputKind::Srt720p,
                SweepOutputKind::Rtmp1080p,
                SweepOutputKind::Srt1080p,
            ]
        );
        assert_eq!(
            resource_egress_scenario("egress-growth-transcode-mixed")
                .unwrap()
                .branch_label(),
            "one transcode family (720p)"
        );
    }

    #[test]
    fn sweep_output_kind_centralizes_urls_and_multi_audio_encoding() {
        assert_eq!(
            SweepOutputKind::Rtmp720p.publish_url(1936, 8891, "out"),
            "rtmp://127.0.0.1:1936/live/out"
        );
        assert_eq!(
            SweepOutputKind::Srt720p.publish_url(1936, 8891, "out"),
            "srt://127.0.0.1:8891?streamid=publish:live/out"
        );
        assert_eq!(
            SweepOutputKind::Srt720p.read_url(1936, 8891, "out"),
            "srt://127.0.0.1:8891?streamid=read:live/out&timeout=30000000"
        );
        assert_eq!(SweepOutputKind::Rtmp720p.encoding(true), "720p+atrack:0");
        assert_eq!(SweepOutputKind::Srt720p.encoding(true), "720p+atrack:0,1");
        assert_eq!(SweepOutputKind::SrtSource.encoding(true), "source");
    }

    #[test]
    fn resource_output_progress_timeout_scales_and_caps() {
        assert_eq!(
            scaled_output_progress_timeout(1, 30, 4, 240),
            Duration::from_secs(30)
        );
        assert_eq!(
            scaled_output_progress_timeout(20, 30, 4, 240),
            Duration::from_secs(106)
        );
        assert_eq!(
            scaled_output_progress_timeout(60, 30, 4, 240),
            Duration::from_secs(240)
        );
        assert_eq!(
            scaled_output_progress_timeout(0, 30, 4, 240),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn mixed_input_planning_shares_stages_across_duplicate_outputs() {
        for case in mixed_input_cases() {
            let single = planned_mixed_stage_count(*case, 1);
            let duplicated = planned_mixed_stage_count(*case, 2);
            let expected = expected_mixed_stage_count(*case);

            assert_eq!(
                single,
                expected,
                "{} should plan the expected unique stage set",
                case.scenario_id()
            );
            assert_eq!(
                duplicated,
                single,
                "{} should not add unique processing stages when N_PER_GROUP grows",
                case.scenario_id()
            );
        }
    }

    #[test]
    fn mixed_input_suite_default_runs_aggregate_not_duplicate_rows() {
        let matrix_spec = mode_spec("mixed.matrix").expect("mixed.matrix must be listed");
        assert!(matrix_spec.suite_default);
        let signal_spec = mode_spec(MIXED_SIGNAL_MODE).expect("mixed.signal must be listed");
        assert!(!signal_spec.suite_default);
        let fast_spec =
            mode_spec(MIXED_FAST_BREADTH_MODE).expect("mixed.fast-breadth must be listed");
        assert!(!fast_spec.suite_default);
        for case in mixed_input_cases() {
            let mode = mixed_input_mode_name(*case);
            let spec = mode_spec(&mode).unwrap_or_else(|| panic!("{mode} must be listed"));
            assert!(
                !spec.suite_default,
                "{mode} is covered by mixed.matrix and should not duplicate default suite work"
            );
        }
    }

    #[test]
    fn mixed_input_modes_share_one_bench_profile_policy() {
        for case in mixed_input_cases() {
            let mode = mixed_input_mode_name(*case);
            let spec = mode_spec(&mode).unwrap_or_else(|| panic!("{mode} must be listed"));
            assert!(
                spec.requires_bench_profile,
                "{mode} should inherit the mixed harness bench-profile requirement"
            );
        }
    }

    #[test]
    fn mixed_input_fixture_selection_tracks_reorder_signal() {
        for case in mixed_input_cases() {
            let fixture = mixed_input_fixture(*case).unwrap_or_else(|error| {
                panic!(
                    "{} should resolve a checked-in fixture: {error}",
                    case.scenario_id()
                )
            });
            let file_name = fixture.file_name().unwrap().to_string_lossy();
            match case.reorder() {
                MixedInputReorder::Bf0 => assert!(
                    file_name.contains("-bf0"),
                    "{} should use a bf0 fixture, got {}",
                    case.scenario_id(),
                    file_name
                ),
                MixedInputReorder::Bf2 => assert!(
                    !file_name.contains("-bf0"),
                    "{} should use the reordered bf2 fixture family, got {}",
                    case.scenario_id(),
                    file_name
                ),
            }
        }
    }

    #[test]
    fn single_track_output_matrix_exercises_all_protocol_encoding_pairs() {
        let pairs: Vec<_> = single_track_mixed_output_cases()
            .iter()
            .map(|case| (mixed_output_protocol_name(case.protocol()), case.encoding()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("rtmp", "source"),
                ("rtmp", "720p"),
                ("rtmp", "1080p"),
                ("srt", "source"),
                ("srt", "720p"),
                ("srt", "1080p"),
            ]
        );
        assert!(
            single_track_mixed_output_cases()
                .iter()
                .all(|case| case.expected_audio_tracks() == 1)
        );
    }

    #[test]
    fn single_track_output_matrix_reports_same_rows_it_executes() {
        let rows = mixed_output_matrix_json(single_track_mixed_output_cases());
        let groups: Vec<_> = rows.iter().map(|row| row["id"].as_str().unwrap()).collect();
        assert_eq!(
            groups,
            vec![
                "rtmp.src.a0",
                "rtmp.720p.a0",
                "rtmp.1080p.a0",
                "srt.src.a0",
                "srt.720p.a0",
                "srt.1080p.a0",
            ]
        );
    }

    #[test]
    fn multi_track_output_matrix_exercises_rtmp_subsets_and_srt_all_plus_subsets() {
        let groups: Vec<_> = multi_track_mixed_output_cases()
            .iter()
            .map(|case| case.id())
            .collect();
        assert_eq!(
            groups,
            vec![
                "rtmp.src.a0",
                "rtmp.src.a1",
                "rtmp.720p.a0",
                "rtmp.720p.a1",
                "rtmp.1080p.a0",
                "rtmp.1080p.a1",
                "srt.src.all",
                "srt.src.a0",
                "srt.src.a1",
                "srt.720p.all",
                "srt.720p.a0",
                "srt.720p.a1",
                "srt.1080p.all",
                "srt.1080p.a0",
                "srt.1080p.a1",
            ]
        );
        let rtmp_cases: Vec<_> = multi_track_mixed_output_cases()
            .iter()
            .filter(|case| case.protocol() == MixedOutputProtocol::Rtmp)
            .collect();
        assert_eq!(rtmp_cases.len(), 6);
        assert!(
            rtmp_cases
                .iter()
                .all(|case| case.expected_audio_tracks() == 1)
        );
        assert!(
            rtmp_cases
                .iter()
                .all(|case| case.selected_audio_track().is_some())
        );

        let srt_all_cases: Vec<_> = multi_track_mixed_output_cases()
            .iter()
            .filter(|case| {
                case.protocol() == MixedOutputProtocol::Srt && case.selected_audio_track().is_none()
            })
            .collect();
        assert_eq!(srt_all_cases.len(), 3);
        assert!(
            srt_all_cases
                .iter()
                .all(|case| case.expected_audio_tracks() == 2)
        );
    }
}
