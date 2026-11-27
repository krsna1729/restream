//! End-to-end integration harness that drives RTMP, SRT, HLS, and API flows
//! against a running restream instance for higher-level verification.
#![allow(clippy::disallowed_methods)] // test code: raw std locks are fine
// Unit tests park tasks behind held guards on purpose.
#![cfg_attr(test, allow(clippy::await_holding_invalid_type))]

use axum::Router;
use axum::extract::{DefaultBodyLimit, OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, put};
use bytes::Bytes;
use restream_domain::domain::audio_routing::{
    AudioRouting, is_audio_operation, parse_audio_operation,
};
use restream_domain::domain::output_spec::{
    OutputConfig, OutputUrlScheme, OutputVideoCodec, OutputVideoConfig, RtmpOutputMode,
};
use restream_domain::timestamp::now_rfc3339;
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
#[path = "test_harness/api_waits.rs"]
mod api_waits;
#[path = "test_harness/catalog.rs"]
mod catalog;
#[path = "test_harness/catalog_cli.rs"]
mod catalog_cli;
#[path = "test_harness/core.rs"]
mod core;
#[path = "test_harness/fault_case_routines.rs"]
mod fault_case_routines;
#[path = "test_harness/fault_input_promotion.rs"]
mod fault_input_promotion;
#[path = "test_harness/fault_manifest.rs"]
mod fault_manifest;
#[path = "test_harness/fault_recovery.rs"]
mod fault_recovery;
#[path = "test_harness/fault_runner.rs"]
mod fault_runner;
#[path = "test_harness/harness_srt_sink.rs"]
mod harness_srt_sink;
#[path = "test_harness/hls_pull_duty.rs"]
mod hls_pull_duty;
#[path = "test_harness/hls_push_duty.rs"]
mod hls_push_duty;
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
#[path = "test_harness/moq_dev_interop.rs"]
mod moq_dev_interop;
#[path = "test_harness/moq_impairment.rs"]
mod moq_impairment;
#[path = "test_harness/moq_moxygen_interop.rs"]
mod moq_moxygen_interop;
#[path = "test_harness/moq_relay_interop.rs"]
mod moq_relay_interop;
#[path = "test_harness/observe.rs"]
mod observe;
#[path = "test_harness/output_helpers.rs"]
mod output_helpers;
#[path = "test_harness/output_progress.rs"]
mod output_progress;
#[path = "test_harness/thread_cpu.rs"]
mod thread_cpu;

#[path = "test_harness/admission.rs"]
mod admission;
#[path = "test_harness/peer_state.rs"]
mod peer_state;
#[path = "test_harness/resource_sweep.rs"]
mod resource_sweep;
#[path = "test_harness/sinks.rs"]
mod sinks;
#[path = "test_harness/srt_backup_failover.rs"]
mod srt_backup_failover;
#[path = "test_harness/srt_raw_sink.rs"]
mod srt_raw_sink;
#[path = "test_harness/srt_sink.rs"]
mod srt_sink;
#[path = "test_harness/srt_slow_peer.rs"]
mod srt_slow_peer;
#[path = "test_harness/srt_urls.rs"]
mod srt_urls;
#[path = "test_harness/ts_continuity.rs"]
mod ts_continuity;

#[path = "test_harness/suite.rs"]
mod suite;

#[cfg(test)]
#[path = "test_harness/udp_drain.rs"]
mod udp_drain;

use admission::*;
use api_client::*;
use api_waits::*;
use catalog_cli::*;
use core::*;
use fault_case_routines::*;
use fault_input_promotion::*;
use fault_manifest::*;
use fault_recovery::*;
use fault_runner::*;
use harness_srt_sink::*;
use hls_pull_duty::*;
use hls_push_duty::*;
use hls_put::*;
use live_modes::*;
use media_probes::*;
pub(crate) use mixed_adaptive_ring::*;
use mixed_manifest::*;
use mixed_runner::*;
pub(crate) use mode_specs::*;
use observe::*;
use output_helpers::*;
use output_progress::*;
use resource_sweep::*;
use sinks::*;
use srt_backup_failover::*;
use srt_raw_sink::*;
use srt_slow_peer::*;
use srt_urls::*;
use suite::*;

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
    use restream_domain::domain::stage::StageKind;
    use restream_domain::planner::{PlannedOutput, plan_pipeline_graph};

    let outputs = mixed_output_cases_for_input(case)
        .iter()
        .flat_map(|output_case| {
            (0..duplicates_per_output).map(move |duplicate| {
                let url = match output_case.protocol() {
                    MixedOutputProtocol::Rtmp => "rtmp://example/live/out",
                    MixedOutputProtocol::Rtmps => "rtmps://example/live/out",
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
    let plan = plan_pipeline_graph("pipe", Some(case.expected_video_codec()), &outputs, false);

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
            std::process::exit(1);
        }
        if let Err(error) = run().await {
            eprintln!("test harness failed: {error}");
            // Exit here rather than drop the runtime: a failed run can leave
            // blocking tasks the runtime's drop would wait on.
            std::process::exit(1);
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

    prepare_run(&command, &raw[1..])?;
    let result = run_command(&command, &raw[1..]).await;
    finish_run(&command, result)
}

fn prepare_run(command: &str, args: &[String]) -> Result<(), String> {
    ensure_loopback();
    maybe_prune_old_artifacts()?;
    maybe_global_process_cleanup();
    ensure_msr_nofile_limit(command)?;
    ensure_measurement_profile(command, args)?;
    Ok(())
}

/// Artifact write and success exit. The runtime is left un-dropped on the
/// success path because dropping it would wait on the engine's still-running
/// blocking tasks and OS threads.
fn finish_run(command: &str, result: Result<Value, String>) -> Result<(), String> {
    let value = result?;
    let path = mixed_command_artifact_path(command)
        .unwrap_or_else(|| artifact_path(&format!("{command}.json")));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).map_err(|e| e.to_string())?;
    if !env_flag("TEST_HARNESS_SUPPRESS_SUCCESS_JSON") {
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    }
    println!("artifact={}", path.display());
    // Exit without dropping the runtime, which would wait on the engine's
    // still-running blocking tasks and OS threads.
    std::process::exit(0);
}

async fn run_command(command: &str, args: &[String]) -> Result<Value, String> {
    if let Some(result) = run_mixed_mode(command).await {
        return result;
    }
    if let Some(result) = run_engine_mode(command).await {
        return result;
    }
    if let Some(result) = run_fault_mode(command).await {
        return result;
    }
    if let Some(result) = run_admission_mode(command).await {
        return result;
    }
    if let Some(result) = run_duty_and_interop_mode(command, args).await {
        return result;
    }
    Err(unknown_command_error(command))
}

async fn run_mixed_mode(command: &str) -> Option<Result<Value, String>> {
    if command == MIXED_MATRIX_MODE {
        Some(mixed_input_matrix_correctness().await)
    } else if command == MIXED_SIGNAL_MODE {
        Some(mixed_signal_correctness().await)
    } else if command == MIXED_FAST_BREADTH_MODE {
        Some(mixed_fast_breadth_correctness().await)
    } else if let Some(case) = mixed_input_case_for_command(command) {
        Some(mixed_input_case_correctness(case).await)
    } else {
        None
    }
}

async fn run_engine_mode(command: &str) -> Option<Result<Value, String>> {
    let result = match command {
        "api-smoke" => api_smoke().await,
        "srt.policy" => srt_policy_correctness().await,
        "timestamp.bframe" => bframe_rtmp_correctness().await,
        "ramp-family" => ramp_family_correctness().await,
        "suite" => suite_run().await,
        "preflight" => preflight_check().await,
        "file.live-edge" => file_live_edge().await,
        "signal.control" => signal_control().await,
        "recovery" => recovery().await,
        "resource-sweep" => resource_sweep().await,
        "srt-sink" => srt_sink::srt_sink_mode().await,
        "msr" => msr().await,
        "bitrate-sweep" => bitrate_sweep().await,
        "branch-matrix" => branch_matrix().await,
        "srt-crypto-matrix" => srt_crypto_matrix().await,
        _ => return None,
    };
    Some(result)
}

async fn run_fault_mode(command: &str) -> Option<Result<Value, String>> {
    let result = match command {
        "fault.egress-retry" => fault_egress_retry().await,
        "fault.output-stall" => fault_output_stall().await,
        "fault.srt-output-stall" => fault_srt_output_stall().await,
        "fault.sibling-isolation" => fault_sibling_isolation().await,
        "fault.resilience" => fault_resilience().await,
        "srt.slow-peer" => srt_slow_peer().await,
        "srt.backup-failover" => srt_backup_failover().await,
        _ => return None,
    };
    Some(result)
}

fn admission_fabric(command: &str) -> Option<AdmissionFabric> {
    Some(match command {
        "srt.admission" => AdmissionFabric::Srt,
        "sink.admission" => AdmissionFabric::Sink,
        "pipeline.admission" => AdmissionFabric::Pipeline,
        "hls-put.admission" => AdmissionFabric::HlsPut,
        _ => return None,
    })
}

async fn run_admission_mode(command: &str) -> Option<Result<Value, String>> {
    let fabric = admission_fabric(command)?;
    Some(admission(fabric).await)
}

async fn run_duty_and_interop_mode(
    command: &str,
    args: &[String],
) -> Option<Result<Value, String>> {
    let result = match command {
        "hls-pull-duty" => hls_pull_duty(args).await,
        "hls-push-duty" => hls_push_duty(args).await,
        "moq-relay-interop" => moq_relay_interop::moq_relay_interop(args).await,
        "moq-dev-interop" => moq_dev_interop::moq_dev_interop(args).await,
        "moq-impairment" => moq_impairment::moq_impairment(args).await,
        "moq-moxygen-interop" => moq_moxygen_interop::moq_moxygen_interop(args).await,
        _ => return None,
    };
    Some(result)
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

/// Bound on any single HTTP request the harness makes to a restream/mediamtx
/// process under test. `restream_http::client()` has no default request
/// timeout, so a stalled/deadlocked server response would hang the
/// awaiting `.send().await` call forever — invisible to every caller-side
/// deadline (`wait_for_http_ok`'s own retry loop, a scenario's
/// `outputs-progress` budget, even the CI job's outer `timeout` command
/// cannot interrupt a single blocked `.await`) and, notably, invisible to
/// this harness's own progress logging, since nothing prints while blocked
/// inside one HTTP call. Every `reqwest::Client` this harness constructs
/// must be built with this timeout so a stuck server produces a loud,
/// bounded error instead of consuming an entire CI job's time budget in
/// silence, well past any of the scenario's own per-pipeline deadlines.
pub(crate) const HARNESS_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Every peer this client talks to is a loopback process the harness started,
/// and requests carry the admin password and session cookie in cleartext HTTP.
/// Environment proxies (`HTTP_PROXY`, `ALL_PROXY`) and redirects are therefore
/// disabled so no request or body can leave the host.
pub(crate) fn harness_http_client() -> reqwest::Client {
    restream_http::client_builder()
        .timeout(HARNESS_HTTP_REQUEST_TIMEOUT)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("harness reqwest client")
}

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
