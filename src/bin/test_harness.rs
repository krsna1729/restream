//! End-to-end integration harness that drives RTMP, SRT, HLS, and API flows
//! against a running restream instance for higher-level verification.

use axum::Router;
use axum::extract::{DefaultBodyLimit, OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, put};
use bytes::Bytes;
use chrono::Utc;
use restream::domain::audio_routing::{AudioRouting, is_audio_operation, parse_audio_operation};
use restream::domain::output_spec::{
    OutputConfig, OutputUrlScheme, OutputVideoCodec, OutputVideoConfig, RtmpOutputMode,
};
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
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

#[path = "test_harness/api_client.rs"]
mod api_client;
#[path = "test_harness/catalog.rs"]
#[allow(dead_code)]
mod catalog;
#[path = "test_harness/catalog_cli.rs"]
mod catalog_cli;
#[path = "test_harness/core.rs"]
mod core;
#[path = "test_harness/fault_input_promotion.rs"]
mod fault_input_promotion;
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
#[path = "test_harness/mediamtx_probe.rs"]
mod mediamtx_probe;
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
#[path = "test_harness/srt_urls.rs"]
mod srt_urls;
#[path = "test_harness/suite.rs"]
mod suite;
#[path = "test_harness/workflow_exec.rs"]
mod workflow_exec;

use api_client::*;
use catalog_cli::*;
use core::*;
use fault_input_promotion::*;
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
use srt_urls::*;
use suite::*;
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
    use restream::domain::stage::StageKind;
    use restream::planner::{BackendPolicy, PlannedOutput, plan_pipeline_graph};

    let outputs = mixed_output_cases_for_input(case)
        .iter()
        .flat_map(|output_case| {
            (0..duplicates_per_output).map(move |duplicate| {
                let url = match output_case.protocol() {
                    MixedOutputProtocol::Rtmp => "rtmp://example/live/out",
                    MixedOutputProtocol::Srt => "srt://example:9000?streamid=publish:out",
                };
                PlannedOutput::new(
                    format!("{}-{duplicate}", output_case.id()),
                    output_case.output_config(),
                    url,
                )
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
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let command = raw.first().cloned().unwrap_or_else(|| "suite".to_string());
    if command == "catalog" {
        return run_catalog_cli(&raw[1..]);
    }

    ensure_loopback();
    maybe_prune_old_artifacts()?;
    maybe_global_process_cleanup();
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
            "msr" => msr().await,
            "msr.dashboard" => msr_dashboard().await,
            "bitrate-sweep" => bitrate_sweep().await,
            "branch-matrix" => branch_matrix().await,
            "backend-policy-matrix" => backend_policy_matrix().await,
            "srt-crypto-matrix" => srt_crypto_matrix().await,
            "rtmp-fabric-matrix" => rtmp_fabric_matrix().await,
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
            if !env_flag("TEST_HARNESS_SUPPRESS_SUCCESS_JSON") {
                println!("{}", serde_json::to_string_pretty(&value).unwrap());
            }
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
        .unwrap_or_else(|| PathBuf::from(".local/artifacts/api-smoke"));
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

#[cfg(test)]
#[path = "test_harness/root_tests.rs"]
mod tests;
