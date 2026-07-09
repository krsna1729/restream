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
#[path = "test_harness/fault_manifest.rs"]
mod fault_manifest;
#[path = "test_harness/fault_runner.rs"]
mod fault_runner;
#[path = "test_harness/hls_put.rs"]
mod hls_put;
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

use fault_manifest::*;
use fault_runner::*;
use hls_put::*;
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
const FILE_LIVE_EDGE_MAX_DURATION_DRIFT_SECS: f64 = 0.75;

fn path_profile(path: &Path) -> Option<&'static str> {
    let mut components = path.components();
    while let Some(component) = components.next() {
        if component.as_os_str() == "target" {
            return components
                .next()
                .and_then(|value| value.as_os_str().to_str())
                .and_then(|value| match value {
                    "debug" => Some("debug"),
                    "release" => Some("release"),
                    "bench" => Some("bench"),
                    _ => None,
                });
        }
    }
    None
}

fn is_bench_profile(path: &Path) -> bool {
    matches!(path_profile(path), Some("bench"))
}

fn default_work_db_path(work_dir: &Path, file_name: &str) -> PathBuf {
    // Keep mutable harness state scoped to each WORK_DIR so long suites do not
    // contend through a shared repo-root SQLite database.
    work_dir.join(file_name)
}

fn command_uses_host_net(raw: &[String]) -> bool {
    raw.iter().any(|arg| arg == "--no-netns")
        || std::env::var("TEST_HARNESS_USE_HOST_NET")
            .ok()
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

fn strip_netns_opt(raw: &[String]) -> Vec<String> {
    raw.iter()
        .filter(|arg| arg.as_str() != "--no-netns")
        .cloned()
        .collect()
}

fn netns_available() -> bool {
    std::process::Command::new("unshare")
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn maybe_reexec_in_port_namespace() -> Result<(), String> {
    if std::env::var_os("RESTREAM_HARNESS_IN_NETNS").is_some() {
        return Ok(());
    }

    let raw: Vec<String> = std::env::args().skip(1).collect();
    let command = raw.first().map(String::as_str).unwrap_or("suite");
    if command == "suite"
        || command == "preflight"
        || !command_requires_port_namespace(command)
        || command_uses_host_net(&raw)
    {
        return Ok(());
    }

    if !netns_available() {
        return Err(format!(
            "{command} requires a network namespace by default; install `unshare` support or rerun with --no-netns"
        ));
    }

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let status = std::process::Command::new("unshare")
        .args(["--net", "--user", "--map-root-user"])
        .arg(&exe)
        .args(strip_netns_opt(&raw))
        .env("RESTREAM_HARNESS_IN_NETNS", "1")
        .status()
        .map_err(|e| format!("failed to re-exec {command} inside a network namespace: {e}"))?;

    let code = status.code().unwrap_or(1);
    unsafe { libc::_exit(code) };
}

fn ensure_measurement_profile(command: &str, raw: &[String]) -> Result<(), String> {
    let needs_bench = if command == "suite" {
        suite_modes_require_bench_profile(raw)?
    } else {
        command == "preflight" || measurement_mode_requires_bench_profile(command)
    };
    if !needs_bench {
        return Ok(());
    }

    let harness_path = std::env::current_exe().map_err(|e| e.to_string())?;
    let restream_path = default_restream_bin();
    if is_bench_profile(&harness_path) && is_bench_profile(&restream_path) {
        return Ok(());
    }

    Err(format!(
        "{command} requires bench-profile binaries for valid measurements; build them with `scripts/build-bench-harness.sh` and run `target/bench/test_harness`"
    ))
}

fn harness_runtime_worker_threads() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    std::env::var("HARNESS_TOKIO_WORKER_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(cpus.clamp(2, 16))
        .max(1)
}

fn harness_runtime_max_blocking_threads() -> usize {
    std::env::var("HARNESS_TOKIO_MAX_BLOCKING_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(256)
        .max(1)
}

fn default_restream_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("RESTREAM_BIN").map(PathBuf::from) {
        return path;
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(bin_dir) = exe.parent()
    {
        let sibling = bin_dir.join("restream");
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from("target/release/restream")
}

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

fn mixed_command_artifact_path(command: &str) -> Option<PathBuf> {
    if command == MIXED_MATRIX_MODE
        || command == MIXED_SIGNAL_MODE
        || command == MIXED_FAST_BREADTH_MODE
    {
        let work_dir = std::env::var_os("WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if command == MIXED_FAST_BREADTH_MODE {
                    mixed_fast_breadth_default_work_dir()
                } else if command == MIXED_SIGNAL_MODE {
                    mixed_signal_default_work_dir()
                } else {
                    mixed_matrix_default_work_dir()
                }
            });
        return Some(work_dir.join("scenario.json"));
    }
    let case = mixed_input_case_for_command(command)?;
    let work_dir = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| mixed_input_default_work_dir(case));
    Some(work_dir.join("scenario.json"))
}

fn artifact_path(name: &str) -> PathBuf {
    std::env::var_os("TEST_HARNESS_ARTIFACT_DIR")
        .or_else(|| std::env::var_os("WORK_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("test/artifacts/latest"))
        .join(name)
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

fn maybe_global_process_cleanup() {
    if !env_flag("ALLOW_GLOBAL_PROCESS_CLEANUP") {
        return;
    }
    for program in ["restream", "mediamtx", "ffmpeg"] {
        let _ = std::process::Command::new("pkill")
            .args(["-x", program])
            .status();
    }
}

fn maybe_prune_old_artifacts() -> Result<(), String> {
    if env_flag("KEEP_ARTIFACTS") {
        return Ok(());
    }
    let artifact_root = PathBuf::from("test/artifacts");
    let Ok(entries) = std::fs::read_dir(&artifact_root) else {
        return Ok(());
    };
    let mut run_dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_timestamp_run_dir)
        })
        .collect();
    if run_dirs.len() <= 3 {
        return Ok(());
    }
    run_dirs.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    let remove_count = run_dirs.len().saturating_sub(3);
    for path in run_dirs.into_iter().take(remove_count) {
        std::fs::remove_dir_all(&path).map_err(|error| {
            format!(
                "failed to prune old artifact directory {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn command_with_optional_cgroup(program: impl AsRef<OsStr>, scope: &str) -> Command {
    if !env_flag("HARNESS_USE_CGROUP_WRAPPER") {
        return Command::new(program);
    }
    let mut command = Command::new("scripts/cgroup-wrap");
    command.arg("--scope").arg(scope).arg("--").arg(program);
    command
}

fn is_timestamp_run_dir(name: &str) -> bool {
    if name.len() != 16 {
        return false;
    }
    let bytes = name.as_bytes();
    bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'Z'
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path))
    }
}

fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn harness_srt_passphrase() -> Option<String> {
    std::env::var("HARNESS_SRT_PASSPHRASE")
        .ok()
        .filter(|value| !value.is_empty())
}

fn harness_srt_pbkeylen() -> Option<String> {
    std::env::var("HARNESS_SRT_PBKEYLEN")
        .ok()
        .filter(|value| !value.is_empty())
}

/// SRT encryption parameters injected into harness SRT listeners and URLs.
#[derive(Clone, Debug)]
struct HarnessSrtCrypto {
    label: String,
    passphrase: Option<String>,
    pbkeylen: Option<String>,
}

impl HarnessSrtCrypto {
    fn plaintext() -> Self {
        Self {
            label: "plaintext".to_string(),
            passphrase: None,
            pbkeylen: None,
        }
    }

    fn encrypted(pbkeylen: u32) -> Self {
        Self {
            label: format!("encrypted-{pbkeylen}"),
            passphrase: Some("0123456789abcd".to_string()),
            pbkeylen: Some(pbkeylen.to_string()),
        }
    }

    fn transport_label(&self) -> String {
        match (&self.passphrase, &self.pbkeylen) {
            (None, _) => "plaintext".to_string(),
            (Some(_), Some(len)) => format!("encrypted-{len}"),
            (Some(_), None) => "encrypted".to_string(),
        }
    }
}

fn harness_srt_crypto_from_env() -> HarnessSrtCrypto {
    match harness_srt_passphrase() {
        Some(passphrase) => HarnessSrtCrypto {
            label: match harness_srt_pbkeylen() {
                Some(len) => format!("encrypted-{len}"),
                None => "encrypted".to_string(),
            },
            passphrase: Some(passphrase),
            pbkeylen: harness_srt_pbkeylen(),
        },
        None => HarnessSrtCrypto::plaintext(),
    }
}

fn parse_srt_crypto_variants(name: &str, default: &str) -> Result<Vec<HarnessSrtCrypto>, String> {
    let mut out = Vec::new();
    for part in std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let variant = match part.to_ascii_lowercase().as_str() {
            "plaintext" | "plain" => HarnessSrtCrypto::plaintext(),
            "encrypted-16" | "enc16" | "aes128" | "128" => HarnessSrtCrypto::encrypted(16),
            "encrypted-24" | "enc24" | "aes192" | "192" => HarnessSrtCrypto::encrypted(24),
            "encrypted-32" | "enc32" | "aes256" | "256" => HarnessSrtCrypto::encrypted(32),
            other => {
                return Err(format!(
                    "{name} contains unsupported SRT crypto variant '{other}'"
                ));
            }
        };
        if out
            .iter()
            .all(|existing: &HarnessSrtCrypto| existing.label != variant.label)
        {
            out.push(variant);
        }
    }
    if out.is_empty() {
        return Err(format!("{name} did not resolve to any SRT crypto variants"));
    }
    Ok(out)
}

fn append_srt_crypto(url: String, crypto: &HarnessSrtCrypto) -> String {
    let Some(passphrase) = crypto.passphrase.as_deref() else {
        return url;
    };
    let separator = if url.contains('?') { '&' } else { '?' };
    let mut out = format!("{url}{separator}passphrase={passphrase}");
    if let Some(pbkeylen) = crypto.pbkeylen.as_deref() {
        out.push_str(&format!("&pbkeylen={pbkeylen}"));
    }
    out
}

fn apply_srt_listener_env(cmd: &mut Command, crypto: &HarnessSrtCrypto) {
    if let Some(passphrase) = crypto.passphrase.as_deref() {
        cmd.env("RESTREAM_SRT_PASSPHRASE", passphrase);
        if let Some(pbkeylen) = crypto.pbkeylen.as_deref() {
            cmd.env("RESTREAM_SRT_PBKEYLEN", pbkeylen);
        }
    } else {
        cmd.env_remove("RESTREAM_SRT_PASSPHRASE");
        cmd.env_remove("RESTREAM_SRT_PBKEYLEN");
    }
}

fn apply_harness_srt_listener_env(cmd: &mut Command) {
    apply_srt_listener_env(cmd, &harness_srt_crypto_from_env());
}

// ── Shared test infrastructure (Phase 1) ────────────────────────────────────
//
// `TestPorts` + `start_restream_child` de-duplicate the port and child-process
// setup that was previously inlined in `start_ramp_restream` and
// `start_mixed_restream`.

/// Concrete restream listener ports for one isolated harness process.
struct TestPorts {
    http: u16,
    rtmp: u16,
    srt: u16,
}

/// Synthesized non-overlapping port ranges for restream, MediaMTX, and probes.
#[derive(Clone, Copy)]
struct HarnessPortDefaults {
    restream_http: u16,
    restream_rtmp: u16,
    restream_srt: u16,
    mtx_rtmp: u16,
    mtx_srt: u16,
    mtx_hls: u16,
    mtx_api: u16,
    sink: u16,
    hls_put: u16,
    ffmpeg_srt_sink_base: u16,
    ffmpeg_signal_sink_base: u16,
}

static HARNESS_PORT_DEFAULTS: OnceLock<HarnessPortDefaults> = OnceLock::new();

impl TestPorts {
    fn from_env() -> Self {
        let ports = harness_port_defaults();
        Self {
            http: ports.restream_http,
            rtmp: ports.restream_rtmp,
            srt: ports.restream_srt,
        }
    }
}

async fn start_restream_child(
    bin: &Path,
    ports: &TestPorts,
    db_path: &Path,
    log_path: &Path,
) -> Result<Child, String> {
    start_restream_child_opts(bin, ports, db_path, log_path, true, None, &[]).await
}

async fn start_restream_api(
    bin: &Path,
    ports: &TestPorts,
    db_path: &Path,
    log_path: &Path,
) -> Result<(Child, RampApi), String> {
    let child = start_restream_child(bin, ports, db_path, log_path).await?;
    Ok((child, login_api(ports).await?))
}

async fn login_api(ports: &TestPorts) -> Result<RampApi, String> {
    let mut api = RampApi::new(ports.http);
    api.login().await?;
    Ok(api)
}

async fn start_restream_child_with_env(
    bin: &Path,
    ports: &TestPorts,
    db_path: &Path,
    log_path: &Path,
    env_overrides: &[(&str, String)],
) -> Result<Child, String> {
    start_restream_child_opts(bin, ports, db_path, log_path, true, None, env_overrides).await
}

async fn start_restream_child_in_media_dir(
    bin: &Path,
    ports: &TestPorts,
    db_path: &Path,
    log_path: &Path,
    media_dir: &Path,
) -> Result<Child, String> {
    start_restream_child_opts(bin, ports, db_path, log_path, true, Some(media_dir), &[]).await
}

async fn start_restream_child_opts(
    bin: &Path,
    ports: &TestPorts,
    db_path: &Path,
    log_path: &Path,
    clean_db: bool,
    media_dir: Option<&Path>,
    env_overrides: &[(&str, String)],
) -> Result<Child, String> {
    if !bin.exists() {
        return Err(format!("restream binary not found at {}", bin.display()));
    }
    if clean_db {
        cleanup_ramp_db(db_path);
    }
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let log_dir = log_path
        .parent()
        .map(|parent| parent.join("logs"))
        .unwrap_or_else(|| PathBuf::from("logs"));
    std::fs::create_dir_all(&log_dir).map_err(|e| e.to_string())?;
    let log = std::fs::File::create(log_path).map_err(|e| e.to_string())?;
    let stderr_log = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = command_with_optional_cgroup(bin, &format!("restream-{}", ports.http));
    command
        .env("RESTREAM_HTTP_PORT", ports.http.to_string())
        .env("RESTREAM_RTMP_PORT", ports.rtmp.to_string())
        .env("RESTREAM_SRT_PORT", ports.srt.to_string())
        .env("RESTREAM_LOG_DIR", &log_dir)
        .env("RESTREAM_DB_PATH", db_path.to_string_lossy().to_string())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr_log))
        .kill_on_drop(true);
    for (key, value) in env_overrides {
        command.env(key, value);
    }
    if let Some(media_dir) = media_dir {
        command.env(
            "RESTREAM_MEDIA_DIR",
            absolute_path(media_dir)?.to_string_lossy().to_string(),
        );
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/healthz", ports.http),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut child).await;
        return Err(format!("restream did not become ready: {err}"));
    }
    if let Err(err) = wait_for_tcp_listener_ready(ports.rtmp, Duration::from_secs(10)).await {
        stop_child(&mut child).await;
        return Err(format!(
            "restream RTMP listener did not become ready: {err}"
        ));
    }
    Ok(child)
}

async fn run_burst_graph_check(api: &RampApi, pipeline_id: &str) -> Result<(bool, Value), String> {
    let graph = api
        .get_json(&format!("/api/v1/pipelines/{pipeline_id}/graph"))
        .await?;
    let readers = graph_ring_readers(&graph);
    let burst_ok = readers
        .iter()
        .filter(|r| {
            r["burstCount"].as_u64().unwrap_or(0) > 0
                && r["avgBurstSize"].as_f64().unwrap_or(0.0) > 0.0
        })
        .count();
    let passed = !readers.is_empty() && burst_ok == readers.len();
    let summary = json!({
        "readerCount": readers.len(),
        "burstOk": burst_ok,
    });
    Ok((passed, summary))
}

/// One ramp-family input/output profile.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RampConfig {
    name: &'static str,
    ingest_proto: &'static str,
    out_proto: &'static str,
    encoding: &'static str,
}

static RAMP_CONFIGS_FROM_DSL: OnceLock<Vec<RampConfig>> = OnceLock::new();

fn ramp_configs() -> &'static [RampConfig] {
    RAMP_CONFIGS_FROM_DSL.get_or_init(|| {
        serde_json::from_str::<Vec<RampConfig>>(include_str!("test_harness/ramp_configs.json"))
            .expect("embedded ramp_configs.json should define valid ramp rows")
    })
}

/// Runtime configuration and artifact paths for ramp-family runs.
struct RampEnv {
    work_dir: PathBuf,
    scale_log: PathBuf,
    summary_log: PathBuf,
    restream_log: PathBuf,
    mediamtx_log: PathBuf,
    mediamtx_config: PathBuf,
    restream_bin: PathBuf,
    restream_db_path: PathBuf,
    restream_http: u16,
    restream_rtmp: u16,
    restream_srt: u16,
    mtx_rtmp: u16,
    mtx_srt: u16,
    mtx_api: u16,
    n_outputs: usize,
    snap_every: usize,
    snapshot_sleep: Duration,
    cleanup_sleep: Duration,
}

impl RampEnv {
    fn from_env() -> Self {
        let work_dir = std::env::var_os("WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("test/artifacts/ramp"));
        let ports = harness_port_defaults();
        Self {
            scale_log: std::env::var_os("SCALE_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("scale.csv")),
            summary_log: std::env::var_os("SUMMARY_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("summary.txt")),
            restream_log: std::env::var_os("RAMP_RESTREAM_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("restream.log")),
            mediamtx_log: std::env::var_os("RAMP_MEDIAMTX_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("mediamtx.log")),
            mediamtx_config: std::env::var_os("RAMP_MEDIAMTX_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("mediamtx.yml")),
            restream_bin: default_restream_bin(),
            restream_db_path: std::env::var_os("RESTREAM_DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_work_db_path(&work_dir, "ramp.db")),
            restream_http: ports.restream_http,
            restream_rtmp: ports.restream_rtmp,
            restream_srt: ports.restream_srt,
            mtx_rtmp: ports.mtx_rtmp,
            mtx_srt: ports.mtx_srt,
            mtx_api: ports.mtx_api,
            n_outputs: env_usize("N_OUTPUTS", 10),
            snap_every: env_usize("SNAP_EVERY", 1).max(1),
            snapshot_sleep: Duration::from_secs(env_secs("SNAPSHOT_SLEEP_SECS", 3)),
            cleanup_sleep: Duration::from_secs(env_secs("RAMP_CONFIG_CLEANUP_SECS", 8)),
            work_dir,
        }
    }
}

/// Small authenticated HTTP client wrapper for the local restream API.
struct RampApi {
    client: reqwest::Client,
    base_url: String,
    cookie: Option<String>,
}

impl RampApi {
    fn new(http_port: u16) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: format!("http://127.0.0.1:{http_port}"),
            cookie: None,
        }
    }

    async fn login(&mut self) -> Result<(), String> {
        let response = self
            .client
            .post(format!("{}/api/v1/auth/login", self.base_url))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(r#"{"password":"admin"}"#)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("login failed with HTTP {}", response.status()));
        }
        self.cookie = response
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::to_string);
        if self.cookie.is_none() {
            return Err("login response did not include a session cookie".to_string());
        }
        Ok(())
    }

    async fn get_json(&self, path: &str) -> Result<Value, String> {
        let mut request = self.client.get(format!("{}{}", self.base_url, path));
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        json_response(request).await
    }

    async fn get_json_or_not_found(&self, path: &str) -> Result<Option<Value>, String> {
        let mut request = self.client.get(format!("{}{}", self.base_url, path));
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        let response = request.send().await.map_err(|e| e.to_string())?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|e| e.to_string())?;
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(format!(
                "HTTP {status}: {}",
                String::from_utf8_lossy(&bytes)
            ));
        }
        if bytes.is_empty() {
            return Ok(Some(Value::Null));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| e.to_string())
    }

    async fn get_text_response(&self, path: &str) -> Result<(reqwest::StatusCode, String), String> {
        let mut request = self.client.get(format!("{}{}", self.base_url, path));
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        let response = request.send().await.map_err(|e| e.to_string())?;
        let status = response.status();
        let body = response.text().await.map_err(|e| e.to_string())?;
        Ok((status, body))
    }

    async fn post_json(&self, path: &str, body: Value) -> Result<Value, String> {
        let mut request = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        json_response(request).await
    }

    async fn post_empty(&self, path: &str) -> Result<Value, String> {
        self.post_json(path, json!({})).await
    }

    async fn post_null(&self, path: &str) -> Result<Value, String> {
        self.post_json(path, Value::Null).await
    }

    async fn patch_json(&self, path: &str, body: Value) -> Result<Value, String> {
        let mut request = self
            .client
            .patch(format!("{}{}", self.base_url, path))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        json_response(request).await
    }

    async fn put_json(&self, path: &str, body: Value) -> Result<Value, String> {
        let mut request = self
            .client
            .put(format!("{}{}", self.base_url, path))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        json_response(request).await
    }

    async fn delete_json(&self, path: &str) -> Result<Value, String> {
        let mut request = self.client.delete(format!("{}{}", self.base_url, path));
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        json_response(request).await
    }
}

async fn get_logs(api: &RampApi, query: &str) -> Result<Vec<Value>, String> {
    let response = api.get_json(&format!("/api/v1/logs?{query}")).await?;
    response["logs"]
        .as_array()
        .cloned()
        .ok_or_else(|| format!("logs response missing array for query: {query}"))
}

fn log_event_type(log: &Value) -> Option<&str> {
    log["eventType"].as_str()
}

fn log_target(log: &Value) -> Option<&str> {
    log["target"].as_str()
}

fn log_message(log: &Value) -> Option<&str> {
    log["message"].as_str()
}

fn log_pipeline_id(log: &Value) -> Option<&str> {
    log["pipelineId"].as_str()
}

fn parse_log_fields(log: &Value) -> Option<Value> {
    let fields = log.get("fields")?;
    match fields {
        Value::Object(_) => Some(fields.clone()),
        Value::String(raw) if !raw.trim().is_empty() => serde_json::from_str(raw).ok(),
        _ => None,
    }
}

fn log_has_correlation_id(log: &Value) -> bool {
    parse_log_fields(log)
        .and_then(|fields| {
            fields
                .get("correlation_id")
                .and_then(|value| value.as_str())
                .or_else(|| fields.get("correlationId").and_then(|value| value.as_str()))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .is_some()
}

fn logs_contain_event(logs: &[Value], event_type: &str) -> bool {
    logs.iter()
        .any(|log| log_event_type(log) == Some(event_type))
}

async fn verify_api_smoke_history_contract(api: &RampApi) -> Result<Value, String> {
    let lifecycle_logs = get_logs(api, "event_class=lifecycle&limit=50&order=desc").await?;

    Ok(json!({
        "logsEndpointOk": true,
        "logCount": lifecycle_logs.len(),
    }))
}

async fn verify_live_history_contract(
    api: &RampApi,
    expected_event_types: &[&str],
) -> Result<Value, String> {
    let all_logs = get_logs(api, "limit=2000&order=desc").await?;

    let pipeline_logs: Vec<Value> = all_logs
        .iter()
        .filter(|log| log_pipeline_id(log).is_some())
        .cloned()
        .collect();
    if pipeline_logs.is_empty() {
        return Err("live history contract found no pipeline-scoped logs".to_string());
    }

    let missing_event_types: Vec<&str> = expected_event_types
        .iter()
        .copied()
        .filter(|event_type| !logs_contain_event(&pipeline_logs, event_type))
        .collect();
    if !missing_event_types.is_empty() {
        return Err(format!(
            "live history contract missing lifecycle events: {}",
            missing_event_types.join(", ")
        ));
    }

    let correlated_pipeline_log_count = pipeline_logs
        .iter()
        .filter(|log| log_has_correlation_id(log))
        .count();

    let ext_transcoder_logs: Vec<Value> = pipeline_logs
        .iter()
        .filter(|log| {
            log_target(log).is_some_and(|target| target.contains("external_transcoder"))
                || log_message(log).is_some_and(|message| message.contains("[ext-transcoder]"))
        })
        .cloned()
        .collect();
    let ext_transcoder_correlated = ext_transcoder_logs.iter().any(log_has_correlation_id);

    Ok(json!({
        "pipelineLogCount": pipeline_logs.len(),
        "expectedEventTypes": expected_event_types,
        "correlatedPipelineLogCount": correlated_pipeline_log_count,
        "externalTranscoderLogCount": ext_transcoder_logs.len(),
        "externalTranscoderCorrelated": ext_transcoder_correlated,
    }))
}

async fn verify_external_transcoder_history_contract(api: &RampApi) -> Result<Value, String> {
    let logs = get_logs(
        api,
        "target=restream::media::external_transcoder&limit=200&order=desc",
    )
    .await?;

    if logs.is_empty() {
        return Err(
            "external transcoder history contract found no restream::media::external_transcoder logs"
                .to_string(),
        );
    }

    let correlated_log_count = logs
        .iter()
        .filter(|log| log_has_correlation_id(log))
        .count();
    if correlated_log_count == 0 {
        return Err(
            "external transcoder history contract found no correlated stage logs".to_string(),
        );
    }

    Ok(json!({
        "targetLogCount": logs.len(),
        "correlatedLogCount": correlated_log_count,
    }))
}

fn harness_port_defaults() -> HarnessPortDefaults {
    *HARNESS_PORT_DEFAULTS.get_or_init(|| {
        let mut reserved = HashSet::new();
        HarnessPortDefaults {
            restream_http: env_or_allocated_port("RESTREAM_HTTP", 3030, &mut reserved),
            restream_rtmp: env_or_allocated_port("RESTREAM_RTMP", 1935, &mut reserved),
            restream_srt: env_or_allocated_port("RESTREAM_SRT", 10080, &mut reserved),
            mtx_rtmp: env_or_allocated_port("MTX_RTMP", 1936, &mut reserved),
            mtx_srt: env_or_allocated_port("MTX_SRT", 8891, &mut reserved),
            mtx_hls: env_or_allocated_port("MTX_HLS", 8890, &mut reserved),
            mtx_api: env_or_allocated_port("MTX_API", 9997, &mut reserved),
            sink: env_or_allocated_port_range("SINK_PORT", SINK_PORT, 256, &mut reserved),
            hls_put: env_or_allocated_port_range("HLS_PUT_PORT", 8990, 16, &mut reserved),
            ffmpeg_srt_sink_base: env_or_allocated_port_range(
                "FFMPEG_SRT_SINK_BASE",
                15_000,
                1024,
                &mut reserved,
            ),
            ffmpeg_signal_sink_base: env_or_allocated_port_range(
                "FFMPEG_SIGNAL_SINK_BASE",
                16_000,
                1024,
                &mut reserved,
            ),
        }
    })
}

fn env_or_allocated_port(name: &str, default: u16, reserved: &mut HashSet<u16>) -> u16 {
    env_or_allocated_port_range(name, default, 1, reserved)
}

fn env_or_allocated_port_range(
    name: &str,
    default: u16,
    width: u16,
    reserved: &mut HashSet<u16>,
) -> u16 {
    let width = width.max(1);
    if let Some(port) = std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
    {
        reserve_port_range(port, width, reserved);
        return port;
    }

    let port = synthesized_harness_port_range(name, width, reserved).unwrap_or(default);
    reserve_port_range(port, width, reserved);
    port
}

fn reserve_port_range(start: u16, width: u16, reserved: &mut HashSet<u16>) {
    let width = width.max(1) as u32;
    let start = start as u32;
    for offset in 0..width {
        let candidate = start + offset;
        if candidate > u16::MAX as u32 {
            break;
        }
        reserved.insert(candidate as u16);
    }
}

fn synthesized_harness_port_range(name: &str, width: u16, reserved: &HashSet<u16>) -> Option<u16> {
    // Do not probe-bind here: some restricted runners deny ad hoc socket
    // creation before the harness re-execs into its private loopback namespace.
    // A per-process high-port bundle is enough to avoid host collisions by
    // default while still allowing explicit env overrides when needed.
    let width = width.max(1) as u32;
    let min_port = 20_000u32;
    let max_port = 50_000u32;
    let span = max_port
        .checked_sub(min_port)?
        .checked_sub(width)?
        .checked_add(1)?;
    let pid = std::process::id();
    let name_hash = name.bytes().fold(0u32, |acc, byte| {
        acc.wrapping_mul(33).wrapping_add(byte as u32)
    });
    let base = min_port + pid.wrapping_mul(97).wrapping_add(name_hash) % span;
    for step in 0..1024u32 {
        let candidate = min_port + (base - min_port + step * 37) % span;
        let candidate = candidate as u16;
        let candidate_end = candidate as u32 + width;
        if candidate_end > max_port {
            continue;
        }
        if (0..width).all(|offset| !reserved.contains(&((candidate as u32 + offset) as u16))) {
            return Some(candidate);
        }
    }
    None
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn effective_fault_output_stall_siblings(
    configured_siblings: usize,
    n_per_group: Option<usize>,
) -> usize {
    let configured = configured_siblings.max(1);
    let n_per_group = n_per_group.unwrap_or(configured).max(1);
    configured.min(n_per_group)
}

fn fault_output_stall_sibling_count() -> usize {
    let configured = env_usize("FAULT_OUTPUT_STALL_SIBLINGS", 12);
    let n_per_group = std::env::var("N_PER_GROUP")
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    effective_fault_output_stall_siblings(configured, n_per_group)
}

// ── api-smoke (Phase 3) ─────────────────────────────────────────────────────
//
// Lightweight live test for the API/DB/lifecycle layer. No media — just spin up
// the binary, walk the API (auth, pipeline/output CRUD, start/stop), restart
// the child, and assert pipelines survived (DB persistence).

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

async fn ramp_family_correctness() -> Result<Value, String> {
    let env = RampEnv::from_env();
    if env.n_outputs == 0 {
        return Err("N_OUTPUTS must be greater than zero".to_string());
    }
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    ensure_ramp_artifacts(&env)?;

    let configs = selected_ramp_configs();
    if configs.is_empty() {
        return Err("RAMP_FAMILY_CONFIGS selected no ramp-family configs".to_string());
    }

    let mut mediamtx = start_ramp_mediamtx(&env).await?;
    let mut restream = start_ramp_restream(&env).await?;
    let mut api = RampApi::new(env.restream_http);
    api.login().await?;

    let mut case_results = Vec::with_capacity(configs.len());
    for config in configs {
        case_results.push(run_ramp_config(config, &env, &api, restream.id().unwrap_or(0)).await?);
    }

    stop_child(&mut restream).await;
    stop_child(&mut mediamtx).await;

    Ok(json!({
        "passed": true,
        "mode": "ramp-family",
        "configs": case_results,
        "artifacts": {
            "scaleCsv": env.scale_log,
            "summary": env.summary_log,
            "restreamLog": env.restream_log,
            "mediamtxLog": env.mediamtx_log,
        }
    }))
}

fn selected_ramp_configs() -> Vec<RampConfig> {
    let allow = std::env::var("RAMP_FAMILY_CONFIGS").ok().map(|value| {
        value
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>()
    });
    ramp_configs()
        .iter()
        .copied()
        .filter(|config| {
            allow
                .as_ref()
                .is_none_or(|items| items.iter().any(|item| item == config.name))
        })
        .collect()
}

fn ensure_ramp_artifacts(env: &RampEnv) -> Result<(), String> {
    if !env.scale_log.exists() {
        std::fs::write(
            &env.scale_log,
            "config,step,label,cpu_pct,rss_kb,ffmpeg_n,ffmpeg_rss_kb,total_rss_kb\n",
        )
        .map_err(|e| e.to_string())?;
    }
    if !env.summary_log.exists() {
        std::fs::write(&env.summary_log, "").map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn start_ramp_restream(env: &RampEnv) -> Result<Child, String> {
    start_restream_child(
        &env.restream_bin,
        &TestPorts {
            http: env.restream_http,
            rtmp: env.restream_rtmp,
            srt: env.restream_srt,
        },
        &env.restream_db_path,
        &env.restream_log,
    )
    .await
}

fn cleanup_ramp_db(path: &Path) {
    let path_string = path.to_string_lossy();
    let db_path = path_string
        .strip_prefix("sqlite:")
        .unwrap_or(path_string.as_ref())
        .split('?')
        .next()
        .unwrap_or("data.db");
    let db_path = PathBuf::from(db_path);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
    let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
}

async fn start_ramp_mediamtx(env: &RampEnv) -> Result<Child, String> {
    std::fs::write(
        &env.mediamtx_config,
        format!(
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nrtmp: yes\nrtmpAddress: :{}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{}\nhls: no\nwebrtc: no\napi: yes\napiAddress: :{}\nmetrics: no\npaths:\n  all:\n",
            env.mtx_rtmp, env.mtx_srt, env.mtx_api
        ),
    )
    .map_err(|e| e.to_string())?;
    let log = std::fs::File::create(&env.mediamtx_log).map_err(|e| e.to_string())?;
    let stderr_log = log.try_clone().map_err(|e| e.to_string())?;
    let mut child = Command::new("mediamtx")
        .arg(&env.mediamtx_config)
        .env_remove("MTX_RTMP")
        .env_remove("MTX_SRT")
        .env_remove("MTX_HLS")
        .env_remove("MTX_API")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr_log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/v3/paths/list", env.mtx_api),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut child).await;
        return Err(format!("mediamtx did not become ready: {err}"));
    }
    Ok(child)
}

async fn wait_for_http_ok(url: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let client = reqwest::Client::new();
    loop {
        if let Ok(response) = client.get(url).send().await
            && response.status().is_success()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {url}"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn proc_net_has_listening_port(contents: &str, port: u16) -> bool {
    let wanted_port = format!("{port:04X}");
    contents.lines().skip(1).any(|line| {
        let mut fields = line.split_whitespace();
        let _slot = fields.next();
        let Some(local_addr) = fields.next() else {
            return false;
        };
        let Some(state) = fields.nth(1) else {
            return false;
        };
        let Some((_, local_port)) = local_addr.rsplit_once(':') else {
            return false;
        };
        state == "0A" && local_port.eq_ignore_ascii_case(&wanted_port)
    })
}

fn tcp_listener_ready(port: u16) -> Result<bool, String> {
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                if proc_net_has_listening_port(&contents, port) {
                    return Ok(true);
                }
            }
            Err(err) => {
                return Err(format!("failed to read {path}: {err}"));
            }
        }
    }
    Ok(false)
}

async fn wait_for_tcp_listener_ready(port: u16, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if tcp_listener_ready(port)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "port {port} did not enter LISTEN state within {}s",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn start_local_mediamtx(
    config_path: &Path,
    log_path: &Path,
    ports: HarnessPortDefaults,
) -> Result<Child, String> {
    std::fs::write(
        config_path,
        format!(
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nrtmp: yes\nrtmpAddress: :{}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{}\nhls: yes\nhlsAddress: :{}\nhlsPartDuration: 200ms\nhlsSegmentDuration: 2s\nwebrtc: no\napi: yes\napiAddress: :{}\nmetrics: no\npaths:\n  all:\n",
            ports.mtx_rtmp, ports.mtx_srt, ports.mtx_hls, ports.mtx_api
        ),
    )
    .map_err(|e| e.to_string())?;
    let log = std::fs::File::create(log_path).map_err(|e| e.to_string())?;
    let stderr_log = log.try_clone().map_err(|e| e.to_string())?;
    let mut child = Command::new("mediamtx")
        .arg(config_path)
        .env_remove("MTX_RTMP")
        .env_remove("MTX_SRT")
        .env_remove("MTX_HLS")
        .env_remove("MTX_API")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr_log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/v3/paths/list", ports.mtx_api),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut child).await;
        return Err(format!("mediamtx did not become ready: {err}"));
    }
    Ok(child)
}

async fn run_ramp_config(
    config: RampConfig,
    env: &RampEnv,
    api: &RampApi,
    restream_pid: u32,
) -> Result<Value, String> {
    println!(
        "\n[ramp-family] {} {} ingest -> {} {} x{} outputs",
        config.name, config.ingest_proto, config.out_proto, config.encoding, env.n_outputs
    );
    let stream_key = format!("sk-{}", config.name);
    let pipeline_id = create_resource_pipeline(api, config.name, &stream_key).await?;

    let mut publisher = spawn_ramp_publisher(config, env, &stream_key).await?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(45)).await?;
    let baseline_snapshot = snapshot_ramp(env, restream_pid, config.name, 0, "baseline").await?;
    let rss_baseline = process_rss_kb(restream_pid).await.unwrap_or(0);

    let mut output_ids = Vec::with_capacity(env.n_outputs);
    for n in 1..=env.n_outputs {
        let url = match config.out_proto {
            "rtmp" => format!("rtmp://127.0.0.1:{}/live/{}-{n}", env.mtx_rtmp, config.name),
            "srt" => format!(
                "srt://127.0.0.1:{}?streamid=publish:live/{}-{n}",
                env.mtx_srt, config.name
            ),
            other => return Err(format!("unsupported ramp output protocol {other}")),
        };
        let output_id =
            create_output(api, &pipeline_id, &format!("out{n}"), &url, config.encoding).await?;
        start_output(api, &pipeline_id, &output_id).await?;
        output_ids.push(output_id);
        if n == 1 || n % env.snap_every == 0 {
            snapshot_ramp(env, restream_pid, config.name, n, &format!("out{n}")).await?;
        }
    }

    let rss_final = process_rss_kb(restream_pid).await.unwrap_or(0);
    let ffmpeg = ffmpeg_pipe1_stats().await;
    let rss_delta = rss_final.saturating_sub(rss_baseline);
    let per_output = rss_delta / env.n_outputs as u64;
    append_line(
        &env.summary_log,
        &format!(
            "{},rss_delta_kb={},per_output_kb={},ffmpeg_n={},ffmpeg_rss_kb={}\n",
            config.name, rss_delta, per_output, ffmpeg.count, ffmpeg.rss_kb
        ),
    )?;

    let expected = if config.encoding == "source" {
        "1920x1080"
    } else {
        "1280x720"
    };
    let first_url = read_url(config, env, 1);
    let last_url = read_url(config, env, env.n_outputs);
    let first_dims = check_ramp_stream("out1", &first_url, expected, 10).await;
    let last_dims =
        check_ramp_stream(&format!("out{}", env.n_outputs), &last_url, expected, 10).await;

    stop_child(&mut publisher).await;
    for output_id in &output_ids {
        let _ = api
            .post_null(&format!(
                "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop"
            ))
            .await;
    }
    tokio::time::sleep(env.cleanup_sleep).await;

    Ok(json!({
        "config": config.name,
        "pipelineId": pipeline_id,
        "outputs": output_ids.len(),
        "baseline": baseline_snapshot,
        "rssDeltaKb": rss_delta,
        "perOutputKb": per_output,
        "ffmpegCount": ffmpeg.count,
        "ffmpegRssKb": ffmpeg.rss_kb,
        "spotChecks": {
            "first": {"expected": expected, "got": first_dims},
            "last": {"expected": expected, "got": last_dims},
        }
    }))
}

async fn spawn_ramp_publisher(
    config: RampConfig,
    env: &RampEnv,
    stream_key: &str,
) -> Result<Child, String> {
    let fixture = ramp_fixture()?;
    let (url, format) = match config.ingest_proto {
        "rtmp" => (
            format!("rtmp://127.0.0.1:{}/live/{stream_key}", env.restream_rtmp),
            "flv",
        ),
        "srt" => (
            format!(
                "srt://127.0.0.1:{}?streamid=publish:live/{stream_key}&latency=200000",
                env.restream_srt
            ),
            "mpegts",
        ),
        other => return Err(format!("unsupported ramp ingest protocol {other}")),
    };
    spawn_publisher_with_selection(
        &fixture,
        &url,
        format,
        PublishTrackSelection::PrimaryAv,
        None,
    )
}

async fn wait_for_api_input_live(
    api: &RampApi,
    pipeline_id: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(health) = api.get_json("/api/v1/engine/health").await
            && health["pipelines"][pipeline_id]["input"]["status"] == "on"
            && health["pipelines"][pipeline_id]["input"]["bytesReceived"]
                .as_u64()
                .unwrap_or(0)
                > 0
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{pipeline_id}: ingest did not go live within {}s",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn wait_for_api_input_media_ready(
    api: &RampApi,
    pipeline_id: &str,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    let mut last_snapshot = Value::Null;

    loop {
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            let snapshot = health["pipelines"][pipeline_id].clone();
            if !snapshot.is_null() {
                last_snapshot = snapshot.clone();
                let input = &snapshot["input"];
                let input_live =
                    input["status"] == "on" && input["bytesReceived"].as_u64().unwrap_or(0) > 0;
                let has_video = !input["video"].is_null();
                let has_audio = input["audioTracks"]
                    .as_array()
                    .map(|tracks| !tracks.is_empty())
                    .unwrap_or(false);
                if input_live && has_video && has_audio {
                    return Ok(snapshot);
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{pipeline_id}: ingest went live but media probe was incomplete within {}s; last snapshot={}",
                timeout.as_secs(),
                last_snapshot
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn install_bframe_transcode_profiles(api: &RampApi) -> Result<(), String> {
    let settings = api.get_json("/api/v1/settings").await?;
    let mut profiles: restream::domain::transcode_profile::TranscodeProfiles =
        serde_json::from_value(settings["transcodeProfiles"].clone())
            .map_err(|error| format!("parse transcode profiles: {error}"))?;

    for (name, bframes) in [("h264_bf0", 0usize), ("h264_bf2", 2usize)] {
        profiles.insert(
            name.to_string(),
            restream::domain::transcode_profile::TranscodeProfile {
                preset: "veryfast".to_string(),
                tune: String::new(),
                crf: 23,
                gop: 60,
                bframes,
                bitrate: 0,
                max_bitrate: 0,
                width: 0,
                height: 0,
            },
        );
    }

    api.patch_json("/api/v1/settings", json!({ "transcodeProfiles": profiles }))
        .await?;
    Ok(())
}

/// Expected presence of B-frame signal in a generated/probed stream.
#[derive(Clone, Copy)]
enum ExpectedBframeSignal {
    None,
    Present,
}

async fn run_transcode_bframe_probe_case(
    api: &RampApi,
    pipeline_id: &str,
    work_dir: &Path,
    mediamtx_rtmp_port: u16,
    label: &str,
    encoding: &str,
    expected_signal: ExpectedBframeSignal,
) -> Result<Value, String> {
    let stream_name = format!("e2e-bframe-{label}");
    let publish_url = format!("rtmp://127.0.0.1:{mediamtx_rtmp_port}/live/{stream_name}");
    let output_id = create_output(api, pipeline_id, label, &publish_url, encoding).await?;
    if let Err(error) = start_output(api, pipeline_id, &output_id).await {
        stop_mixed_outputs(api, pipeline_id, std::slice::from_ref(&output_id)).await;
        return Err(format!("{label}: start output failed: {error}"));
    }

    let probe = wait_for_probe_shape(
        label,
        &publish_url,
        None,
        "h264",
        1,
        Duration::from_secs(30),
    )
    .await;
    let packet_path = work_dir.join(format!("{label}-packets.json"));
    let packet_probe = ffprobe_video_packets(&publish_url, &packet_path).await;
    stop_mixed_outputs(api, pipeline_id, std::slice::from_ref(&output_id)).await;

    let probe = probe?;
    let packet_probe = packet_probe?;
    let packet_count = count_video_packets(&packet_probe);
    let bframe_count = count_bframe_packets(&packet_probe);
    let dts_monotone = video_dts_monotone(&packet_probe);
    let bframe_signal_ok = match expected_signal {
        ExpectedBframeSignal::None => bframe_count == 0,
        ExpectedBframeSignal::Present => bframe_count > 0,
    };
    let passed = packet_count >= 30 && dts_monotone && bframe_signal_ok;

    let mut result = json!({
        "passed": passed,
        "encoding": encoding,
        "readUrl": publish_url,
        "packetArtifact": packet_path,
        "packetCount": packet_count,
        "bframeCount": bframe_count,
        "dtsMonotone": dts_monotone,
        "expectedBframes": match expected_signal {
            ExpectedBframeSignal::None => 0,
            ExpectedBframeSignal::Present => 2,
        },
        "probe": probe,
    });
    if packet_count < 30 {
        result["error"] = json!(format!(
            "{label}: expected at least 30 video packets, got {packet_count}"
        ));
    } else if !bframe_signal_ok {
        result["error"] = match expected_signal {
            ExpectedBframeSignal::None => {
                json!(format!("{label}: expected no packets with PTS > DTS"))
            }
            ExpectedBframeSignal::Present => {
                json!(format!("{label}: expected packets with PTS > DTS"))
            }
        };
    } else if !dts_monotone {
        result["error"] = json!(format!("{label}: DTS values are not monotone"));
    }

    if passed {
        Ok(result)
    } else {
        Err(format!("{label}: transcode B-frame probe failed: {result}"))
    }
}

async fn wait_for_output_stalled_status(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    timeout: Duration,
) -> Result<(Value, Value), String> {
    let deadline = Instant::now() + timeout;
    let mut last_status = Value::Null;
    let mut last_health = Value::Null;

    loop {
        if let Ok(status) = api
            .get_json(&format!(
                "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/status"
            ))
            .await
        {
            last_status = status.clone();
            if let Ok(health) = api.get_json("/api/v1/engine/health").await
                && let Some(output) = health["pipelines"][pipeline_id]["outputs"]
                    .as_object()
                    .and_then(|outputs| outputs.get(output_id).cloned())
            {
                last_health = output.clone();
                let stalled_visible = status["status"].as_str() == Some("stalled")
                    && output["status"].as_str() == Some("stalled")
                    && status["rawStatus"].as_str() == Some("running")
                    && output["rawStatus"].as_str() == Some("running")
                    && !status["retrying"].as_bool().unwrap_or(false)
                    && !output["retrying"].as_bool().unwrap_or(false)
                    && status["lastError"].is_null()
                    && output["lastError"].is_null()
                    && status["failurePhase"].is_null()
                    && output["failurePhase"].is_null()
                    && status["startedAt"].is_string()
                    && output["startedAt"] == status["startedAt"]
                    && output["targetAddr"] == status["targetAddr"]
                    && output["totalSize"] == status["totalSize"];
                let stale_age_visible = match status["lastProgressAgeMs"].as_u64() {
                    Some(age_ms) => age_ms >= 10_000,
                    None => status["lastProgressAt"].is_null(),
                };
                if stalled_visible && stale_age_visible {
                    return Ok((status, output));
                }
            }
        }

        if Instant::now() >= deadline {
            return Err(format!(
                "{pipeline_id}/{output_id}: output status did not surface stalled state within {}s; last_status={} last_health={}",
                timeout.as_secs(),
                last_status,
                last_health
            ));
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_api_input_off(
    api: &RampApi,
    pipeline_id: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            let status = health["pipelines"][pipeline_id]["input"]["status"]
                .as_str()
                .unwrap_or("unknown");
            if status == "off" {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{pipeline_id}: ingest did not go off within {}s",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn wait_for_api_recording_state(
    api: &RampApi,
    pipeline_id: &str,
    expected_active: bool,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let health = api.get_json("/api/v1/engine/health").await?;
        let recording = &health["pipelines"][pipeline_id]["recording"];
        let enabled = recording["enabled"].as_bool().unwrap_or(false);
        let active = recording["active"].as_bool().unwrap_or(false);
        if active == expected_active {
            return Ok(json!({
                "enabled": enabled,
                "active": active,
            }));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "recording state for pipeline {pipeline_id} did not reach active={expected_active}; enabled={enabled} active={active}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn wait_for_api_hls_preview_state(
    api: &RampApi,
    pipeline_id: &str,
    expected_active: bool,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let health = api.get_json("/api/v1/engine/health").await?;
        let preview = &health["pipelines"][pipeline_id]["hlsPreview"];
        let active = preview["active"].as_bool().unwrap_or(false);
        if active == expected_active {
            return Ok(preview.clone());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "HLS preview state for pipeline {pipeline_id} did not reach active={expected_active}; preview={preview}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn wait_for_pipeline_file_ingest_running_state(
    api: &RampApi,
    pipeline_id: &str,
    expected_running: bool,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let ingest = api
            .get_json(&format!("/api/v1/pipelines/{pipeline_id}/file-ingest"))
            .await?;
        let running = ingest["running"].as_bool().unwrap_or(false);
        if running == expected_running {
            return Ok(ingest);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "file ingest state for pipeline {pipeline_id} did not reach running={expected_running}; ingest={ingest}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn wait_for_hls_playlist_ready(
    api: &RampApi,
    pipeline_id: &str,
    timeout: Duration,
) -> Result<(reqwest::StatusCode, String), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let (status, body) = api
            .get_text_response(&format!("/hls/{pipeline_id}/master.m3u8"))
            .await?;
        if status.is_success() && body.contains("#EXTM3U") {
            return Ok((status, body));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "HLS playlist for pipeline {pipeline_id} did not become ready within {}s; last_status={} body={body}",
                timeout.as_secs(),
                status
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// One ramp-family resource snapshot written to CSV and JSON summaries.
struct RampSnapshot {
    cpu_pct: String,
    rss_kb: u64,
    ffmpeg_count: u64,
    ffmpeg_rss_kb: u64,
}

async fn snapshot_ramp(
    env: &RampEnv,
    restream_pid: u32,
    config: &str,
    step: usize,
    label: &str,
) -> Result<Value, String> {
    if !env.snapshot_sleep.is_zero() {
        tokio::time::sleep(env.snapshot_sleep).await;
    }
    let ffmpeg = ffmpeg_pipe1_stats().await;
    let snapshot = RampSnapshot {
        cpu_pct: process_cpu_pct(restream_pid)
            .await
            .unwrap_or_else(|| "0".to_string()),
        rss_kb: process_rss_kb(restream_pid).await.unwrap_or(0),
        ffmpeg_count: ffmpeg.count,
        ffmpeg_rss_kb: ffmpeg.rss_kb,
    };
    let total = snapshot.rss_kb + snapshot.ffmpeg_rss_kb;
    append_line(
        &env.scale_log,
        &format!(
            "{config},{step},\"{label}\",{},{},{},{},{}\n",
            snapshot.cpu_pct, snapshot.rss_kb, snapshot.ffmpeg_count, snapshot.ffmpeg_rss_kb, total
        ),
    )?;
    println!(
        "  {step:<4} {label:<20} cpu={} rss={} KB ffmpeg#={} ffmpeg_rss={} KB total={} KB",
        snapshot.cpu_pct, snapshot.rss_kb, snapshot.ffmpeg_count, snapshot.ffmpeg_rss_kb, total
    );
    Ok(json!({
        "step": step,
        "label": label,
        "cpuPct": snapshot.cpu_pct,
        "rssKb": snapshot.rss_kb,
        "ffmpegCount": snapshot.ffmpeg_count,
        "ffmpegRssKb": snapshot.ffmpeg_rss_kb,
        "totalRssKb": total,
    }))
}

fn append_line(path: &Path, line: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(line.as_bytes()).map_err(|e| e.to_string())
}

/// Count and RSS total for external FFmpeg worker processes.
#[derive(Clone)]
struct FfmpegStats {
    count: u64,
    rss_kb: u64,
    pids: Vec<u32>,
}

async fn ffmpeg_pipe1_stats() -> FfmpegStats {
    let output = Command::new("ps").arg("aux").output().await;
    let Ok(output) = output else {
        return FfmpegStats {
            count: 0,
            rss_kb: 0,
            pids: Vec::new(),
        };
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut count = 0;
    let mut rss_kb = 0;
    for line in text.lines() {
        if line.contains("ffmpeg") && line.contains("pipe:1") {
            count += 1;
            rss_kb += line
                .split_whitespace()
                .nth(5)
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
        }
    }
    FfmpegStats {
        count,
        rss_kb,
        pids: Vec::new(),
    }
}

async fn process_cpu_pct(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "%cpu="])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some(if value.is_empty() {
        "0".to_string()
    } else {
        value
    })
}

async fn process_rss_kb(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "rss="])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn read_url(config: RampConfig, env: &RampEnv, output_index: usize) -> String {
    match config.out_proto {
        "rtmp" => format!(
            "rtmp://127.0.0.1:{}/live/{}-{output_index}",
            env.mtx_rtmp, config.name
        ),
        "srt" => format!(
            "srt://127.0.0.1:{}?streamid=read:live/{}-{output_index}&timeout=30000000",
            env.mtx_srt, config.name
        ),
        _ => String::new(),
    }
}

async fn check_ramp_stream(
    label: &str,
    url: &str,
    expected: &str,
    retries: usize,
) -> Option<String> {
    let mut last = None;
    for _ in 0..retries {
        if let Ok(dimensions) = probe_dims_ramp(url).await {
            if dimensions == expected {
                println!("  ok   {label:<45} -> {dimensions}");
                return Some(dimensions);
            }
            if !dimensions.is_empty() {
                last = Some(dimensions);
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!(
        "  FAIL {label:<45} expected={expected} got={}",
        last.as_deref().unwrap_or("none")
    );
    last
}

async fn probe_dims_ramp(url: &str) -> Result<String, String> {
    probe_dims_ramp_with_cookie(url, None).await
}

/// Minimal HLS playlist progress marker used by live-edge checks.
#[derive(Clone, Debug)]
struct HlsPlaylistSnapshot {
    media_sequence: Option<u64>,
    last_segment: Option<String>,
}

fn parse_hls_playlist_snapshot(body: &str) -> HlsPlaylistSnapshot {
    let media_sequence = body
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
        .and_then(|value| value.trim().parse::<u64>().ok());
    let last_segment = body
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| line.trim().to_string());

    HlsPlaylistSnapshot {
        media_sequence,
        last_segment,
    }
}

async fn probe_dims_ramp_with_cookie(url: &str, cookie: Option<&str>) -> Result<String, String> {
    let mut command = Command::new("ffprobe");
    command.args([
        "-v",
        "error",
        "-probesize",
        "10000000",
        "-analyzeduration",
        "10000000",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height",
        "-of",
        "csv=p=0",
    ]);
    if let Some(cookie) = cookie {
        command.args(["-headers", &format!("Cookie: {cookie}\r\n")]);
    }
    let child = command
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .map_err(|_| format!("ffprobe timed out: {url}"))?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe failed: {url}: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .replace(',', "x"))
}

fn srt_publish_url(port: u16, stream_key: &str, crypto: Option<(&str, u32)>) -> String {
    let mut url =
        format!("srt://127.0.0.1:{port}?streamid=publish:live/{stream_key}&pkt_size=1316");
    if let Some((passphrase, pbkeylen)) = crypto {
        url.push_str(&format!("&passphrase={passphrase}&pbkeylen={pbkeylen}"));
    }
    url
}

fn srt_read_url(port: u16, stream_key: &str, crypto: Option<(&str, u32)>) -> String {
    let mut url = format!(
        "srt://127.0.0.1:{port}?streamid=read:live/{stream_key}&mode=caller&transtype=live&latency=100"
    );
    if let Some((passphrase, pbkeylen)) = crypto {
        url.push_str(&format!("&passphrase={passphrase}&pbkeylen={pbkeylen}"));
    }
    url
}

async fn expect_ingest_rejected(
    api: &RampApi,
    pipeline_id: &str,
    fixture: &Path,
    publish_url: &str,
    label: &str,
) -> Result<Value, String> {
    let mut publisher = spawn_publisher(fixture, publish_url, "mpegts", true).await?;
    tokio::time::sleep(Duration::from_secs(4)).await;
    let live = wait_for_api_input_live(api, pipeline_id, Duration::from_secs(1))
        .await
        .is_ok();
    stop_child(&mut publisher).await;
    if live {
        return Err(format!("{label}: ingest unexpectedly went live"));
    }
    wait_for_api_input_off(api, pipeline_id, Duration::from_secs(5)).await?;
    Ok(json!({"passed": true, "label": label}))
}

async fn expect_srt_read_failure(url: &str, label: &str) -> Result<Value, String> {
    match ffprobe(url).await {
        Ok(probe) => Err(format!("{label}: read unexpectedly succeeded: {probe}")),
        Err(error) => Ok(json!({"passed": true, "label": label, "error": error})),
    }
}

async fn create_srt_policy_pipeline(
    api: &RampApi,
    name: &str,
    policy: Value,
) -> Result<String, String> {
    create_srt_policy_pipeline_with_key(api, name, name, policy).await
}

async fn create_srt_policy_pipeline_with_key(
    api: &RampApi,
    name: &str,
    stream_key: &str,
    policy: Value,
) -> Result<String, String> {
    let pipeline = api
        .post_json(
            "/api/v1/pipelines",
            json!({"name": name, "streamKey": stream_key, "srtIngestPolicy": policy}),
        )
        .await?;
    pipeline["pipeline"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{name} pipeline id missing"))
}

async fn srt_policy_correctness() -> Result<Value, String> {
    let work_dir = artifact_path("srt.policy");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let ports = TestPorts::from_env();

    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture = checked_h264_fixture()?;

    let mut results = serde_json::Map::new();

    api.patch_json(
        "/api/v1/settings",
        json!({"srtIngest": {"mode": "plaintext", "pbkeylen": 16, "passphrase": null}}),
    )
    .await?;
    let plain_inherit_id =
        create_srt_policy_pipeline(&api, "policy-plain-inherit", json!({"mode": "inherit"}))
            .await?;
    let mut plain_pub = spawn_publisher(
        &fixture,
        &srt_publish_url(ports.srt, "policy-plain-inherit", None),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(&api, &plain_inherit_id, Duration::from_secs(15)).await?;
    let plain_read_probe = ffprobe(&srt_read_url(ports.srt, "policy-plain-inherit", None)).await?;
    assert_media_only(&plain_read_probe, "plain inherit read")?;
    stop_child(&mut plain_pub).await;
    wait_for_api_input_off(&api, &plain_inherit_id, Duration::from_secs(10)).await?;
    results.insert(
        "globalPlaintextInherit".to_string(),
        json!({"passed": true, "readProbe": plain_read_probe}),
    );

    api.patch_json(
        "/api/v1/settings",
        json!({"srtIngest": {"mode": "encrypted", "passphrase": "globalpass123", "pbkeylen": 16}}),
    )
    .await?;
    let global_enc_id =
        create_srt_policy_pipeline(&api, "policy-global-enc", json!({"mode": "inherit"})).await?;
    let mut global_enc_pub = spawn_publisher(
        &fixture,
        &srt_publish_url(ports.srt, "policy-global-enc", Some(("globalpass123", 16))),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(&api, &global_enc_id, Duration::from_secs(15)).await?;
    let global_enc_read = ffprobe(&srt_read_url(
        ports.srt,
        "policy-global-enc",
        Some(("globalpass123", 16)),
    ))
    .await?;
    assert_media_only(&global_enc_read, "global encrypted read")?;
    let global_enc_read_fail = expect_srt_read_failure(
        &srt_read_url(ports.srt, "policy-global-enc", None),
        "global encrypted plaintext read",
    )
    .await?;
    stop_child(&mut global_enc_pub).await;
    wait_for_api_input_off(&api, &global_enc_id, Duration::from_secs(10)).await?;
    let global_enc_publish_fail = expect_ingest_rejected(
        &api,
        &global_enc_id,
        &fixture,
        &srt_publish_url(ports.srt, "policy-global-enc", None),
        "global encrypted plaintext publish",
    )
    .await?;
    results.insert(
        "globalEncrypted16Inherit".to_string(),
        json!({
            "passed": true,
            "readProbe": global_enc_read,
            "plaintextReadRejected": global_enc_read_fail,
            "plaintextPublishRejected": global_enc_publish_fail,
        }),
    );

    let plain_override_id =
        create_srt_policy_pipeline(&api, "policy-plain-override", json!({"mode": "plaintext"}))
            .await?;
    let mut plain_override_pub = spawn_publisher(
        &fixture,
        &srt_publish_url(ports.srt, "policy-plain-override", None),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(&api, &plain_override_id, Duration::from_secs(15)).await?;
    let plain_override_read =
        ffprobe(&srt_read_url(ports.srt, "policy-plain-override", None)).await?;
    assert_media_only(&plain_override_read, "plain override read")?;
    stop_child(&mut plain_override_pub).await;
    wait_for_api_input_off(&api, &plain_override_id, Duration::from_secs(10)).await?;
    results.insert(
        "globalEncrypted16PipelinePlaintext".to_string(),
        json!({"passed": true, "readProbe": plain_override_read}),
    );

    api.patch_json(
        "/api/v1/settings",
        json!({"srtIngest": {"mode": "plaintext", "pbkeylen": 16, "passphrase": null}}),
    )
    .await?;
    for (label, stream_key, passphrase, pbkeylen) in [
        (
            "pipelineEncrypted24",
            "policy-enc-24",
            "pipepass1234",
            24u32,
        ),
        (
            "pipelineEncrypted32",
            "policy-enc-32",
            "pipepass12345",
            32u32,
        ),
    ] {
        let pipeline_id = create_srt_policy_pipeline_with_key(
            &api,
            label,
            stream_key,
            json!({"mode": "encrypted", "passphrase": passphrase, "pbkeylen": pbkeylen}),
        )
        .await?;
        let mut pub_ok = spawn_publisher(
            &fixture,
            &srt_publish_url(ports.srt, stream_key, Some((passphrase, pbkeylen))),
            "mpegts",
            true,
        )
        .await?;
        wait_for_api_input_live(&api, &pipeline_id, Duration::from_secs(15)).await?;
        let read_ok = ffprobe(&srt_read_url(
            ports.srt,
            stream_key,
            Some((passphrase, pbkeylen)),
        ))
        .await?;
        assert_media_only(&read_ok, label)?;
        let read_plain_fail = expect_srt_read_failure(
            &srt_read_url(ports.srt, stream_key, None),
            &format!("{label} plaintext read"),
        )
        .await?;
        let read_wrong_pass_fail = expect_srt_read_failure(
            &srt_read_url(ports.srt, stream_key, Some(("wrongpass123", pbkeylen))),
            &format!("{label} wrong passphrase read"),
        )
        .await?;
        stop_child(&mut pub_ok).await;
        wait_for_api_input_off(&api, &pipeline_id, Duration::from_secs(10)).await?;
        let publish_plain_fail = expect_ingest_rejected(
            &api,
            &pipeline_id,
            &fixture,
            &srt_publish_url(ports.srt, stream_key, None),
            &format!("{label} plaintext publish"),
        )
        .await?;
        results.insert(
            label.to_string(),
            json!({
                "passed": true,
                "readProbe": read_ok,
                "plaintextReadRejected": read_plain_fail,
                "wrongPassphraseReadRejected": read_wrong_pass_fail,
                "plaintextPublishRejected": publish_plain_fail,
            }),
        );
    }

    stop_child(&mut child).await;
    let value = Value::Object(results);
    let path = work_dir.join("results.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    Ok(value)
}

fn probe_audio_track_count(probe: &Value) -> usize {
    probe["streams"]
        .as_array()
        .map(|streams| {
            streams
                .iter()
                .filter(|s| s["codec_type"] == "audio")
                .count()
        })
        .unwrap_or(0)
}

fn video_dimensions(probe: &Value) -> Option<String> {
    let stream = probe["streams"]
        .as_array()?
        .iter()
        .find(|stream| stream["codec_type"] == "video")?;
    Some(format!(
        "{}x{}",
        stream["width"].as_i64()?,
        stream["height"].as_i64()?
    ))
}

fn video_codec_name(probe: &Value) -> Option<String> {
    probe["streams"]
        .as_array()?
        .iter()
        .find(|stream| stream["codec_type"] == "video")?["codec_name"]
        .as_str()
        .map(str::to_string)
}

fn graph_ring_readers(graph: &Value) -> Vec<Value> {
    graph["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| node["type"] == "ring_buffer")
        .flat_map(|node| {
            node["details"]["readers"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

fn graph_active_node_count(graph: &Value, node_type: &str) -> usize {
    graph["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| node["type"] == node_type && node["active"].as_bool().unwrap_or(false))
        .count()
}

async fn wait_for_probe_shape(
    label: &str,
    url: &str,
    expected_dimensions: Option<&str>,
    expected_video_codec: &str,
    expected_audio_tracks: usize,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    let mut last_probe = json!({});
    let mut last_error = String::new();
    loop {
        match ffprobe(url).await {
            Ok(probe) => {
                let dimensions = video_dimensions(&probe).unwrap_or_default();
                let codec = video_codec_name(&probe).unwrap_or_default();
                let audio_tracks = probe_audio_track_count(&probe);
                let dimensions_ok =
                    expected_dimensions.is_none_or(|expected| dimensions == expected);
                if dimensions_ok
                    && codec == expected_video_codec
                    && audio_tracks == expected_audio_tracks
                {
                    return Ok(probe);
                }
                last_probe = json!({
                    "dimensions": dimensions,
                    "videoCodec": codec,
                    "audioTracks": audio_tracks,
                    "probe": probe,
                });
            }
            Err(error) => {
                last_error = error;
            }
        }

        if Instant::now() >= deadline {
            return Err(format!(
                "{label}: expected codec={expected_video_codec} audio_tracks={expected_audio_tracks} dimensions={:?}; last_probe={last_probe}; last_error={last_error}",
                expected_dimensions
            ));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Test: RTMP B-frame ingest -> RTMP egress timestamp round-trip.
///
/// Publishes B-frame H.264/AAC over RTMP, sends egress to the generalized
/// harness sink, and verifies ffprobe observes composition offsets (PTS > DTS)
/// while DTS stays monotone.
async fn bframe_rtmp_correctness() -> Result<Value, String> {
    let work_dir = artifact_path("timestamp.bframe");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let mediamtx_config = work_dir.join("mediamtx.yml");
    let mediamtx_log = work_dir.join("mediamtx.log");
    let all_ports = harness_port_defaults();
    let sink_port = harness_port_defaults().sink;
    let ports = TestPorts::from_env();

    let mut mediamtx = start_local_mediamtx(&mediamtx_config, &mediamtx_log, all_ports).await?;
    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let pipeline_id =
        create_pipeline_with_stream_key(&api, "B-frame RTMP source", "e2e-bframe-src").await?;

    // Create RTMP egress output pointed at the harness sink
    let sink_url = format!("rtmp://127.0.0.1:{sink_port}/live/e2e-bframe-sink");
    let output_id = create_output(&api, &pipeline_id, "bframe-sink", &sink_url, "source").await?;

    // Start generalized sink
    let sink_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink_server = start_generalized_sink_server(sink_port, sink_metrics.clone()).await?;

    let fixture = checked_h264_fixture()?;

    let mut publisher = spawn_publisher(
        &fixture,
        &format!("rtmp://127.0.0.1:{}/live/e2e-bframe-src", ports.rtmp),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(&api, &pipeline_id, Duration::from_secs(15)).await?;
    println!("[timestamp.bframe] Source ingest established");

    // Start the output
    start_output(&api, &pipeline_id, &output_id).await?;

    // Wait for sink to accumulate packets
    let deadline = Instant::now() + Duration::from_secs(15);
    while sink_metrics.video_count.load(Ordering::Relaxed) < 30 {
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Also probe via ffprobe for B-frame packet-level analysis
    let packets_path = work_dir.join("bframe-packets.json");
    let read_url = format!("rtmp://127.0.0.1:{}/live/e2e-bframe-src", ports.rtmp);
    let packet_probe = ffprobe_video_packets(&read_url, &packets_path).await?;
    let packet_count = count_video_packets(&packet_probe);
    let bframe_count = count_bframe_packets(&packet_probe);
    let ffprobe_dts_monotone = video_dts_monotone(&packet_probe);

    let sink_dts_monotone = sink_metrics.dts_monotone();
    let video_count = sink_metrics.video_count.load(Ordering::Relaxed);
    let sink_summary = sink_metrics.summary();

    let source_passed =
        packet_count >= 30 && bframe_count > 0 && ffprobe_dts_monotone && sink_dts_monotone;
    let mut source_results = json!({
        "passed": source_passed,
        "packetCount": packet_count,
        "bframeCount": bframe_count,
        "ffprobeDtsMonotone": ffprobe_dts_monotone,
        "sinkDtsMonotone": sink_dts_monotone,
        "sinkVideoCount": video_count,
        "sink": sink_summary,
    });
    if packet_count < 30 {
        source_results["error"] = json!(format!(
            "expected at least 30 video packets, got {packet_count}"
        ));
    } else if bframe_count == 0 {
        source_results["error"] = json!("RTMP egress did not expose any packets with PTS > DTS");
    } else if !ffprobe_dts_monotone || !sink_dts_monotone {
        source_results["error"] = json!("RTMP egress DTS values are not monotone");
    }

    install_bframe_transcode_profiles(&api).await?;
    let transcode_bframes_0 = run_transcode_bframe_probe_case(
        &api,
        &pipeline_id,
        &work_dir,
        all_ports.mtx_rtmp,
        "h264-bf0",
        "h264_bf0",
        ExpectedBframeSignal::None,
    )
    .await?;
    let transcode_bframes_2 = run_transcode_bframe_probe_case(
        &api,
        &pipeline_id,
        &work_dir,
        all_ports.mtx_rtmp,
        "h264-bf2",
        "h264_bf2",
        ExpectedBframeSignal::Present,
    )
    .await?;

    stop_child(&mut publisher).await;
    stop_generalized_sink_server(sink_server);
    stop_child(&mut child).await;
    stop_child(&mut mediamtx).await;

    let passed = source_passed
        && transcode_bframes_0["passed"].as_bool().unwrap_or(false)
        && transcode_bframes_2["passed"].as_bool().unwrap_or(false);
    let results = json!({
        "passed": passed,
        "sourcePassthrough": source_results,
        "transcodeBframes0": transcode_bframes_0,
        "transcodeBframes2": transcode_bframes_2,
    });

    let path = work_dir.join("results.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&results).unwrap())
        .map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&results).unwrap());
    if passed {
        Ok(results)
    } else {
        Err(format!("RTMP B-frame round-trip failed: {results}"))
    }
}

async fn run_file_live_edge_case(
    api: &mut RampApi,
    ports: &TestPorts,
    media_dir: &Path,
    fixture: &Path,
    case_id: &str,
    live_optimized: bool,
    target_gop_seconds: u32,
) -> Result<Value, String> {
    let fixture_name = format!(
        "{case_id}-{}",
        fixture
            .file_name()
            .ok_or("fixture missing file name")?
            .to_string_lossy()
    );
    let media_dest = media_dir.join(&fixture_name);
    std::fs::copy(fixture, &media_dest).map_err(|e| e.to_string())?;

    let pipeline = api
        .post_json(
            "/api/v1/pipelines",
            json!({"name": case_id, "streamKey": case_id}),
        )
        .await?;
    let pipeline_id = pipeline["pipeline"]["id"]
        .as_str()
        .ok_or("pipeline create response missing pipeline.id")?
        .to_string();

    api.put_json(
        &format!("/api/v1/pipelines/{pipeline_id}/file-ingest"),
        json!({
            "filename": fixture_name,
            "loop": true,
            "liveOptimized": live_optimized,
            "targetGopSeconds": target_gop_seconds,
        }),
    )
    .await?;

    let source_analysis = api
        .get_json(&format!("/api/v1/media/{}/analysis", fixture_name))
        .await?;

    let ingest = api
        .get_json(&format!("/api/v1/pipelines/{pipeline_id}/file-ingest"))
        .await?;
    let ingest_id = ingest["id"]
        .as_str()
        .ok_or("pipeline file ingest missing id")?
        .to_string();

    api.post_empty(&format!("/api/v1/ingests/{ingest_id}/start"))
        .await?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(30)).await?;
    wait_for_pipeline_file_ingest_running_state(api, &pipeline_id, true, Duration::from_secs(10))
        .await?;

    let playlist_url = format!(
        "http://127.0.0.1:{}/hls/{pipeline_id}/master.m3u8",
        ports.http
    );
    let (_playlist_status, playlist_body) =
        wait_for_hls_playlist_ready(api, &pipeline_id, Duration::from_secs(20)).await?;
    let hls_preview =
        wait_for_api_hls_preview_state(api, &pipeline_id, true, Duration::from_secs(10)).await?;
    let hls_probe = probe_dims_ramp_with_cookie(&playlist_url, api.cookie.as_deref()).await;
    let hls_progress_wait_secs = 5.0;
    let hls_playlist_progress = {
        let (_, playlist_before) = api
            .get_text_response(&format!("/hls/{pipeline_id}/index.m3u8"))
            .await?;
        let before = parse_hls_playlist_snapshot(&playlist_before);
        tokio::time::sleep(Duration::from_secs_f64(hls_progress_wait_secs)).await;
        let (_, playlist_after) = api
            .get_text_response(&format!("/hls/{pipeline_id}/index.m3u8"))
            .await?;
        let after = parse_hls_playlist_snapshot(&playlist_after);
        let segment_changed = before.last_segment != after.last_segment;
        let media_sequence_delta = match (before.media_sequence, after.media_sequence) {
            (Some(before), Some(after)) => Some(after.saturating_sub(before)),
            _ => None,
        };
        json!({
            "passed": segment_changed,
            "waitSecs": hls_progress_wait_secs,
            "before": {
                "mediaSequence": before.media_sequence,
                "lastSegment": before.last_segment,
            },
            "after": {
                "mediaSequence": after.media_sequence,
                "lastSegment": after.last_segment,
            },
            "segmentChanged": segment_changed,
            "mediaSequenceDelta": media_sequence_delta,
        })
    };

    let before_files = media_dir_entries(media_dir)?;
    api.post_empty(&format!("/api/v1/pipelines/{pipeline_id}/recording/start"))
        .await?;
    wait_for_api_recording_state(api, &pipeline_id, true, Duration::from_secs(10)).await?;

    let capture_target_secs = 8.0;
    let recording_started = Instant::now();
    tokio::time::sleep(Duration::from_secs_f64(capture_target_secs)).await;

    api.post_empty(&format!("/api/v1/pipelines/{pipeline_id}/recording/stop"))
        .await?;
    let capture_elapsed_secs = recording_started.elapsed().as_secs_f64();
    wait_for_api_recording_state(api, &pipeline_id, false, Duration::from_secs(20)).await?;

    let recording_mp4 =
        wait_for_new_media_file(media_dir, &before_files, ".mp4", Duration::from_secs(30)).await?;
    let recorded_analysis = restream::media::file_analysis::analyze_media_file(&recording_mp4)?;

    let expected_source_ts = recording_mp4.with_extension("ts");
    let source_retained = expected_source_ts.exists();

    api.post_empty(&format!("/api/v1/ingests/{ingest_id}/stop"))
        .await?;
    wait_for_pipeline_file_ingest_running_state(api, &pipeline_id, false, Duration::from_secs(10))
        .await?;
    wait_for_api_input_off(api, &pipeline_id, Duration::from_secs(20)).await?;

    let recorded_duration_secs = recorded_analysis.duration_sec.ok_or_else(|| {
        format!(
            "recorded output {} has no duration",
            recording_mp4.display()
        )
    })?;
    let duration_delta_secs = absolute_delta_secs(recorded_duration_secs, capture_elapsed_secs);
    let duration_ok = duration_delta_secs <= FILE_LIVE_EDGE_MAX_DURATION_DRIFT_SECS;
    let hls_ok = playlist_body.contains("#EXTM3U")
        && hls_probe.is_ok()
        && hls_playlist_progress["passed"] == true;
    let live_optimized_gop_ok = if live_optimized {
        recorded_analysis
            .max_keyframe_interval_sec
            .is_some_and(|value| value <= target_gop_seconds as f64 + 0.6)
    } else {
        true
    };

    Ok(json!({
        "case": case_id,
        "passed": duration_ok && hls_ok && live_optimized_gop_ok && !source_retained,
        "liveOptimized": live_optimized,
        "targetGopSeconds": target_gop_seconds,
        "captureElapsedSecs": capture_elapsed_secs,
        "recordedDurationSecs": recorded_duration_secs,
        "durationDeltaSecs": duration_delta_secs,
        "maxAllowedDurationDriftSecs": FILE_LIVE_EDGE_MAX_DURATION_DRIFT_SECS,
        "durationOk": duration_ok,
        "sourceAnalysis": source_analysis,
        "recordedAnalysis": recorded_analysis,
        "hlsPreview": hls_preview,
        "hlsPlaylistReady": playlist_body.contains("#EXTM3U"),
        "hlsProbe": match hls_probe {
            Ok(dimensions) => json!({"passed": true, "dimensions": dimensions}),
            Err(error) => json!({"passed": false, "error": error}),
        },
        "hlsPlaylistProgress": hls_playlist_progress,
        "liveOptimizedGopOk": live_optimized_gop_ok,
        "sourceRetained": source_retained,
        "recordingFile": recording_mp4,
    }))
}

async fn file_live_edge() -> Result<Value, String> {
    let work_dir = artifact_path("file.live-edge");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let media_dir = work_dir.join("media");
    std::fs::create_dir_all(&media_dir).map_err(|e| e.to_string())?;

    let ports = TestPorts::from_env();
    let mut child =
        start_restream_child_in_media_dir(&restream_bin, &ports, &db_path, &log_path, &media_dir)
            .await?;
    let mut api = login_api(&ports).await?;

    let passthrough = run_file_live_edge_case(
        &mut api,
        &ports,
        &media_dir,
        &checked_h264_fixture()?,
        "file-live-edge-passthrough",
        false,
        2,
    )
    .await?;

    let live_optimized = run_file_live_edge_case(
        &mut api,
        &ports,
        &media_dir,
        &restream::test_fixtures::sparse_gop_mp4_fixture()?,
        "file-live-edge-optimized",
        true,
        2,
    )
    .await?;

    stop_child(&mut child).await;

    let cases = vec![passthrough, live_optimized];
    let passed = cases.iter().all(|case| case["passed"] == true);
    Ok(json!({
        "mode": "file.live-edge",
        "passed": passed,
        "cases": cases,
        "mediaDir": media_dir,
        "logPath": log_path,
    }))
}

async fn signal_control() -> Result<Value, String> {
    let work_dir = artifact_path("signal.control");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    let env = MixedEnv::from_env_with_default_work_dir("signal.control", work_dir.clone());
    let duration = env.av_signal_seconds;
    let cases = [
        ("h264-single-source", "h264", false, false),
        ("h264-single-720p", "h264", false, true),
        ("h265-single-source", "h265", false, false),
        ("h265-single-720p", "h265", false, true),
        ("h264-multi-source", "h264", true, false),
        ("h265-multi-source", "h265", true, false),
    ];
    let mut results = Vec::new();
    for (name, codec, multi_audio, transcode_720p) in cases {
        let fixture = restream::test_fixtures::av_marker_transport_fixture(codec, multi_audio)?;
        let capture_path = work_dir.join(format!("{name}.signal.mkv"));
        ffmpeg_control_capture(&fixture, &capture_path, duration, transcode_720p).await?;
        let started = Instant::now();
        validate_signal_capture_artifact(
            &env,
            "signal.control",
            &format!("SC-{name}"),
            name,
            &fixture.to_string_lossy(),
            &capture_path,
            duration,
            started,
        )
        .await?;
        results.push(json!({
            "name": name,
            "fixture": fixture,
            "capture": capture_path,
            "transcode720p": transcode_720p,
            "passed": true,
        }));
    }
    Ok(json!({
        "mode": "signal.control",
        "passed": true,
        "durationSecs": duration,
        "workDir": work_dir,
        "cases": results,
    }))
}

async fn ffmpeg_control_capture(
    fixture: &Path,
    capture_path: &Path,
    duration: u64,
    transcode_720p: bool,
) -> Result<(), String> {
    let duration_s = duration.to_string();
    let fixture_s = fixture.to_string_lossy().to_string();
    let mut command = Command::new("ffmpeg");
    command.args([
        "-y",
        "-nostdin",
        "-hide_banner",
        "-v",
        "warning",
        "-stream_loop",
        "-1",
        "-i",
        &fixture_s,
        "-t",
        &duration_s,
        "-map",
        "0:v:0",
        "-map",
        "0:a:0",
    ]);
    if transcode_720p {
        command.args([
            "-vf",
            "scale=1280:720",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-g",
            "60",
            "-c:a",
            "copy",
        ]);
    } else {
        command.args(["-c", "copy"]);
    }
    command.args(["-f", "matroska"]).arg(capture_path);
    let child = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let output = tokio::time::timeout(Duration::from_secs(duration + 60), child.wait_with_output())
        .await
        .map_err(|_| format!("signal control capture timed out: {}", fixture.display()))?
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "signal control capture failed for {}: {}",
            fixture.display(),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

async fn fault_rtmp_egress_sink_disappear(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    timeout: Duration,
) -> Result<Value, String> {
    let pid = create_pipeline(api, "fault-egress-rtmp").await?;

    let sink_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink_server = start_generalized_sink_server(sink_port, sink_metrics.clone()).await?;

    let sink_url = format!("rtmp://127.0.0.1:{sink_port}/live/fault-egress-rtmp-sink");
    let oid = create_output(api, &pid, "rtmp-sink", &sink_url, "source").await?;

    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!("rtmp://127.0.0.1:{}/live/fault-egress-rtmp", ports.rtmp),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;

    start_output(api, &pid, &oid).await?;

    let _ = wait_for_sink_video_above(&sink_metrics, 9, timeout).await;
    println!("[fault] RTMP egress delivering data");

    stop_generalized_sink_server(sink_server);

    let started = Instant::now();
    let retry =
        wait_for_output_retry_or_cleanup_observation(api, &pid, &oid, Duration::from_secs(10))
            .await;
    let elapsed = started.elapsed();
    let recovery_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let recovered_server =
        start_generalized_sink_server(sink_port, recovery_metrics.clone()).await?;

    let recovery_started = Instant::now();
    let recovery_deadline = recovery_started + Duration::from_secs(25);
    let mut recovered = false;
    let mut recovery_status = String::from("unknown");
    let mut saw_retrying = retry.status_visible;
    while Instant::now() < recovery_deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(status) = api
            .get_json(&format!("/api/v1/pipelines/{pid}/outputs/{oid}/status"))
            .await
        {
            recovery_status = status["status"].as_str().unwrap_or("unknown").to_string();
            if recovery_status == "retrying" {
                saw_retrying = true;
            }
        }
        if recovery_metrics.video_count.load(Ordering::Relaxed) >= 10 {
            recovered = true;
            break;
        }
    }
    stop_generalized_sink_server(recovered_server);
    let final_output = observe_final_output(api, &pid, &oid).await;
    let retry_phase_ok = output_retry_or_cleanup_phase_ok(&retry);
    println!(
        "[fault] RTMP egress sink disappear: {} (phase={}, hasError={}, sawRetrying={}, healthSawRetrying={}, recovered={}, recoveryStatus={}, finalRetrying={}, {:.1}s)",
        if retry_phase_ok
            && recovered
            && saw_retrying
            && retry.health_visible
            && !final_output.retrying
        {
            "PASS"
        } else {
            "FAIL"
        },
        retry.phase,
        retry.has_error,
        saw_retrying,
        retry.health_visible,
        recovered,
        recovery_status,
        final_output.retrying,
        elapsed.as_secs_f64()
    );

    stop_child(&mut pub_child).await;

    Ok(json!({
        "test": "rtmp-egress-sink-disappear",
        "passed": retry_phase_ok && recovered && saw_retrying && retry.health_visible && !final_output.retrying,
        "phase": retry.phase,
        "hasError": retry.has_error,
        "elapsedMs": elapsed.as_millis(),
        "sawRetrying": saw_retrying,
        "healthSawRetrying": retry.health_visible,
        "retryAttempts": retry.attempts,
        "retryBackoffMs": retry.backoff_ms,
        "recovered": recovered,
        "recoveryStatus": recovery_status,
        "finalRetrying": final_output.retrying,
    }))
}

async fn fault_srt_egress_sink_disappear(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    timeout: Duration,
) -> Result<Value, String> {
    let pid = create_pipeline(api, "fault-egress-srt").await?;

    let sink_pid = create_pipeline(api, "srt-sink-target").await?;

    let sink_url = format!(
        "srt://127.0.0.1:{}?streamid=publish:live/srt-sink-target&pkt_size=1316",
        ports.srt
    );
    let oid = create_output(api, &pid, "srt-sink", &sink_url, "source").await?;

    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!(
            "srt://127.0.0.1:{}?streamid=publish:live/fault-egress-srt&pkt_size=1316",
            ports.srt
        ),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;

    start_output(api, &pid, &oid).await?;

    let deadline = Instant::now() + timeout;
    let mut sink_live = false;
    while Instant::now() < deadline {
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            let status = health["pipelines"][&sink_pid]["input"]["status"]
                .as_str()
                .unwrap_or("off");
            if status == "on" {
                sink_live = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if sink_live {
        println!("[fault] SRT egress delivering to sink pipeline");
    }

    let delete_url = format!("{}/api/v1/pipelines/{sink_pid}", api.base_url);
    let mut request = api.client.delete(&delete_url);
    if let Some(cookie) = &api.cookie {
        request = request.header(reqwest::header::COOKIE, cookie);
    }
    let _ = request.send().await;

    let started = Instant::now();
    let retry =
        wait_for_output_retry_or_cleanup_observation(api, &pid, &oid, Duration::from_secs(10))
            .await;
    let elapsed = started.elapsed();
    let final_output = observe_final_output(api, &pid, &oid).await;
    let retry_phase_ok = output_retry_or_cleanup_phase_ok(&retry);
    println!(
        "[fault] SRT egress sink disappear: {} (phase={}, hasError={}, sawRetrying={}, healthSawRetrying={}, finalRetrying={}, {:.1}s)",
        if retry_phase_ok && retry.status_visible && retry.health_visible && final_output.retrying {
            "PASS"
        } else {
            "FAIL"
        },
        retry.phase,
        retry.has_error,
        retry.status_visible,
        retry.health_visible,
        final_output.retrying,
        elapsed.as_secs_f64()
    );

    stop_child(&mut pub_child).await;

    Ok(json!({
        "test": "srt-egress-sink-disappear",
        "passed": retry_phase_ok && retry.status_visible && retry.health_visible && final_output.retrying,
        "phase": retry.phase,
        "hasError": retry.has_error,
        "elapsedMs": elapsed.as_millis(),
        "sawRetrying": retry.status_visible,
        "healthSawRetrying": retry.health_visible,
        "retryAttempts": retry.attempts,
        "retryBackoffMs": retry.backoff_ms,
        "finalRetrying": final_output.retrying,
    }))
}

async fn fault_rtmp_egress_sink_stalls(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    timeout: Duration,
) -> Result<Value, String> {
    let pid = create_pipeline(api, "fault-egress-rtmp-stall").await?;

    let oid = create_output(
        api,
        &pid,
        "rtmp-stall-sink",
        &format!("rtmp://127.0.0.1:{sink_port}/live/fault-egress-rtmp-stall-sink"),
        "source",
    )
    .await?;

    let stall_server = start_stalled_rtmp_sink_server(sink_port).await?;
    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!(
            "rtmp://127.0.0.1:{}/live/fault-egress-rtmp-stall",
            ports.rtmp
        ),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;

    start_output(api, &pid, &oid).await?;

    let accept_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < accept_deadline && !stall_server.publish_accepted.load(Ordering::Relaxed)
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let accepted = stall_server.publish_accepted.load(Ordering::Relaxed);
    let stalled_result =
        wait_for_output_stalled_status(api, &pid, &oid, Duration::from_secs(20)).await;
    let (status_snapshot, health_snapshot) = stalled_result
        .as_ref()
        .map(|(status, health)| (status.clone(), health.clone()))
        .unwrap_or((Value::Null, Value::Null));
    let passed = accepted && stalled_result.is_ok();

    println!(
        "[fault] RTMP egress sink stalls: {} (publishAccepted={} status={} phase={} targetAddr={} totalSize={} lastProgressAgeMs={})",
        if passed { "PASS" } else { "FAIL" },
        accepted,
        status_snapshot["status"].as_str().unwrap_or("unknown"),
        status_snapshot["phase"].as_str().unwrap_or("unknown"),
        status_snapshot["targetAddr"].as_str().unwrap_or(""),
        status_snapshot["totalSize"].as_u64().unwrap_or(0),
        status_snapshot["lastProgressAgeMs"]
            .as_u64()
            .map(|age| age.to_string())
            .unwrap_or_else(|| "null".to_string()),
    );

    stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
    stop_child(&mut pub_child).await;
    stop_stalled_rtmp_sink_server(stall_server);

    Ok(json!({
        "test": "rtmp-egress-sink-stalls",
        "passed": passed,
        "publishAccepted": accepted,
        "status": status_snapshot,
        "healthOutput": health_snapshot,
        "error": stalled_result.err(),
    }))
}

async fn wait_for_outputs_live_and_progressing(
    api: &RampApi,
    pipeline_id: &str,
    output_ids: &[String],
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    let mut stabilized = Vec::new();
    let mut attempts = 0u32;
    let mut latest = Value::Null;

    while Instant::now() < deadline {
        attempts = attempts.saturating_add(1);
        let health = api.get_json("/api/v1/engine/health").await?;
        let mut snapshots = Vec::with_capacity(output_ids.len());
        let mut all_live = true;

        for output_id in output_ids {
            let output = health["pipelines"][pipeline_id]["outputs"][output_id].clone();
            let status = output["status"].as_str().unwrap_or("unknown");
            let phase = output["phase"].as_str().unwrap_or("unknown");
            let raw_status = output["rawStatus"].as_str().unwrap_or("unknown");
            let bytes_out = output["bytesOut"].as_u64().unwrap_or(0);
            let total_size = output["totalSize"].as_u64().unwrap_or(0);
            let retrying = output["retrying"].as_bool().unwrap_or(false);
            let failure_phase = output["failurePhase"].as_str().unwrap_or("");
            let last_error = output["lastError"].as_str().unwrap_or("");
            let last_progress_age_ms = output["lastProgressAgeMs"].as_u64();
            let healthy = status == "running"
                && matches!(phase, "sending" | "uploading")
                && raw_status == "running"
                && bytes_out > 0
                && total_size > 0
                && !retrying
                && failure_phase.is_empty()
                && last_error.is_empty()
                && last_progress_age_ms.is_some_and(|age| age <= 5_000);
            if !healthy {
                all_live = false;
            }
            snapshots.push(json!({
                "outputId": output_id,
                "status": status,
                "phase": phase,
                "rawStatus": raw_status,
                "bytesOut": bytes_out,
                "totalSize": total_size,
                "lastProgressAgeMs": last_progress_age_ms,
                "retrying": retrying,
                "failurePhase": output["failurePhase"],
                "lastError": output["lastError"],
                "healthy": healthy,
            }));
        }

        latest = json!({
            "attempt": attempts,
            "outputs": snapshots,
        });

        if all_live {
            stabilized.push(latest.clone());
            if stabilized.len() >= 2 {
                return Ok(json!({
                    "attempts": attempts,
                    "stabilizedSamples": stabilized,
                }));
            }
        } else {
            stabilized.clear();
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    Err(format!(
        "{} output(s) for pipeline {pipeline_id} did not stay live/progressing within {}s; latest={latest}",
        output_ids.len(),
        timeout.as_secs()
    ))
}

async fn fault_rtmp_stalled_sink_isolation_under_many_outputs(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    stall_sink_port: u16,
    healthy_sink_base_port: u16,
    sibling_outputs: usize,
    timeout: Duration,
) -> Result<Value, String> {
    let sibling_outputs = sibling_outputs.max(1);
    let pid = create_pipeline(api, "fault-egress-rtmp-stall-isolation").await?;

    let stalled_oid = create_output(
        api,
        &pid,
        "rtmp-stall-sink-isolation",
        &format!(
            "rtmp://127.0.0.1:{stall_sink_port}/live/fault-egress-rtmp-stall-isolation-stalled"
        ),
        "source",
    )
    .await?;

    let mut healthy_servers = Vec::with_capacity(sibling_outputs);
    let mut healthy_output_ids = Vec::with_capacity(sibling_outputs);
    let mut healthy_metrics = Vec::with_capacity(sibling_outputs);
    for index in 0..sibling_outputs {
        let port = healthy_sink_base_port.saturating_add(index as u16);
        let metrics = Arc::new(GeneralizedSinkMetrics::default());
        let server = start_generalized_sink_server(port, metrics.clone()).await?;
        let oid = create_output(
            api,
            &pid,
            &format!("rtmp-healthy-sink-{index:02}"),
            &format!(
                "rtmp://127.0.0.1:{port}/live/fault-egress-rtmp-stall-isolation-healthy-{index:02}"
            ),
            "source",
        )
        .await?;
        healthy_output_ids.push(oid);
        healthy_metrics.push(metrics);
        healthy_servers.push(server);
    }

    let stall_server = start_stalled_rtmp_sink_server(stall_sink_port).await?;
    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!(
            "rtmp://127.0.0.1:{}/live/fault-egress-rtmp-stall-isolation",
            ports.rtmp
        ),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;

    start_output(api, &pid, &stalled_oid).await?;
    for output_id in &healthy_output_ids {
        start_output(api, &pid, output_id).await?;
    }

    let stalled_accept_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < stalled_accept_deadline
        && !stall_server.publish_accepted.load(Ordering::Relaxed)
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let stalled_publish_accepted = stall_server.publish_accepted.load(Ordering::Relaxed);
    let healthy_accept_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < healthy_accept_deadline {
        let accepted = healthy_metrics
            .iter()
            .all(|metrics| metrics.publishing.load(Ordering::Relaxed) > 0);
        if accepted {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let healthy_publish_accepted = healthy_metrics
        .iter()
        .all(|metrics| metrics.publishing.load(Ordering::Relaxed) > 0);

    let healthy_progress_result = wait_for_outputs_live_and_progressing(
        api,
        &pid,
        &healthy_output_ids,
        Duration::from_secs(25),
    )
    .await;
    let stalled_result =
        wait_for_output_stalled_status(api, &pid, &stalled_oid, Duration::from_secs(25)).await;

    let healthy_snapshots = healthy_progress_result.as_ref().ok().cloned();
    let stalled_snapshots = stalled_result
        .as_ref()
        .map(|(status, health)| json!({ "status": status, "health": health }))
        .ok();

    let mut healthy_metric_summaries = Vec::with_capacity(healthy_metrics.len());
    for (index, metrics) in healthy_metrics.iter().enumerate() {
        healthy_metric_summaries.push(json!({
            "index": index,
            "publishing": metrics.publishing.load(Ordering::Relaxed),
            "videoCount": metrics.video_count.load(Ordering::Relaxed),
            "audioCount": metrics.audio_count.load(Ordering::Relaxed),
            "bytes": metrics.bytes.load(Ordering::Relaxed),
        }));
    }

    let passed = stalled_publish_accepted
        && healthy_publish_accepted
        && healthy_progress_result.is_ok()
        && stalled_result.is_ok();

    println!(
        "[fault] RTMP stalled sink isolation under sibling load: {} (siblings={} stalledAccepted={} healthyAccepted={} healthyProgress={} stalledVisible={})",
        if passed { "PASS" } else { "FAIL" },
        sibling_outputs,
        stalled_publish_accepted,
        healthy_publish_accepted,
        healthy_progress_result.is_ok(),
        stalled_result.is_ok(),
    );

    stop_mixed_outputs(api, &pid, std::slice::from_ref(&stalled_oid)).await;
    stop_mixed_outputs(api, &pid, &healthy_output_ids).await;
    stop_child(&mut pub_child).await;
    stop_stalled_rtmp_sink_server(stall_server);
    for server in healthy_servers {
        stop_generalized_sink_server(server);
    }

    Ok(json!({
        "test": "rtmp-stalled-sink-isolation-under-many-outputs",
        "passed": passed,
        "siblingOutputs": sibling_outputs,
        "stalledOutputId": stalled_oid,
        "healthyOutputIds": healthy_output_ids,
        "stalledPublishAccepted": stalled_publish_accepted,
        "healthyPublishAccepted": healthy_publish_accepted,
        "healthyProgress": healthy_snapshots,
        "stalledSnapshot": stalled_snapshots,
        "healthySinkMetrics": healthy_metric_summaries,
        "healthyProgressError": healthy_progress_result.err(),
        "stalledError": stalled_result.err(),
    }))
}

async fn create_pipeline_with_stream_key(
    api: &RampApi,
    name: &str,
    stream_key: &str,
) -> Result<String, String> {
    let pipeline = api
        .post_json(
            "/api/v1/pipelines",
            json!({"name": name, "streamKey": stream_key}),
        )
        .await?;
    pipeline["pipeline"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("missing pipeline id for {name}"))
}

async fn create_pipeline(api: &RampApi, name: &str) -> Result<String, String> {
    create_pipeline_with_stream_key(api, name, name).await
}

async fn delete_pipeline_v1(api: &RampApi, pipeline_id: &str) -> Result<(), String> {
    let delete_url = format!("{}/api/v1/pipelines/{pipeline_id}", api.base_url);
    let mut request = api.client.delete(&delete_url);
    if let Some(cookie) = &api.cookie {
        request = request.header(reqwest::header::COOKIE, cookie);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("delete pipeline {pipeline_id}: {e}"))?;
    if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
        Ok(())
    } else {
        Err(format!(
            "delete pipeline {pipeline_id}: unexpected status {}",
            response.status()
        ))
    }
}

async fn fault_egress_retry() -> Result<Value, String> {
    let work_dir = artifact_path("fault.egress-retry");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let retry_limit_db_path = work_dir.join("retry-limit.sqlite");
    let retry_limit_log_path = work_dir.join("retry-limit.log");
    let sink_port = harness_port_defaults().sink;
    let ports = TestPorts::from_env();
    let timeout = Duration::from_secs(15);

    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture_h264 = checked_h264_fixture()?;
    let results = vec![
        fault_rtmp_egress_sink_disappear(&api, &ports, &fixture_h264, sink_port, timeout).await?,
        fault_srt_egress_sink_disappear(&api, &ports, &fixture_h264, timeout).await?,
    ];

    stop_child(&mut child).await;

    let retry_limit_env = [
        ("RESTREAM_OUTPUT_MAX_RETRIES", "2".to_string()),
        ("RESTREAM_OUTPUT_RETRY_BASE_MS", "200".to_string()),
        ("RESTREAM_OUTPUT_RETRY_MAX_MS", "400".to_string()),
        ("RESTREAM_RECONCILER_INTERVAL_MS", "100".to_string()),
    ];
    let mut retry_limit_child = start_restream_child_with_env(
        &restream_bin,
        &ports,
        &retry_limit_db_path,
        &retry_limit_log_path,
        &retry_limit_env,
    )
    .await?;
    let retry_limit_api = login_api(&ports).await?;
    let mut retry_limit_results = Vec::new();
    for case in retry_budget_cases() {
        let workflow_result = run_retry_budget_case_via_workflow(
            &retry_limit_api,
            &ports,
            &fixture_h264,
            sink_port,
            case,
        )
        .await?;
        retry_limit_results.push(workflow_result);
    }
    stop_child(&mut retry_limit_child).await;

    let mut results = results;
    results.extend(retry_limit_results);

    let all_passed = results.iter().all(|r| r["passed"] == true);
    let result = json!({
        "mode": "fault.egress-retry",
        "passed": all_passed,
        "tests": results,
    });

    let result_path = work_dir.join("fault.egress-retry.json");
    std::fs::write(&result_path, serde_json::to_string_pretty(&result).unwrap())
        .map_err(|e| e.to_string())?;
    println!("artifact={}", result_path.display());

    if !all_passed {
        return Err("fault.egress-retry: not all tests passed".to_string());
    }
    Ok(result)
}

async fn fault_output_stall() -> Result<Value, String> {
    let work_dir = artifact_path("fault.output-stall");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let sink_port = harness_port_defaults().sink;
    let ports = TestPorts::from_env();
    let timeout = Duration::from_secs(15);

    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture_h264 = checked_h264_fixture()?;
    let stall_single =
        fault_rtmp_egress_sink_stalls(&api, &ports, &fixture_h264, sink_port, timeout).await?;
    let sibling_outputs = fault_output_stall_sibling_count();
    let isolation = fault_rtmp_stalled_sink_isolation_under_many_outputs(
        &api,
        &ports,
        &fixture_h264,
        sink_port.saturating_add(10),
        sink_port.saturating_add(100),
        sibling_outputs,
        timeout,
    )
    .await?;

    stop_child(&mut child).await;

    let tests = vec![stall_single, isolation];
    let passed = tests
        .iter()
        .all(|result| result["passed"].as_bool().unwrap_or(false));
    let payload = json!({
        "mode": "fault.output-stall",
        "passed": passed,
        "siblingOutputs": sibling_outputs,
        "tests": tests,
    });

    let result_path = work_dir.join("fault.output-stall.json");
    std::fs::write(
        &result_path,
        serde_json::to_string_pretty(&payload).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!("artifact={}", result_path.display());

    if !passed {
        return Err("fault.output-stall: not all tests passed".to_string());
    }
    Ok(payload)
}

const RECOVERY_WARM_VIDEO_MIN: u64 = 10;

async fn wait_for_sink_video_above(
    metrics: &GeneralizedSinkMetrics,
    threshold: u64,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if metrics.video_count.load(Ordering::Relaxed) > threshold {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

fn health_input_snapshot(health: Option<&Value>, pipeline_id: &str) -> Value {
    health
        .map(|health| health["pipelines"][pipeline_id]["input"].clone())
        .unwrap_or(Value::Null)
}

fn disconnect_grace_remaining_bounded(input: &Value) -> bool {
    input["disconnectGraceRemainingMs"]
        .as_u64()
        .is_some_and(|remaining| remaining > 0 && remaining <= 5_000)
}

fn input_disconnect_cleared(input: &Value) -> bool {
    input["status"] == "on"
        && input["probeStatus"] == "ready"
        && input["lastSessionProtocol"].is_null()
        && input["lastDisconnectReason"].is_null()
        && input["lastFailurePhase"].is_null()
        && input["recentDisconnectError"] == false
}

/// Final output state checked by recovery/fault cells after perturbation.
struct FinalOutputObservation {
    status: Option<Value>,
    health: Value,
    running: bool,
    retrying: bool,
    error_cleared: bool,
    recent_failure_count: u64,
    flapping: bool,
    health_recent_failure_count: u64,
    health_flapping: bool,
}

async fn observe_final_output(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
) -> FinalOutputObservation {
    let status = api
        .get_json(&format!(
            "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/status"
        ))
        .await
        .ok();
    let health = api.get_json("/api/v1/engine/health").await.ok();
    let output_health = health
        .as_ref()
        .map(|health| health["pipelines"][pipeline_id]["outputs"][output_id].clone())
        .unwrap_or(Value::Null);

    FinalOutputObservation {
        running: status.as_ref().and_then(|status| status["status"].as_str()) == Some("running"),
        retrying: status
            .as_ref()
            .and_then(|status| status["retrying"].as_bool())
            .unwrap_or(false),
        error_cleared: status
            .as_ref()
            .is_some_and(|status| status["lastError"].is_null()),
        recent_failure_count: status
            .as_ref()
            .and_then(|status| status["recentFailureCount"].as_u64())
            .unwrap_or(0),
        flapping: status
            .as_ref()
            .and_then(|status| status["flapping"].as_bool())
            .unwrap_or(false),
        health_recent_failure_count: output_health["recentFailureCount"].as_u64().unwrap_or(0),
        health_flapping: output_health["flapping"].as_bool().unwrap_or(false),
        status,
        health: output_health,
    }
}

/// Output retry state observed from both the public status endpoint and engine health.
struct OutputRetryObservation {
    status_visible: bool,
    health_visible: bool,
    has_error: bool,
    cleaned_up: bool,
    phase: String,
    failure_phase: String,
    last_error: String,
    attempts: Option<u64>,
    backoff_ms: Option<u64>,
}

impl Default for OutputRetryObservation {
    fn default() -> Self {
        Self {
            status_visible: false,
            health_visible: false,
            has_error: false,
            cleaned_up: false,
            phase: String::from("unknown"),
            failure_phase: String::from("unknown"),
            last_error: String::new(),
            attempts: None,
            backoff_ms: None,
        }
    }
}

async fn wait_for_output_retry_observation(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    timeout: Duration,
) -> OutputRetryObservation {
    let deadline = Instant::now() + timeout;
    let mut observation = OutputRetryObservation::default();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(status) = api
            .get_json(&format!(
                "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/status"
            ))
            .await
        {
            observation.status_visible = status["status"].as_str() == Some("retrying")
                && status["retrying"].as_bool() == Some(true);
            observation.phase = status["phase"].as_str().unwrap_or("unknown").to_string();
            observation.failure_phase = status["failurePhase"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            observation.last_error = status["lastError"].as_str().unwrap_or("").to_string();
            observation.has_error = !observation.last_error.is_empty();
            if observation.status_visible {
                observation.attempts = status["retryAttempts"].as_u64();
                observation.backoff_ms = status["retryBackoffMs"].as_u64();
            }
        }
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            let output = &health["pipelines"][pipeline_id]["outputs"][output_id];
            observation.health_visible = output["status"].as_str() == Some("retrying")
                && output["retrying"].as_bool() == Some(true);
        }
        if observation.status_visible && observation.health_visible && observation.has_error {
            break;
        }
    }
    observation
}

async fn wait_for_output_retry_or_cleanup_observation(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    timeout: Duration,
) -> OutputRetryObservation {
    let deadline = Instant::now() + timeout;
    let mut observation = OutputRetryObservation::default();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        match api
            .get_json(&format!(
                "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/status"
            ))
            .await
        {
            Err(_) => {
                observation.cleaned_up = true;
                observation.phase = "cleaned-up".to_string();
                break;
            }
            Ok(status) => {
                observation.status_visible = status["status"].as_str() == Some("retrying");
                observation.phase = status["phase"].as_str().unwrap_or("unknown").to_string();
                observation.last_error = status["lastError"].as_str().unwrap_or("").to_string();
                observation.has_error = !observation.last_error.is_empty();
                if observation.status_visible {
                    observation.attempts = status["retryAttempts"].as_u64();
                    observation.backoff_ms = status["retryBackoffMs"].as_u64();
                }
            }
        }
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            observation.health_visible =
                health["pipelines"][pipeline_id]["outputs"][output_id]["status"].as_str()
                    == Some("retrying");
        }
        if observation.status_visible && observation.has_error {
            break;
        }
    }
    observation
}

fn output_retry_or_cleanup_phase_ok(observation: &OutputRetryObservation) -> bool {
    observation.cleaned_up || (observation.status_visible && observation.has_error)
}

async fn output_running_without_retry(api: &RampApi, pipeline_id: &str, output_id: &str) -> bool {
    api.get_json(&format!(
        "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/status"
    ))
    .await
    .ok()
    .is_some_and(|status| {
        status["status"].as_str() == Some("running")
            && !status["retrying"].as_bool().unwrap_or(false)
    })
}

async fn wait_for_output_running(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if output_running_without_retry(api, pipeline_id, output_id).await {
            return true;
        }
    }
    false
}

async fn wait_for_output_running_and_sink_video_above(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    metrics: &GeneralizedSinkMetrics,
    threshold: u64,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let video_progressed = metrics.video_count.load(Ordering::Relaxed) > threshold;
        if video_progressed && output_running_without_retry(api, pipeline_id, output_id).await {
            return true;
        }
    }
    false
}

async fn recovery() -> Result<Value, String> {
    let work_dir = artifact_path("recovery");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let sink_port = harness_port_defaults().sink;
    let hls_put_port = harness_port_defaults().hls_put;
    let ports = TestPorts::from_env();
    let timeout = Duration::from_secs(15);

    let (mut child, mut api) =
        start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture_h264 = checked_h264_fixture()?;
    let results = recovery_live_cases(
        &mut api,
        &ports,
        &fixture_h264,
        sink_port,
        hls_put_port,
        timeout,
    )
    .await?;

    let history_contract = verify_live_history_contract(&api, &["egress.failed"]).await?;
    println!("[recovery] history contract verified");

    stop_child(&mut child).await;

    let all_passed = results.iter().all(|r| r["passed"] == true);
    let result = json!({
        "mode": "recovery",
        "passed": all_passed,
        "tests": results,
        "historyContract": history_contract,
    });

    let result_path = work_dir.join("recovery.json");
    std::fs::write(&result_path, serde_json::to_string_pretty(&result).unwrap())
        .map_err(|e| e.to_string())?;
    println!("artifact={}", result_path.display());

    if !all_passed {
        return Err("recovery: not all tests passed".to_string());
    }
    Ok(result)
}

async fn fault_resilience() -> Result<Value, String> {
    let work_dir = artifact_path("fault.resilience");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let sink_port = harness_port_defaults().sink;
    let hls_put_port = harness_port_defaults().hls_put;
    let ports = TestPorts::from_env();
    let timeout = Duration::from_secs(15);

    let (mut child, mut api) =
        start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture_h264 = checked_h264_fixture()?;

    let mut results: Vec<Value> = Vec::new();

    for case in publisher_disconnect_cases() {
        results
            .push(run_publisher_disconnect_case(&api, &ports, &fixture_h264, timeout, case).await?);
    }

    results.extend(
        recovery_live_cases(
            &mut api,
            &ports,
            &fixture_h264,
            sink_port,
            hls_put_port,
            timeout,
        )
        .await?,
    );

    for test_name in [
        "file-ingest-stop",
        "recording-stops-after-ingest-disconnect",
    ] {
        results.push(
            run_ingest_lifecycle_case(
                &api,
                &ports,
                &fixture_h264,
                ingest_lifecycle_case(test_name)?,
            )
            .await?,
        );
    }

    // ── 5. External transcoder tears down after ingest disappears ───────
    {
        let pid = create_pipeline(&api, "fault-transcoder").await?;

        let sink_metrics = Arc::new(GeneralizedSinkMetrics::default());
        let sink_server = start_generalized_sink_server(sink_port, sink_metrics.clone()).await?;

        let sink_url = format!("rtmp://127.0.0.1:{sink_port}/live/fault-transcoder-sink");
        let oid = create_output(&api, &pid, "rtmp.720p.a0", &sink_url, "720p").await?;

        let mut pub_child = spawn_publisher(
            &fixture_h264,
            &format!("rtmp://127.0.0.1:{}/live/fault-transcoder", ports.rtmp),
            "flv",
            false,
        )
        .await?;
        wait_for_api_input_live(&api, &pid, timeout).await?;

        start_output(&api, &pid, &oid).await?;

        let restream_pid = child.id().ok_or("restream pid missing")?;
        let warm_deadline = Instant::now() + Duration::from_secs(15);
        let mut ffmpeg_spawned = false;
        let mut peak_ffmpeg_children = 0u64;
        let mut peak_transcoder_buffers = 0u64;
        let mut saw_output_bytes = false;
        while Instant::now() < warm_deadline {
            let ffmpeg = ffmpeg_children_stats(restream_pid)?;
            let telemetry = api.get_json("/api/v1/engine/telemetry").await?;
            let active_transcoder_buffers =
                telemetry["activeTranscoderBuffers"].as_u64().unwrap_or(0);
            peak_ffmpeg_children = peak_ffmpeg_children.max(ffmpeg.count);
            peak_transcoder_buffers = peak_transcoder_buffers.max(active_transcoder_buffers);
            saw_output_bytes |= sink_metrics.bytes.load(Ordering::Relaxed) > 0;
            if (ffmpeg.count > 0 || active_transcoder_buffers > 0) && saw_output_bytes {
                ffmpeg_spawned = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        stop_child(&mut pub_child).await;
        let started = Instant::now();
        let off_result = wait_for_api_input_off(&api, &pid, timeout).await;
        let cleanup_deadline = Instant::now() + Duration::from_secs(15);
        let mut cleanup_ok = false;
        let mut final_ffmpeg_count = u64::MAX;
        let mut final_transcoder_buffers = u64::MAX;
        while Instant::now() < cleanup_deadline {
            let ffmpeg = ffmpeg_children_stats(restream_pid)?;
            let telemetry = api.get_json("/api/v1/engine/telemetry").await?;
            let active_transcoder_buffers = telemetry["activeTranscoderBuffers"]
                .as_u64()
                .unwrap_or(u64::MAX);
            final_ffmpeg_count = ffmpeg.count;
            final_transcoder_buffers = active_transcoder_buffers;
            if ffmpeg.count == 0 && active_transcoder_buffers == 0 {
                cleanup_ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let status = api
            .get_json(&format!("/api/v1/pipelines/{pid}/outputs/{oid}/status"))
            .await;
        let output_cleaned_up = match &status {
            Err(_) => true,
            Ok(json) if json.get("error").is_some() => true,
            Ok(json) => {
                json["endedAt"].is_string()
                    && matches!(json["status"].as_str(), Some("stopped" | "failed"))
            }
        };
        let elapsed = started.elapsed();
        let passed = ffmpeg_spawned && off_result.is_ok() && cleanup_ok && output_cleaned_up;
        println!(
            "[fault] External transcoder tears down: {} (spawned={}, peakFfmpegChildren={}, peakTranscoderBuffers={}, finalFfmpegChildren={}, activeTranscoderBuffers={}, outputCleanedUp={}, {:.1}s)",
            if passed { "PASS" } else { "FAIL" },
            ffmpeg_spawned,
            peak_ffmpeg_children,
            peak_transcoder_buffers,
            final_ffmpeg_count,
            final_transcoder_buffers,
            output_cleaned_up,
            elapsed.as_secs_f64()
        );
        results.push(json!({
            "test": "external-transcoder-stops-after-ingest-disconnect",
            "passed": passed,
            "elapsedMs": elapsed.as_millis(),
            "inputOffError": off_result.err(),
            "ffmpegSpawned": ffmpeg_spawned,
            "peakFfmpegChildren": peak_ffmpeg_children,
            "peakTranscoderBuffers": peak_transcoder_buffers,
            "sawOutputBytes": saw_output_bytes,
            "finalFfmpegChildren": final_ffmpeg_count,
            "finalActiveTranscoderBuffers": final_transcoder_buffers,
            "outputCleanedUp": output_cleaned_up,
        }));

        stop_generalized_sink_server(sink_server);
    }

    // ── 6. RTMP egress sink disappears ──────────────────────────────────
    results.push(
        fault_rtmp_egress_sink_disappear(&api, &ports, &fixture_h264, sink_port, timeout).await?,
    );

    // ── 7. RTMP egress sink stops draining and surfaces stalled ─────────
    results.push(
        fault_rtmp_egress_sink_stalls(&api, &ports, &fixture_h264, sink_port, timeout).await?,
    );

    // ── 8. SRT egress sink disappears ───────────────────────────────────
    results.push(fault_srt_egress_sink_disappear(&api, &ports, &fixture_h264, timeout).await?);

    for test_name in [
        "hls-preview-stops-after-ingest-disconnect",
        "file-ingest-eof-clears-and-restarts",
    ] {
        results.push(
            run_ingest_lifecycle_case(
                &api,
                &ports,
                &fixture_h264,
                ingest_lifecycle_case(test_name)?,
            )
            .await?,
        );
    }

    let history_contract = verify_live_history_contract(&api, &["egress.failed"]).await?;
    let external_transcoder_history = verify_external_transcoder_history_contract(&api).await?;
    println!("[fault.resilience] history contract verified");

    stop_child(&mut child).await;

    let all_passed = results.iter().all(|r| r["passed"] == true);
    let result = json!({
        "mode": "fault.resilience",
        "passed": all_passed,
        "tests": results,
        "historyContract": history_contract,
        "externalTranscoderHistory": external_transcoder_history,
    });

    let result_path = work_dir.join("fault.resilience.json");
    std::fs::write(&result_path, serde_json::to_string_pretty(&result).unwrap())
        .map_err(|e| e.to_string())?;
    println!("artifact={}", result_path.display());

    if !all_passed {
        return Err("fault.resilience: not all tests passed".to_string());
    }
    Ok(result)
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
