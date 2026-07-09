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
use serde::Deserialize;
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
#[path = "test_harness/mixed_manifest.rs"]
mod mixed_manifest;
#[path = "test_harness/mixed_runner.rs"]
mod mixed_runner;
#[path = "test_harness/workflow_exec.rs"]
mod workflow_exec;

use catalog::HarnessCatalog;
use fault_manifest::*;
use fault_runner::*;
use mixed_manifest::*;
use mixed_runner::*;
use workflow_exec::*;

/// Metadata describing how a harness mode participates in suite runs,
/// derived from the `test/harness/modes.json` catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HarnessModeSpec {
    name: String,
    suite_default: bool,
    requires_port_namespace: bool,
    requires_bench_profile: bool,
}

fn harness_catalog_root() -> PathBuf {
    std::env::var_os("HARNESS_CATALOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test/harness"))
}

static BUILTIN_MODE_SPECS_FROM_CATALOG: OnceLock<Vec<HarnessModeSpec>> = OnceLock::new();

fn builtin_mode_specs() -> &'static [HarnessModeSpec] {
    BUILTIN_MODE_SPECS_FROM_CATALOG.get_or_init(|| {
        let catalog = HarnessCatalog::load(&harness_catalog_root())
            .expect("test/harness catalog should load");
        let index = catalog
            .mode_index()
            .expect("test/harness modes.json should index cleanly");
        index
            .into_values()
            .map(|entry| {
                let requires = entry.spec.get("requires").cloned().unwrap_or_default();
                HarnessModeSpec {
                    name: entry.name,
                    suite_default: entry
                        .spec
                        .get("suiteDefault")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    requires_port_namespace: requires
                        .get("portNamespace")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    requires_bench_profile: requires
                        .get("benchProfile")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                }
            })
            .collect()
    })
}

fn mixed_scenario_check_id(scenario: &str, check: &str) -> String {
    format!("{scenario}.{check}")
}

fn mixed_output_check_id(scenario: &str, row_id: &str, check: &str) -> String {
    format!("{scenario}.output.{row_id}.{check}")
}

fn mixed_output_instance_name(scenario: &str, row_id: &str, index: usize) -> String {
    format!("{scenario}-{row_id}-{index}")
}

/// Parsed source-ring telemetry used by the mixed adaptive-ring readiness gate.
#[derive(Debug, Clone, Copy)]
struct MixedAdaptiveRingSnapshot {
    capacity: u64,
    depth_secs: f64,
    overflows: u64,
    resized: bool,
    adequate: bool,
    passed: bool,
}

impl MixedAdaptiveRingSnapshot {
    fn to_json(self) -> Value {
        json!({
            "ringCapacity": self.capacity,
            "bufferDepthSecs": self.depth_secs,
            "ringResized": self.resized,
            "adequate": self.adequate,
            "overflows": self.overflows,
        })
    }
}

fn mixed_adaptive_ring_snapshot(telemetry: &Value) -> MixedAdaptiveRingSnapshot {
    let capacity = telemetry["sourceRing"]["capacity"].as_u64().unwrap_or(0);
    let depth_secs = telemetry["sourceRing"]["bufferDepthSecs"]
        .as_f64()
        .unwrap_or(0.0);
    let overflows = telemetry["sourceRing"]["readers"]
        .as_array()
        .map(|readers| {
            readers
                .iter()
                .map(|reader| reader["overflowCount"].as_u64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0);
    // 2 audio tracks x 50 pkt/s + video is roughly 130 pkt/s, so 780 slots is
    // enough for the minimum 5-second depth. A capacity above 1024 additionally
    // proves the adaptive resize path fired.
    let resized = capacity > 1024;
    let adequate = depth_secs >= 5.0 || capacity >= 780;
    let passed = adequate && overflows == 0;
    MixedAdaptiveRingSnapshot {
        capacity,
        depth_secs,
        overflows,
        resized,
        adequate,
        passed,
    }
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
            | StageKind::Recording
            | StageKind::Preview { .. } => {}
        }
    }
    counts
}

fn mixed_input_mode_spec(case: MixedInputCase) -> HarnessModeSpec {
    HarnessModeSpec {
        name: case.scenario_id().to_string(),
        suite_default: false,
        requires_port_namespace: true,
        // Mixed scenarios always emit timing/resource evidence and should run
        // under one harness-level profile policy rather than varying by cell.
        requires_bench_profile: true,
    }
}

fn mode_spec(name: &str) -> Option<HarnessModeSpec> {
    builtin_mode_specs()
        .iter()
        .find(|spec| spec.name == name)
        .cloned()
        .or_else(|| mixed_input_case_for_command(name).map(mixed_input_mode_spec))
}

fn all_mode_specs() -> Vec<HarnessModeSpec> {
    let mut specs = builtin_mode_specs().to_vec();
    specs.extend(
        mixed_input_cases()
            .iter()
            .copied()
            .map(mixed_input_mode_spec),
    );
    specs
}

fn suite_default_modes() -> Vec<String> {
    all_mode_specs()
        .into_iter()
        .filter(|spec| spec.suite_default)
        .map(|spec| spec.name)
        .collect()
}

fn supported_mode_names() -> Vec<String> {
    all_mode_specs().into_iter().map(|spec| spec.name).collect()
}

fn unknown_command_error(other: &str) -> String {
    let supported = supported_mode_names();
    format!("unknown command {other:?}; use {}", supported.join(", "))
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

fn command_requires_port_namespace(command: &str) -> bool {
    mode_spec(command)
        .map(|spec| spec.requires_port_namespace)
        .unwrap_or(false)
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

// Measurement-oriented modes are only meaningful when both binaries come from
// the lightweight bench profile, so we fail fast instead of recording skewed
// numbers from debug or release builds.
fn measurement_mode_requires_bench_profile(mode: &str) -> bool {
    mode_spec(mode)
        .map(|spec| spec.requires_bench_profile)
        .unwrap_or(false)
}

fn suite_modes_require_bench_profile(raw: &[String]) -> Result<bool, String> {
    let mut modes = suite_default_modes();
    let mut preflight_only = false;

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
            "--run-id" | "--work-root" => {
                i += 1;
                raw.get(i)
                    .ok_or_else(|| format!("{} requires a value", raw[i - 1]))?;
            }
            "--no-netns" => {}
            "--continue-on-fail" => {}
            "--preflight-only" => preflight_only = true,
            other => return Err(format!("unknown suite option: {other}")),
        }
        i += 1;
    }

    if modes.is_empty() {
        return Err("--only-modes produced an empty mode list".to_string());
    }

    Ok(preflight_only
        || modes
            .iter()
            .any(|mode| measurement_mode_requires_bench_profile(mode)))
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

// ── Generalized harness sink (Phase 1) ──────────────────────────────────────
//
// Extends the existing SinkMetrics from byte-counting to packet-level tracking
// with timestamps, format, keyframe flags, and counts — the single source of
// truth for egress correctness in live tests.

/// Packet-level observation captured by the generalized RTMP sink.
struct SinkPacket {
    media_type: &'static str,
    timestamp_ms: u64,
    audio_packet_type: Option<u8>,
    audio_has_adts_sync: bool,
    video_is_sequence_header: bool,
}

/// Shared counters and packet history for generalized sink assertions.
struct GeneralizedSinkMetrics {
    connections: AtomicUsize,
    publishing: AtomicUsize,
    messages: AtomicU64,
    bytes: AtomicU64,
    video_count: AtomicU64,
    audio_count: AtomicU64,
    keyframe_count: AtomicU64,
    packets: Mutex<Vec<SinkPacket>>,
    video_codec: Mutex<Option<String>>,
    audio_codec: Mutex<Option<String>>,
}

impl Default for GeneralizedSinkMetrics {
    fn default() -> Self {
        Self {
            connections: AtomicUsize::new(0),
            publishing: AtomicUsize::new(0),
            messages: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            video_count: AtomicU64::new(0),
            audio_count: AtomicU64::new(0),
            keyframe_count: AtomicU64::new(0),
            packets: Mutex::new(Vec::new()),
            video_codec: Mutex::new(None),
            audio_codec: Mutex::new(None),
        }
    }
}

impl GeneralizedSinkMetrics {
    fn audio_packet_stats(&self) -> (Option<u8>, u64, u64, u64) {
        let packets = self.packets.lock().unwrap();
        let mut first_audio_packet_type = None;
        let mut audio_sequence_headers = 0;
        let mut audio_raw_packets = 0;
        let mut audio_raw_with_adts = 0;

        for pkt in packets.iter().filter(|pkt| pkt.media_type == "audio") {
            if first_audio_packet_type.is_none() {
                first_audio_packet_type = pkt.audio_packet_type;
            }
            match pkt.audio_packet_type {
                Some(0) => audio_sequence_headers += 1,
                Some(1) => {
                    audio_raw_packets += 1;
                    if pkt.audio_has_adts_sync {
                        audio_raw_with_adts += 1;
                    }
                }
                _ => {}
            }
        }

        (
            first_audio_packet_type,
            audio_sequence_headers,
            audio_raw_packets,
            audio_raw_with_adts,
        )
    }

    fn dts_monotone(&self) -> bool {
        let packets = self.packets.lock().unwrap();
        let mut last_video_ts: Option<u64> = None;
        for pkt in packets.iter() {
            if pkt.media_type == "video" {
                if pkt.video_is_sequence_header {
                    continue;
                }
                if let Some(prev) = last_video_ts
                    && pkt.timestamp_ms <= prev
                {
                    return false;
                }
                last_video_ts = Some(pkt.timestamp_ms);
            }
        }
        true
    }

    fn summary(&self) -> Value {
        let (
            first_audio_packet_type,
            audio_sequence_headers,
            audio_raw_packets,
            audio_raw_with_adts,
        ) = self.audio_packet_stats();
        json!({
            "connections": self.connections.load(Ordering::Relaxed),
            "publishing": self.publishing.load(Ordering::Relaxed),
            "messages": self.messages.load(Ordering::Relaxed),
            "bytes": self.bytes.load(Ordering::Relaxed),
            "videoCount": self.video_count.load(Ordering::Relaxed),
            "audioCount": self.audio_count.load(Ordering::Relaxed),
            "keyframeCount": self.keyframe_count.load(Ordering::Relaxed),
            "dtsMonotone": self.dts_monotone(),
            "firstAudioPacketType": first_audio_packet_type,
            "audioSequenceHeaders": audio_sequence_headers,
            "audioRawPackets": audio_raw_packets,
            "audioRawPacketsWithAdts": audio_raw_with_adts,
        })
    }
}

async fn handle_generalized_sink_client(
    mut socket: TcpStream,
    metrics: Arc<GeneralizedSinkMetrics>,
) -> Result<(), String> {
    metrics.connections.fetch_add(1, Ordering::Relaxed);
    let mut handshake = Handshake::new(PeerType::Server);
    let mut buffer = vec![0u8; 8_192];
    let remaining = loop {
        let n = socket.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("socket closed during handshake".to_string());
        }
        match handshake
            .process_bytes(&buffer[..n])
            .map_err(|e| format!("handshake: {e:?}"))?
        {
            HandshakeProcessResult::InProgress { response_bytes } => {
                socket
                    .write_all(&response_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                socket
                    .write_all(&response_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                break remaining_bytes;
            }
        }
    };

    let (mut session, initial) =
        ServerSession::new(ServerSessionConfig::new()).map_err(|e| format!("{e:?}"))?;
    write_generalized_sink_results(&mut socket, &mut session, initial, &metrics).await?;
    if !remaining.is_empty() {
        let results = session
            .handle_input(&remaining)
            .map_err(|e| format!("{e:?}"))?;
        write_generalized_sink_results(&mut socket, &mut session, results, &metrics).await?;
    }

    loop {
        let n = socket.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        let results = session
            .handle_input(&buffer[..n])
            .map_err(|e| format!("{e:?}"))?;
        write_generalized_sink_results(&mut socket, &mut session, results, &metrics).await?;
    }
}

async fn write_generalized_sink_results(
    socket: &mut TcpStream,
    session: &mut ServerSession,
    results: Vec<ServerSessionResult>,
    metrics: &GeneralizedSinkMetrics,
) -> Result<(), String> {
    let mut pending: VecDeque<_> = results.into();
    while let Some(result) = pending.pop_front() {
        match result {
            ServerSessionResult::OutboundResponse(packet) => {
                socket
                    .write_all(&packet.bytes)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            ServerSessionResult::RaisedEvent(event) => match event {
                ServerSessionEvent::ConnectionRequested { request_id, .. } => {
                    let mut accepted = session
                        .accept_request(request_id)
                        .map_err(|e| format!("{e:?}"))?;
                    pending.extend(accepted.drain(..));
                }
                ServerSessionEvent::PublishStreamRequested { request_id, .. } => {
                    let mut accepted = session
                        .accept_request(request_id)
                        .map_err(|e| format!("{e:?}"))?;
                    metrics.publishing.fetch_add(1, Ordering::Relaxed);
                    pending.extend(accepted.drain(..));
                }
                ServerSessionEvent::VideoDataReceived {
                    data, timestamp, ..
                } => {
                    metrics.messages.fetch_add(1, Ordering::Relaxed);
                    metrics
                        .bytes
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                    metrics.video_count.fetch_add(1, Ordering::Relaxed);
                    let tag = data.first().copied().unwrap_or(0);
                    let is_keyframe = (tag & 0xF0) == 0x10 || tag == 0x90;
                    if is_keyframe {
                        metrics.keyframe_count.fetch_add(1, Ordering::Relaxed);
                    }
                    if metrics.video_codec.lock().unwrap().is_none() {
                        let codec = if tag & 0x80 != 0 {
                            if data.len() >= 5 {
                                match &data[1..5] {
                                    b"hvc1" => Some("hevc"),
                                    b"av01" => Some("av1"),
                                    b"vp09" => Some("vp9"),
                                    _ => Some("h264"),
                                }
                            } else {
                                None
                            }
                        } else {
                            match tag & 0x0F {
                                7 => Some("h264"),
                                12 => Some("hevc"),
                                _ => None,
                            }
                        };
                        if let Some(c) = codec {
                            *metrics.video_codec.lock().unwrap() = Some(c.to_string());
                        }
                    }
                    if let Ok(mut pkts) = metrics.packets.lock() {
                        pkts.push(SinkPacket {
                            media_type: "video",
                            timestamp_ms: timestamp.value as u64,
                            audio_packet_type: None,
                            audio_has_adts_sync: false,
                            video_is_sequence_header: (tag & 0x80) == 0
                                && data.get(1).copied() == Some(0),
                        });
                    }
                }
                ServerSessionEvent::AudioDataReceived {
                    data, timestamp, ..
                } => {
                    metrics.messages.fetch_add(1, Ordering::Relaxed);
                    metrics
                        .bytes
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                    metrics.audio_count.fetch_add(1, Ordering::Relaxed);
                    if metrics.audio_codec.lock().unwrap().is_none()
                        && let Some(&tag) = data.first()
                    {
                        let codec = match (tag >> 4) & 0x0F {
                            10 => Some("aac"),
                            2 => Some("mp3"),
                            _ => None,
                        };
                        if let Some(c) = codec {
                            *metrics.audio_codec.lock().unwrap() = Some(c.to_string());
                        }
                    }
                    let audio_packet_type = data.get(1).copied();
                    let audio_has_adts_sync =
                        data.len() >= 4 && data[2] == 0xFF && (data[3] & 0xF0) == 0xF0;
                    if let Ok(mut pkts) = metrics.packets.lock() {
                        pkts.push(SinkPacket {
                            media_type: "audio",
                            timestamp_ms: timestamp.value as u64,
                            audio_packet_type,
                            audio_has_adts_sync,
                            video_is_sequence_header: false,
                        });
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(())
}

// ── Harness sink probe (Phase 4) ──────────────────────────────────────────
//
// Spins up a generalized sink, creates an output pointed at it, waits for
// packets, asserts DTS monotonicity / video+audio presence / keyframes,
// then tears down. Returns the sink summary for embedding in test results.

/// Result bundle returned by the live egress sink-probe helper.
struct SinkProbeResult {
    passed: bool,
    summary: Value,
    output_id: String,
}

/// Running generalized RTMP sink and its spawned connection tasks.
struct GeneralizedSinkServer {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    reader_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

/// RTMP sink that intentionally stops reading to exercise output-stall handling.
struct StalledRtmpSinkServer {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    publish_accepted: Arc<std::sync::atomic::AtomicBool>,
}

fn set_socket_recv_buffer(socket: &TcpStream, size: libc::c_int) -> Result<(), String> {
    // SAFETY: `socket.as_raw_fd()` is a live socket descriptor for the duration
    // of this call, and `size` points to initialized stack memory of the
    // expected type for `SO_RCVBUF`.
    let rc = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &size as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

async fn start_generalized_sink_server(
    sink_port: u16,
    metrics: Arc<GeneralizedSinkMetrics>,
) -> Result<GeneralizedSinkServer, String> {
    let listener = TcpListener::bind(format!("127.0.0.1:{sink_port}"))
        .await
        .map_err(|e| format!("sink bind {sink_port}: {e}"))?;
    let cancel = CancellationToken::new();
    let reader_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(Mutex::new(Vec::new()));
    let reader_handles_inner = reader_handles.clone();
    let metrics_inner = metrics.clone();
    let cancel_inner = cancel.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                result = listener.accept() => {
                    if let Ok((socket, _)) = result {
                        let metrics = metrics_inner.clone();
                        let handle = tokio::spawn(async move {
                            let _ = handle_generalized_sink_client(socket, metrics).await;
                        });
                        reader_handles_inner.lock().unwrap().push(handle);
                    }
                }
                _ = cancel_inner.cancelled() => break,
            }
        }
    });

    Ok(GeneralizedSinkServer {
        cancel,
        task,
        reader_handles,
    })
}

async fn handle_stalled_rtmp_sink_client(
    mut socket: TcpStream,
    publish_accepted: Arc<std::sync::atomic::AtomicBool>,
    cancel: CancellationToken,
) -> Result<(), String> {
    let _ = set_socket_recv_buffer(&socket, 4 * 1024);
    let mut handshake = Handshake::new(PeerType::Server);
    let mut buffer = vec![0u8; 8_192];
    let remaining = loop {
        let n = socket.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("socket closed during handshake".to_string());
        }
        match handshake
            .process_bytes(&buffer[..n])
            .map_err(|e| format!("handshake: {e:?}"))?
        {
            HandshakeProcessResult::InProgress { response_bytes } => {
                socket
                    .write_all(&response_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                socket
                    .write_all(&response_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                break remaining_bytes;
            }
        }
    };

    let (mut session, initial) =
        ServerSession::new(ServerSessionConfig::new()).map_err(|e| format!("{e:?}"))?;
    let mut pending: VecDeque<_> = initial.into();
    if !remaining.is_empty() {
        pending.extend(
            session
                .handle_input(&remaining)
                .map_err(|e| format!("{e:?}"))?,
        );
    }

    loop {
        while let Some(result) = pending.pop_front() {
            match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    socket
                        .write_all(&packet.bytes)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                ServerSessionResult::RaisedEvent(event) => match event {
                    ServerSessionEvent::ConnectionRequested { request_id, .. } => {
                        let mut accepted = session
                            .accept_request(request_id)
                            .map_err(|e| format!("{e:?}"))?;
                        pending.extend(accepted.drain(..));
                    }
                    ServerSessionEvent::PublishStreamRequested { request_id, .. } => {
                        let mut accepted = session
                            .accept_request(request_id)
                            .map_err(|e| format!("{e:?}"))?;
                        publish_accepted.store(true, Ordering::Relaxed);
                        pending.extend(accepted.drain(..));
                        while let Some(response) = pending.pop_front() {
                            if let ServerSessionResult::OutboundResponse(packet) = response {
                                socket
                                    .write_all(&packet.bytes)
                                    .await
                                    .map_err(|e| e.to_string())?;
                            }
                        }
                        loop {
                            tokio::select! {
                                _ = cancel.cancelled() => return Ok(()),
                                _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        let n = socket.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        pending = session
            .handle_input(&buffer[..n])
            .map(|results| results.into())
            .map_err(|e| format!("{e:?}"))?;
    }
}

async fn start_stalled_rtmp_sink_server(sink_port: u16) -> Result<StalledRtmpSinkServer, String> {
    let listener = TcpListener::bind(format!("127.0.0.1:{sink_port}"))
        .await
        .map_err(|e| format!("stall sink bind {sink_port}: {e}"))?;
    let cancel = CancellationToken::new();
    let cancel_inner = cancel.clone();
    let publish_accepted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let publish_accepted_inner = publish_accepted.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                result = listener.accept() => {
                    if let Ok((socket, _)) = result {
                        let accepted = publish_accepted_inner.clone();
                        let cancel_client = cancel_inner.clone();
                        tokio::spawn(async move {
                            let _ = handle_stalled_rtmp_sink_client(socket, accepted, cancel_client).await;
                        });
                    }
                }
                _ = cancel_inner.cancelled() => break,
            }
        }
    });

    Ok(StalledRtmpSinkServer {
        cancel,
        task,
        publish_accepted,
    })
}

fn stop_stalled_rtmp_sink_server(server: StalledRtmpSinkServer) {
    server.cancel.cancel();
    server.task.abort();
}

fn stop_generalized_sink_server(server: GeneralizedSinkServer) {
    server.cancel.cancel();
    server.task.abort();
    let handles = server.reader_handles.lock().unwrap();
    for handle in handles.iter() {
        handle.abort();
    }
}

fn output_create_payload(name: &str, url: &str, encoding: &str) -> Value {
    json!({
        "name": name,
        "url": url,
        "config": OutputConfig::parse(encoding),
    })
}

async fn run_sink_probe(
    api: &RampApi,
    pipeline_id: &str,
    label: &str,
    encoding: &str,
    sink_port: u16,
    min_video: u64,
) -> Result<SinkProbeResult, String> {
    let metrics = Arc::new(GeneralizedSinkMetrics::default());
    let server = start_generalized_sink_server(sink_port, metrics.clone()).await?;
    let sink_url = format!("rtmp://127.0.0.1:{sink_port}/live/sink-probe-{label}");
    let output_id = match create_output(
        api,
        pipeline_id,
        &format!("sink-{label}"),
        &sink_url,
        encoding,
    )
    .await
    {
        Ok(output_id) => output_id,
        Err(error) => {
            stop_generalized_sink_server(server);
            return Err(error);
        }
    };
    if let Err(error) = start_output(api, pipeline_id, &output_id).await {
        stop_generalized_sink_server(server);
        return Err(error);
    }

    let deadline = Instant::now() + Duration::from_secs(20);
    while metrics.video_count.load(Ordering::Relaxed) < min_video {
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    let dts_ok = metrics.dts_monotone();
    let video = metrics.video_count.load(Ordering::Relaxed);
    let audio = metrics.audio_count.load(Ordering::Relaxed);
    let keyframes = metrics.keyframe_count.load(Ordering::Relaxed);
    let summary = metrics.summary();
    stop_generalized_sink_server(server);

    // Stop the output
    let _ = api
        .post_empty(&format!(
            "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop"
        ))
        .await;

    let passed = video >= min_video && audio > 0 && keyframes > 0 && dts_ok;
    if !passed {
        eprintln!(
            "[sink-probe:{label}] FAIL: video={video} audio={audio} keyframes={keyframes} dts_monotone={dts_ok}"
        );
    } else {
        println!(
            "[sink-probe:{label}] ok: video={video} audio={audio} keyframes={keyframes} dts_monotone={dts_ok}"
        );
    }

    Ok(SinkProbeResult {
        passed,
        summary,
        output_id,
    })
}

/// Result bundle returned by the synthetic HLS PUT upload probe.
struct HlsPutProbeResult {
    passed: bool,
    summary: Value,
    output_id: String,
}

async fn run_hls_put_probe(
    api: &RampApi,
    pipeline_id: &str,
    label: &str,
    put_port: u16,
) -> Result<HlsPutProbeResult, String> {
    let sink_dir = artifact_path(&format!("hls-put-probe-{label}"));
    let _ = std::fs::remove_dir_all(&sink_dir);
    std::fs::create_dir_all(&sink_dir).map_err(|e| e.to_string())?;

    let (sink_cancel, sink_handle) = start_hls_put_sink(put_port, sink_dir.clone()).await?;

    let put_url =
        format!("http://127.0.0.1:{put_port}/upload?cid=probe-{label}&copy=0&file=out.m3u8");
    let output_id = create_output(
        api,
        pipeline_id,
        &format!("hls-put-{label}"),
        &put_url,
        "source",
    )
    .await?;
    start_output(api, pipeline_id, &output_id).await?;

    let artifacts = wait_for_hls_put_artifacts(&sink_dir, Duration::from_secs(30)).await;
    let mut playlist_ok = false;
    let mut content_types_ok = false;
    let mut segment_ok = false;

    if let Ok(ref arts) = artifacts {
        playlist_ok = validate_hls_playlist(&arts.youtube_playlist, "probe").is_ok();

        if let Ok(requests) = read_hls_put_requests(&sink_dir) {
            let playlist_ct = request_seen(&requests, |r| {
                r["file"] == "out.m3u8" && r["contentType"] == "application/vnd.apple.mpegurl"
            });
            let segment_ct = request_seen(&requests, |r| {
                r["file"]
                    .as_str()
                    .is_some_and(|f| is_segment_file(f, "seg"))
                    && r["contentType"] == "video/mp2t"
            });
            content_types_ok = playlist_ct && segment_ct;
        }

        if let Ok(probe) = ffprobe(&arts.youtube_segment.to_string_lossy()).await {
            let has_video = probe["streams"]
                .as_array()
                .is_some_and(|s| s.iter().any(|s| s["codec_type"] == "video"));
            let has_audio = probe["streams"]
                .as_array()
                .is_some_and(|s| s.iter().any(|s| s["codec_type"] == "audio"));
            segment_ok = has_video && has_audio;
        }
    }

    let status = api
        .get_json(&format!(
            "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/status"
        ))
        .await
        .ok();
    let status_ok = status
        .as_ref()
        .is_some_and(|s| s["bytesOut"].as_u64().unwrap_or(0) > 0);

    let _ = api
        .post_empty(&format!(
            "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop"
        ))
        .await;

    sink_cancel.cancel();
    let _ = sink_handle.await;

    let passed = playlist_ok && content_types_ok && segment_ok && status_ok;
    let summary = json!({
        "playlistValid": playlist_ok,
        "contentTypesCorrect": content_types_ok,
        "segmentDecodable": segment_ok,
        "artifactsFound": artifacts.is_ok(),
        "outputStatus": status,
    });

    if !passed {
        eprintln!(
            "[hls-put-probe:{label}] FAIL: playlist={playlist_ok} content_types={content_types_ok} segment={segment_ok} status={status_ok}"
        );
    } else {
        println!(
            "[hls-put-probe:{label}] ok: playlist={playlist_ok} content_types={content_types_ok} segment={segment_ok} status={status_ok}"
        );
    }

    Ok(HlsPutProbeResult {
        passed,
        summary,
        output_id,
    })
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

/// Resource-sweep process lifetime model for publishers and outputs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ResourceSweepLifecycle {
    Isolated,
    Continuous,
    Cumulative,
}

impl ResourceSweepLifecycle {
    fn from_env() -> Result<Self, String> {
        match std::env::var("RESOURCE_SWEEP_LIFECYCLE")
            .unwrap_or_else(|_| "isolated".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "isolated" => Ok(Self::Isolated),
            "continuous" => Ok(Self::Continuous),
            "cumulative" => Ok(Self::Cumulative),
            other => Err(format!(
                "RESOURCE_SWEEP_LIFECYCLE must be isolated, continuous, or cumulative (got {other})"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Isolated => "isolated",
            Self::Continuous => "continuous",
            Self::Cumulative => "cumulative",
        }
    }
}

/// Environment and output paths for resource-sweep measurement runs.
#[derive(Clone)]
struct ResourceSweepEnv {
    work_dir: PathBuf,
    summary_json: PathBuf,
    summary_csv: PathBuf,
    samples_jsonl: PathBuf,
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
    sample_secs: u64,
    sample_interval_ms: u64,
    settle_secs: u64,
    ingest_counts: Vec<usize>,
    egress_counts: Vec<usize>,
    scenario_filter: Option<HashSet<String>>,
    lifecycle: ResourceSweepLifecycle,
    no_cleanup: bool,
    srt_crypto: HarnessSrtCrypto,
}

impl ResourceSweepEnv {
    fn from_env() -> Result<Self, String> {
        Self::from_env_with_default_dir("test/artifacts/resource-sweep")
    }

    fn from_env_with_default_dir(default_dir: &str) -> Result<Self, String> {
        let work_dir = std::env::var_os("WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(default_dir));
        let ports = harness_port_defaults();
        Ok(Self {
            summary_json: work_dir.join("resource-sweep-results.json"),
            summary_csv: work_dir.join("resource-sweep-results.csv"),
            samples_jsonl: work_dir.join("resource-sweep-samples.jsonl"),
            restream_log: work_dir.join("restream.log"),
            mediamtx_log: work_dir.join("mediamtx.log"),
            mediamtx_config: work_dir.join("mediamtx.yml"),
            restream_bin: default_restream_bin(),
            restream_db_path: std::env::var_os("RESTREAM_DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_work_db_path(&work_dir, "resource-sweep.db")),
            restream_http: ports.restream_http,
            restream_rtmp: ports.restream_rtmp,
            restream_srt: ports.restream_srt,
            mtx_rtmp: ports.mtx_rtmp,
            mtx_srt: ports.mtx_srt,
            mtx_api: ports.mtx_api,
            sample_secs: env_secs("RESOURCE_SWEEP_SAMPLE_SECS", 6),
            sample_interval_ms: env_secs("RESOURCE_SWEEP_SAMPLE_INTERVAL_MS", 1000),
            settle_secs: env_secs("RESOURCE_SWEEP_SETTLE_SECS", 4),
            ingest_counts: parse_usize_list("RESOURCE_SWEEP_INGEST_COUNTS", "1,3,5"),
            egress_counts: parse_usize_list("RESOURCE_SWEEP_EGRESS_COUNTS", "1,5,10"),
            scenario_filter: parse_string_set("RESOURCE_SWEEP_SCENARIOS"),
            lifecycle: ResourceSweepLifecycle::from_env()?,
            no_cleanup: std::env::var("RESOURCE_SWEEP_NO_CLEANUP")
                .ok()
                .is_some_and(|v| v == "1"),
            srt_crypto: harness_srt_crypto_from_env(),
            work_dir,
        })
    }

    fn scenario_enabled(&self, scenario: &str) -> bool {
        self.scenario_filter
            .as_ref()
            .is_none_or(|filter| filter.contains(scenario))
    }
}

fn parse_usize_list(name: &str, default: &str) -> Vec<usize> {
    std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .filter_map(|part| part.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .collect()
}

fn parse_string_set(name: &str) -> Option<HashSet<String>> {
    let values: HashSet<String> = std::env::var(name)
        .ok()?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    (!values.is_empty()).then_some(values)
}

fn parse_bitrate_specs(name: &str, default: &str) -> Result<Vec<BitrateSpec>, String> {
    let mut out = Vec::new();
    for part in std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let normalized = part.to_ascii_uppercase();
        let mbps = if let Some(value) = normalized.strip_suffix('M') {
            value
                .parse::<f64>()
                .map_err(|_| format!("invalid Mbps bitrate {part:?}"))?
        } else if let Some(value) = normalized.strip_suffix('K') {
            value
                .parse::<f64>()
                .map_err(|_| format!("invalid Kbps bitrate {part:?}"))?
                / 1000.0
        } else {
            normalized
                .parse::<f64>()
                .map_err(|_| format!("invalid bitrate {part:?}"))?
        };
        out.push(BitrateSpec {
            label: part.to_string(),
            mbps,
        });
    }
    if out.is_empty() {
        return Err(format!("{name} produced no bitrate values"));
    }
    Ok(out)
}

fn parse_sweep_configs(name: &str) -> Result<Vec<SweepConfig>, String> {
    let raw = std::env::var(name).unwrap_or_else(|_| {
        sweep_configs()
            .iter()
            .map(|cfg| cfg.name)
            .collect::<Vec<_>>()
            .join(",")
    });
    let mut out = Vec::new();
    for part in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let config = sweep_configs()
            .iter()
            .copied()
            .find(|cfg| cfg.name == part)
            .ok_or_else(|| format!("unknown sweep config {part:?}"))?;
        out.push(config);
    }
    if out.is_empty() {
        return Err(format!("{name} produced no configs"));
    }
    Ok(out)
}

/// Input fixture shape used by resource and bitrate sweep families.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SweepConfig {
    name: &'static str,
    ingest_proto: &'static str,
    video_codec: &'static str,
    multi_audio: bool,
}

static SWEEP_CONFIGS_FROM_DSL: OnceLock<Vec<SweepConfig>> = OnceLock::new();

fn sweep_configs() -> &'static [SweepConfig] {
    SWEEP_CONFIGS_FROM_DSL.get_or_init(|| {
        serde_json::from_str(include_str!("test_harness/sweep_configs.json"))
            .expect("embedded sweep_configs.json should define valid sweep rows")
    })
}

/// Output shape used by resource-sweep scenarios.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum SweepOutputKind {
    RtmpSource,
    SrtSource,
    Rtmp720p,
    Srt720p,
    Rtmp1080p,
    Srt1080p,
}

impl SweepOutputKind {
    fn label(self) -> &'static str {
        match self {
            Self::RtmpSource => "rtmp-source",
            Self::SrtSource => "srt-source",
            Self::Rtmp720p => "rtmp.720p.a0",
            Self::Srt720p => "srt.720p.a0",
            Self::Rtmp1080p => "rtmp.1080p.a0",
            Self::Srt1080p => "srt.1080p.a0",
        }
    }

    fn publish_url(self, rtmp_port: u16, srt_port: u16, name: &str) -> String {
        match self {
            Self::RtmpSource | Self::Rtmp720p | Self::Rtmp1080p => {
                format!("rtmp://127.0.0.1:{rtmp_port}/live/{name}")
            }
            Self::SrtSource | Self::Srt720p | Self::Srt1080p => {
                format!("srt://127.0.0.1:{srt_port}?streamid=publish:live/{name}")
            }
        }
    }

    fn read_url(self, rtmp_port: u16, srt_port: u16, name: &str) -> String {
        match self {
            Self::RtmpSource | Self::Rtmp720p | Self::Rtmp1080p => {
                format!("rtmp://127.0.0.1:{rtmp_port}/live/{name}")
            }
            Self::SrtSource | Self::Srt720p | Self::Srt1080p => {
                format!("srt://127.0.0.1:{srt_port}?streamid=read:live/{name}&timeout=30000000")
            }
        }
    }

    const fn encoding(self, multi_audio: bool) -> &'static str {
        match (self, multi_audio) {
            (Self::RtmpSource | Self::SrtSource, _) => "source",
            (Self::Rtmp720p, true) => "720p+atrack:0",
            (Self::Srt720p, true) => "720p+atrack:0,1",
            (Self::Rtmp720p | Self::Srt720p, false) => "720p",
            (Self::Rtmp1080p, true) => "1080p+atrack:0",
            (Self::Srt1080p, true) => "1080p+atrack:0,1",
            (Self::Rtmp1080p | Self::Srt1080p, false) => "1080p",
        }
    }
}

/// Declarative resource-sweep egress scenario row.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceEgressScenario {
    name: String,
    config_index: usize,
    output_kinds: Vec<SweepOutputKind>,
    branch_order: Option<usize>,
    branch_label: Option<&'static str>,
}

impl ResourceEgressScenario {
    fn branch_label(&self) -> &'static str {
        self.branch_label.unwrap_or("other")
    }
}

static RESOURCE_EGRESS_SCENARIOS_FROM_DSL: OnceLock<Vec<ResourceEgressScenario>> = OnceLock::new();

fn resource_egress_scenarios() -> &'static [ResourceEgressScenario] {
    RESOURCE_EGRESS_SCENARIOS_FROM_DSL.get_or_init(|| {
        serde_json::from_str(include_str!("test_harness/resource_egress_scenarios.json"))
            .expect("embedded resource_egress_scenarios.json should define valid resource rows")
    })
}

fn resource_egress_scenario(name: &str) -> Option<&'static ResourceEgressScenario> {
    resource_egress_scenarios()
        .iter()
        .find(|scenario| scenario.name == name)
}

/// Live process stack shared by a resource-sweep sample.
struct ResourceSweepStack {
    mediamtx: Child,
    restream: Child,
    api: RampApi,
    restream_pid: u32,
}

/// Environment and output paths for branch-matrix runs.
#[derive(Clone)]
struct BranchMatrixEnv {
    resource: ResourceSweepEnv,
    summary_json: PathBuf,
    summary_csv: PathBuf,
    summary_md: PathBuf,
    backend: String,
    srt_variants: Vec<HarnessSrtCrypto>,
    scenario_filter: Option<HashSet<String>>,
}

impl BranchMatrixEnv {
    fn from_env() -> Result<Self, String> {
        let mut resource =
            ResourceSweepEnv::from_env_with_default_dir("test/artifacts/branch-matrix")?;
        let work_dir = resource.work_dir.clone();
        let egress_count = env_usize("BRANCH_MATRIX_EGRESS_COUNT", 10).max(1);
        resource.egress_counts = vec![egress_count];
        resource.ingest_counts = vec![1];
        resource.summary_json = work_dir.join("branch-matrix-results.json");
        resource.summary_csv = work_dir.join("branch-matrix-results.csv");
        resource.samples_jsonl = work_dir.join("branch-matrix-samples.jsonl");
        if std::env::var_os("RESTREAM_DB_PATH").is_none() {
            resource.restream_db_path = work_dir.join("branch-matrix.db");
        }
        Ok(Self {
            summary_json: work_dir.join("branch-matrix-results.json"),
            summary_csv: work_dir.join("branch-matrix-results.csv"),
            summary_md: work_dir.join("branch-matrix-summary.md"),
            backend: {
                let policy = restream::planner::backend_policy::BackendPolicy::from_env();
                if policy.internal_video_presets
                    || policy.internal_hevc_to_h264
                    || policy.internal_hls_preview
                    || policy.internal_complex_audio
                {
                    "internal".to_string()
                } else {
                    "external".to_string()
                }
            },
            srt_variants: vec![harness_srt_crypto_from_env()],
            scenario_filter: parse_string_set("BRANCH_MATRIX_SCENARIOS"),
            resource,
        })
    }

    fn scenario_enabled(&self, scenario: &str) -> bool {
        self.scenario_filter
            .as_ref()
            .is_none_or(|filter| filter.contains(scenario))
    }
}

/// Environment and output paths for bitrate-sweep measurement runs.
struct BitrateSweepEnv {
    work_dir: PathBuf,
    summary_json: PathBuf,
    summary_csv: PathBuf,
    samples_jsonl: PathBuf,
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
    stabilize_secs: u64,
    sample_interval_secs: u64,
    output_groups: usize,
    no_cleanup: bool,
    bitrates: Vec<BitrateSpec>,
    configs: Vec<SweepConfig>,
}

impl BitrateSweepEnv {
    fn from_env() -> Result<Self, String> {
        let work_dir = std::env::var_os("WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("test/artifacts/bitrate-sweep"));
        let ports = harness_port_defaults();
        Ok(Self {
            summary_json: work_dir.join("bitrate-sweep-results.json"),
            summary_csv: work_dir.join("bitrate-sweep-results.csv"),
            samples_jsonl: work_dir.join("bitrate-sweep-samples.jsonl"),
            restream_log: work_dir.join("restream.log"),
            mediamtx_log: work_dir.join("mediamtx.log"),
            mediamtx_config: work_dir.join("mediamtx.yml"),
            restream_bin: default_restream_bin(),
            restream_db_path: std::env::var_os("RESTREAM_DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_work_db_path(&work_dir, "bitrate-sweep.db")),
            restream_http: ports.restream_http,
            restream_rtmp: ports.restream_rtmp,
            restream_srt: ports.restream_srt,
            mtx_rtmp: ports.mtx_rtmp,
            mtx_srt: ports.mtx_srt,
            mtx_api: ports.mtx_api,
            stabilize_secs: env_secs("BITRATE_SWEEP_STABILIZE_SECS", 30),
            sample_interval_secs: env_secs("BITRATE_SWEEP_SAMPLE_INTERVAL_SECS", 5).max(1),
            output_groups: env_usize("BITRATE_SWEEP_OUTPUT_GROUPS", 1).max(1),
            no_cleanup: std::env::var("BITRATE_SWEEP_NO_CLEANUP")
                .ok()
                .is_some_and(|v| v == "1"),
            bitrates: parse_bitrate_specs("BITRATE_SWEEP_BITRATES", "1.5M,4M,8M")?,
            configs: parse_sweep_configs("BITRATE_SWEEP_CONFIGS")?,
            work_dir,
        })
    }
}

/// One target bitrate value in a bitrate sweep.
#[derive(Clone)]
struct BitrateSpec {
    label: String,
    mbps: f64,
}

/// One periodic resource sample captured during a bitrate-sweep case.
#[derive(Clone)]
struct BitrateSweepSample {
    config: String,
    bitrate_label: String,
    bitrate_mbps: f64,
    elapsed_secs: u64,
    restream_cpu_pct: f64,
    ffmpeg_cpu_pct: f64,
    total_cpu_pct: f64,
    restream_rss_kb: u64,
    ffmpeg_count: u64,
    ffmpeg_rss_kb: u64,
    total_rss_kb: u64,
    retained_payload_kb: u64,
    source_ring_kb: u64,
    transcoder_ring_kb: u64,
    tsmux_ring_kb: u64,
    avio_len_kb: u64,
    avio_hwm_kb: u64,
    overflow_count: u64,
}

/// Aggregated result for one bitrate/config/output-count case.
struct BitrateSweepCase {
    config: String,
    ingest_proto: String,
    video_codec: String,
    multi_audio: bool,
    bitrate_label: String,
    bitrate_mbps: f64,
    output_groups: usize,
    outputs_total: usize,
    restream_rss_base_kb: u64,
    restream_rss_final_kb: u64,
    restream_rss_delta_kb: u64,
    restream_rss_peak_kb: u64,
    ffmpeg_count_peak: u64,
    ffmpeg_rss_peak_kb: u64,
    total_rss_peak_kb: u64,
    restream_cpu_avg_pct: f64,
    restream_cpu_peak_pct: f64,
    ffmpeg_cpu_avg_pct: f64,
    ffmpeg_cpu_peak_pct: f64,
    total_cpu_avg_pct: f64,
    total_cpu_peak_pct: f64,
    retained_payload_min_kb: u64,
    retained_payload_max_kb: u64,
    retained_payload_final_kb: u64,
    retained_growth_kb_per_min: f64,
    source_ring_peak_kb: u64,
    transcoder_ring_peak_kb: u64,
    tsmux_ring_peak_kb: u64,
    avio_len_peak_kb: u64,
    avio_hwm_peak_kb: u64,
    overflow_count_final: u64,
    correctness_ok: bool,
    correctness_failures: Vec<String>,
}

/// One periodic process/memory sample in resource-oriented sweeps.
#[derive(Clone)]
struct ResourceSample {
    scenario: String,
    label: String,
    lifecycle: String,
    pipelines: usize,
    outputs: usize,
    ingest_types: String,
    egress_mix: String,
    transcode: String,
    restream_cpu_pct: f64,
    ffmpeg_cpu_pct: f64,
    total_cpu_pct: f64,
    rss_kb: u64,
    ffmpeg_count: u64,
    ffmpeg_rss_kb: u64,
    anonymous_kb: u64,
    private_dirty_kb: u64,
    private_clean_kb: u64,
    shared_clean_kb: u64,
    shared_dirty_kb: u64,
    pss_kb: u64,
    swap_kb: u64,
    retained_kb: u64,
    source_ring_kb: u64,
    transcoder_ring_kb: u64,
    tsmux_ring_kb: u64,
    avio_len_kb: u64,
    avio_hwm_kb: u64,
    active_transcoder_buffers: u64,
    ingests: usize,
    egresses: usize,
    stages: usize,
    pipeline_count: usize,
    unattributed_kb: u64,
}

/// Rollup statistics for a resource-sweep scenario.
struct ResourceAggregate {
    scenario: String,
    label: String,
    lifecycle: String,
    pipelines: usize,
    outputs: usize,
    ingest_types: String,
    egress_mix: String,
    transcode: String,
    sample_count: usize,
    restream_cpu_avg_pct: f64,
    restream_cpu_peak_pct: f64,
    ffmpeg_cpu_avg_pct: f64,
    ffmpeg_cpu_peak_pct: f64,
    total_cpu_avg_pct: f64,
    total_cpu_peak_pct: f64,
    rss_avg_kb: f64,
    rss_peak_kb: u64,
    ffmpeg_rss_peak_kb: u64,
    retained_peak_kb: u64,
    source_ring_peak_kb: u64,
    transcoder_ring_peak_kb: u64,
    tsmux_ring_peak_kb: u64,
    avio_len_peak_kb: u64,
    avio_hwm_peak_kb: u64,
    anonymous_peak_kb: u64,
    private_dirty_peak_kb: u64,
    shared_clean_peak_kb: u64,
    pss_peak_kb: u64,
    unattributed_peak_kb: u64,
    active_transcoder_buffers_peak: u64,
    ingests_peak: usize,
    egresses_peak: usize,
    stages_peak: usize,
    pipeline_count_peak: usize,
}

/// Static labels and dimensions for one resource-sweep scenario.
struct ResourceScenarioMeta<'a> {
    scenario: &'a str,
    label: String,
    pipelines: usize,
    outputs: usize,
    ingest_types: String,
    egress_mix: String,
    transcode: &'a str,
}

/// Parsed `/proc/<pid>/smaps_rollup` memory counters used for attribution.
struct ProcMemRollup {
    anonymous_kb: u64,
    private_dirty_kb: u64,
    private_clean_kb: u64,
    shared_clean_kb: u64,
    shared_dirty_kb: u64,
    pss_kb: u64,
    swap_kb: u64,
}

async fn resource_sweep() -> Result<Value, String> {
    let env = ResourceSweepEnv::from_env()?;
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&env.summary_csv);
    let _ = std::fs::remove_file(&env.summary_json);
    let _ = std::fs::remove_file(&env.samples_jsonl);

    let mut stack = if env.lifecycle == ResourceSweepLifecycle::Isolated {
        None
    } else {
        Some(start_resource_sweep_stack(&env).await?)
    };
    let mut retained_publishers: Vec<Child> = Vec::new();
    let mut aggregates = Vec::new();

    if env.scenario_enabled("baseline-empty") {
        aggregates.push(run_resource_baseline(&env, &mut stack, &mut retained_publishers).await?);
    }
    if env.scenario_enabled("ingest-only") {
        for config in sweep_configs() {
            aggregates.push(
                run_resource_ingest_only(&env, &mut stack, &mut retained_publishers, *config)
                    .await?,
            );
        }
    }
    if env.scenario_enabled("ingest-growth-same") {
        aggregates.extend(
            run_resource_ingest_growth(&env, &mut stack, &mut retained_publishers, false).await?,
        );
    }
    if env.scenario_enabled("ingest-growth-mixed") {
        aggregates.extend(
            run_resource_ingest_growth(&env, &mut stack, &mut retained_publishers, true).await?,
        );
    }
    for scenario in resource_egress_scenarios() {
        if !env.scenario_enabled(&scenario.name) {
            continue;
        }
        aggregates.extend(
            run_resource_egress_growth(
                &env,
                &mut stack,
                &mut retained_publishers,
                &scenario.name,
                sweep_configs()[scenario.config_index],
                &scenario.output_kinds,
            )
            .await?,
        );
    }

    write_resource_sweep_csv(&env.summary_csv, &aggregates)?;
    let result = json!({
        "mode": "resource-sweep",
        "lifecycle": env.lifecycle.as_str(),
        "artifacts": {
            "summaryJson": env.summary_json,
            "summaryCsv": env.summary_csv,
            "samplesJsonl": env.samples_jsonl,
            "restreamLog": env.restream_log,
            "mediamtxLog": env.mediamtx_log,
        },
        "aggregates": aggregates.iter().map(resource_aggregate_json).collect::<Vec<_>>(),
    });
    std::fs::write(
        &env.summary_json,
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .map_err(|e| e.to_string())?;

    if env.no_cleanup {
        println!("resource-sweep no-cleanup: leaving final stack running");
        // kill_on_drop(true) is set at spawn time for these children, so simply
        // skipping stop_child() isn't enough — dropping the Child handles below
        // (at function return) would still SIGKILL them. mem::forget leaks the
        // handles instead, which is fine since the process is about to _exit.
        for child in retained_publishers.drain(..) {
            std::mem::forget(child);
        }
        if let Some(stack) = stack.take() {
            std::mem::forget(stack);
        }
    } else {
        for child in &mut retained_publishers {
            stop_child(child).await;
        }
        if let Some(stack) = stack.as_mut() {
            stop_child(&mut stack.restream).await;
            stop_child(&mut stack.mediamtx).await;
        }
    }
    Ok(result)
}

async fn branch_matrix() -> Result<Value, String> {
    let env = BranchMatrixEnv::from_env()?;
    run_branch_matrix_variant(&env).await
}

async fn srt_crypto_matrix() -> Result<Value, String> {
    let mut env = BranchMatrixEnv::from_env()?;
    env.srt_variants =
        parse_srt_crypto_variants("SRT_CRYPTO_MATRIX_VARIANTS", "plaintext,enc16,enc24,enc32")?;

    let parent_work_dir = env.resource.work_dir.clone();
    let mut runs = Vec::new();
    for crypto in env.srt_variants.clone() {
        let mut variant_env = env.clone();
        variant_env.resource.srt_crypto = crypto.clone();
        variant_env.resource.work_dir = parent_work_dir.join(&crypto.label);
        variant_env.resource.summary_json = variant_env
            .resource
            .work_dir
            .join("branch-matrix-results.json");
        variant_env.resource.summary_csv = variant_env
            .resource
            .work_dir
            .join("branch-matrix-results.csv");
        variant_env.resource.samples_jsonl = variant_env
            .resource
            .work_dir
            .join("branch-matrix-samples.jsonl");
        variant_env.resource.restream_log = variant_env.resource.work_dir.join("restream.log");
        variant_env.resource.mediamtx_log = variant_env.resource.work_dir.join("mediamtx.log");
        variant_env.resource.mediamtx_config = variant_env.resource.work_dir.join("mediamtx.yml");
        variant_env.resource.restream_db_path =
            variant_env.resource.work_dir.join("branch-matrix.db");
        variant_env.summary_json = variant_env
            .resource
            .work_dir
            .join("branch-matrix-results.json");
        variant_env.summary_csv = variant_env
            .resource
            .work_dir
            .join("branch-matrix-results.csv");
        variant_env.summary_md = variant_env
            .resource
            .work_dir
            .join("branch-matrix-summary.md");
        runs.push(run_branch_matrix_variant(&variant_env).await?);
    }

    Ok(json!({
        "mode": "srt-crypto-matrix",
        "variants": runs,
    }))
}

async fn run_branch_matrix_variant(env: &BranchMatrixEnv) -> Result<Value, String> {
    let resource = &env.resource;
    std::fs::create_dir_all(&resource.work_dir).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&env.summary_csv);
    let _ = std::fs::remove_file(&env.summary_json);
    let _ = std::fs::remove_file(&env.summary_md);
    let _ = std::fs::remove_file(&resource.samples_jsonl);

    let mut stack = if resource.lifecycle == ResourceSweepLifecycle::Isolated {
        None
    } else {
        Some(start_resource_sweep_stack(resource).await?)
    };
    let mut retained_publishers: Vec<Child> = Vec::new();
    let mut aggregates = Vec::new();

    for scenario in resource_egress_scenarios()
        .iter()
        .filter(|scenario| scenario.branch_order.is_some())
    {
        if !env.scenario_enabled(&scenario.name) {
            continue;
        }
        aggregates.extend(
            run_resource_egress_growth(
                resource,
                &mut stack,
                &mut retained_publishers,
                &scenario.name,
                sweep_configs()[scenario.config_index],
                &scenario.output_kinds,
            )
            .await?,
        );
    }

    write_resource_sweep_csv(&env.summary_csv, &aggregates)?;
    write_branch_matrix_markdown(
        &env.summary_md,
        &env.backend,
        &resource.srt_crypto.transport_label(),
        &aggregates,
    )?;
    let result = json!({
        "mode": "branch-matrix",
        "backend": env.backend,
        "srtIngestTransport": resource.srt_crypto.transport_label(),
        "lifecycle": resource.lifecycle.as_str(),
        "artifacts": {
            "summaryJson": env.summary_json,
            "summaryCsv": env.summary_csv,
            "summaryMarkdown": env.summary_md,
            "samplesJsonl": resource.samples_jsonl,
            "restreamLog": resource.restream_log,
            "mediamtxLog": resource.mediamtx_log,
        },
        "aggregates": aggregates.iter().map(resource_aggregate_json).collect::<Vec<_>>(),
    });
    std::fs::write(
        &env.summary_json,
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .map_err(|e| e.to_string())?;

    if resource.no_cleanup {
        println!("branch-matrix no-cleanup: leaving final stack running");
    } else {
        for child in &mut retained_publishers {
            stop_child(child).await;
        }
        if let Some(stack) = stack.as_mut() {
            stop_child(&mut stack.restream).await;
            stop_child(&mut stack.mediamtx).await;
        }
    }
    Ok(result)
}

async fn bitrate_sweep() -> Result<Value, String> {
    let env = BitrateSweepEnv::from_env()?;
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&env.summary_csv);
    let _ = std::fs::remove_file(&env.summary_json);
    let _ = std::fs::remove_file(&env.samples_jsonl);

    let mut rows = Vec::new();
    for config in &env.configs {
        for bitrate in &env.bitrates {
            let row = run_bitrate_case(&env, *config, bitrate).await?;
            rows.push(row);
        }
    }

    write_bitrate_sweep_csv(&env.summary_csv, &rows)?;
    let result = json!({
        "mode": "bitrate-sweep",
        "artifacts": {
            "summaryJson": env.summary_json,
            "summaryCsv": env.summary_csv,
            "samplesJsonl": env.samples_jsonl,
            "restreamLog": env.restream_log,
            "mediamtxLog": env.mediamtx_log,
        },
        "cases": rows.iter().map(bitrate_sweep_case_json).collect::<Vec<_>>(),
    });
    std::fs::write(
        &env.summary_json,
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    Ok(result)
}

async fn run_bitrate_case(
    env: &BitrateSweepEnv,
    config: SweepConfig,
    bitrate: &BitrateSpec,
) -> Result<BitrateSweepCase, String> {
    let mut stack = start_bitrate_sweep_stack(env).await?;
    let stream_key = format!(
        "bitrate-{}-{}",
        config.name,
        bitrate.label.to_ascii_lowercase().replace('.', "_")
    );
    let pipeline_id = create_resource_pipeline(&stack.api, config.name, &stream_key).await?;
    let srt_crypto = harness_srt_crypto_from_env();
    let mut publisher = spawn_resource_publisher_with_bitrate(
        env.restream_rtmp,
        env.restream_srt,
        &env.work_dir,
        &srt_crypto,
        config,
        &stream_key,
        &bitrate.label,
    )?;
    wait_for_api_input_live(&stack.api, &pipeline_id, Duration::from_secs(45)).await?;
    let restream_rss_base_kb =
        read_proc_status_kb_checked(stack.restream_pid, "VmRSS", &env.restream_log)?;

    let mut output_ids = Vec::new();
    let mut probe_specs = Vec::new();
    for index in 1..=env.output_groups {
        let names = bitrate_case_output_names(config.name, &bitrate.label, index);
        for (kind, name, expected) in [
            (SweepOutputKind::RtmpSource, names.rtmp_source, "1920x1080"),
            (SweepOutputKind::Rtmp720p, names.rtmp_720p, "1280x720"),
            (SweepOutputKind::SrtSource, names.srt_source, "1920x1080"),
            (SweepOutputKind::Srt720p, names.srt_720p, "1280x720"),
        ] {
            let (url, encoding) = bitrate_output_url(env, config, kind, &name);
            let output_id = create_output(&stack.api, &pipeline_id, &name, &url, &encoding).await?;
            start_output(&stack.api, &pipeline_id, &output_id).await?;
            output_ids.push(output_id);
            probe_specs.push((kind, name, expected.to_string()));
        }
    }
    wait_for_outputs_progress(
        &stack.api,
        &pipeline_id,
        &output_ids,
        Duration::from_secs(45),
    )
    .await?;

    let samples = sample_bitrate_window(env, &mut stack, config, bitrate, &pipeline_id).await?;
    let mut correctness_ok = true;
    let mut correctness_failures = Vec::new();
    for (kind, name, expected) in &probe_specs {
        let url = bitrate_probe_url(env, *kind, name);
        if let Some(observed) =
            check_bitrate_stream(name, &url, expected, Duration::from_secs(20)).await?
        {
            correctness_ok = false;
            correctness_failures.push(format!("{name}: expected {expected}, observed {observed}"));
        }
    }

    let restream_rss_final_kb =
        read_proc_status_kb_checked(stack.restream_pid, "VmRSS", &env.restream_log).unwrap_or(0);
    let ffmpeg = ffmpeg_children_stats(stack.restream_pid)?;

    stop_child(&mut publisher).await;
    delete_resource_pipeline(&stack.api, &pipeline_id).await;
    if !env.no_cleanup {
        stop_child(&mut stack.restream).await;
        stop_child(&mut stack.mediamtx).await;
    }

    summarize_bitrate_case(
        config,
        bitrate,
        env.output_groups,
        restream_rss_base_kb,
        restream_rss_final_kb,
        ffmpeg,
        correctness_ok,
        correctness_failures,
        &samples,
    )
}

async fn start_bitrate_sweep_stack(env: &BitrateSweepEnv) -> Result<ResourceSweepStack, String> {
    if !env.restream_bin.exists() {
        return Err(format!(
            "restream binary not found at {}",
            env.restream_bin.display()
        ));
    }
    std::fs::create_dir_all(env.work_dir.join("logs")).map_err(|e| e.to_string())?;
    cleanup_ramp_db(&env.restream_db_path);
    let mediamtx_log = std::fs::File::create(&env.mediamtx_log).map_err(|e| e.to_string())?;
    let mediamtx_err = mediamtx_log.try_clone().map_err(|e| e.to_string())?;
    std::fs::write(
        &env.mediamtx_config,
        format!(
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nrtmp: yes\nrtmpAddress: :{}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{}\nhls: no\nwebrtc: no\napi: yes\napiAddress: :{}\nmetrics: no\npaths:\n  all:\n",
            env.mtx_rtmp, env.mtx_srt, env.mtx_api
        ),
    )
    .map_err(|e| e.to_string())?;
    let mut mediamtx = Command::new("mediamtx")
        .arg(&env.mediamtx_config)
        .stdout(Stdio::from(mediamtx_log))
        .stderr(Stdio::from(mediamtx_err))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/v3/paths/list", env.mtx_api),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut mediamtx).await;
        return Err(format!("mediamtx did not become ready: {err}"));
    }

    let restream_log = std::fs::File::create(&env.restream_log).map_err(|e| e.to_string())?;
    let restream_err = restream_log.try_clone().map_err(|e| e.to_string())?;
    let mut restream_cmd = Command::new(&env.restream_bin);
    restream_cmd
        .env("RESTREAM_HTTP_PORT", env.restream_http.to_string())
        .env("RESTREAM_RTMP_PORT", env.restream_rtmp.to_string())
        .env("RESTREAM_SRT_PORT", env.restream_srt.to_string())
        .env("RESTREAM_LOG_DIR", env.work_dir.join("logs"))
        .env(
            "RESTREAM_DB_PATH",
            env.restream_db_path.to_string_lossy().to_string(),
        )
        .stdout(Stdio::from(restream_log))
        .stderr(Stdio::from(restream_err))
        .kill_on_drop(true);
    apply_harness_srt_listener_env(&mut restream_cmd);
    let mut restream = restream_cmd.spawn().map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/healthz", env.restream_http),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut restream).await;
        stop_child(&mut mediamtx).await;
        return Err(format!("restream did not become ready: {err}"));
    }
    let mut api = RampApi::new(env.restream_http);
    api.login().await?;
    let restream_pid = restream.id().ok_or("restream pid missing")?;
    Ok(ResourceSweepStack {
        mediamtx,
        restream,
        api,
        restream_pid,
    })
}

/// Output names allocated for one bitrate-sweep case.
struct BitrateOutputNames {
    rtmp_source: String,
    rtmp_720p: String,
    srt_source: String,
    srt_720p: String,
}

fn bitrate_case_output_names(
    config_name: &str,
    bitrate_label: &str,
    index: usize,
) -> BitrateOutputNames {
    let suffix = bitrate_label.to_ascii_lowercase().replace('.', "_");
    BitrateOutputNames {
        rtmp_source: format!("{config_name}-{suffix}-rtmp-src-{index}"),
        rtmp_720p: format!("{config_name}-{suffix}-rtmp-720p-{index}"),
        srt_source: format!("{config_name}-{suffix}-srt-src-{index}"),
        srt_720p: format!("{config_name}-{suffix}-srt-720p-{index}"),
    }
}

fn bitrate_output_url(
    env: &BitrateSweepEnv,
    config: SweepConfig,
    kind: SweepOutputKind,
    name: &str,
) -> (String, String) {
    (
        kind.publish_url(env.mtx_rtmp, env.mtx_srt, name),
        kind.encoding(config.multi_audio).to_string(),
    )
}

fn bitrate_probe_url(env: &BitrateSweepEnv, kind: SweepOutputKind, name: &str) -> String {
    kind.read_url(env.mtx_rtmp, env.mtx_srt, name)
}

async fn sample_bitrate_window(
    env: &BitrateSweepEnv,
    stack: &mut ResourceSweepStack,
    config: SweepConfig,
    bitrate: &BitrateSpec,
    pipeline_id: &str,
) -> Result<Vec<BitrateSweepSample>, String> {
    let mut samples = Vec::new();
    let mut prev_ticks = read_proc_stat_ticks(stack.restream_pid)?;
    let mut prev_ffmpeg_ticks: HashMap<u32, u64> = HashMap::new();
    let mut prev_instant = Instant::now();
    let mut elapsed_secs = 0u64;
    let deadline = Instant::now() + Duration::from_secs(env.stabilize_secs);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(env.sample_interval_secs)).await;
        elapsed_secs += env.sample_interval_secs;
        let ffmpeg = ffmpeg_children_stats(stack.restream_pid)?;
        let ticks = read_proc_stat_ticks(stack.restream_pid)?;
        let interval_secs = prev_instant.elapsed().as_secs_f64().max(0.001);
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) as f64 };
        let restream_cpu_pct =
            100.0 * (ticks.saturating_sub(prev_ticks)) as f64 / clk_tck / interval_secs;
        let mut ffmpeg_delta_ticks = 0u64;
        let mut next_ffmpeg_ticks = HashMap::new();
        for pid in &ffmpeg.pids {
            if let Ok(current_ticks) = read_proc_stat_ticks(*pid) {
                let previous_ticks = prev_ffmpeg_ticks.get(pid).copied().unwrap_or(current_ticks);
                ffmpeg_delta_ticks += current_ticks.saturating_sub(previous_ticks);
                next_ffmpeg_ticks.insert(*pid, current_ticks);
            }
        }
        let ffmpeg_cpu_pct = 100.0 * ffmpeg_delta_ticks as f64 / clk_tck / interval_secs;
        let total_cpu_pct = restream_cpu_pct + ffmpeg_cpu_pct;
        prev_ticks = ticks;
        prev_ffmpeg_ticks = next_ffmpeg_ticks;
        prev_instant = Instant::now();

        let telemetry = stack.api.get_json("/api/v1/engine/telemetry").await?;
        let pipeline_telemetry = stack
            .api
            .get_json(&format!("/api/v1/pipelines/{pipeline_id}/telemetry"))
            .await?;
        let accounting = &telemetry["memoryAccounting"];
        let avio = &accounting["avioQueues"];
        let overflow_count = pipeline_telemetry["sourceRing"]["readers"]
            .as_array()
            .map(|readers| {
                readers
                    .iter()
                    .map(|reader| reader["overflowCount"].as_u64().unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);
        let sample = BitrateSweepSample {
            config: config.name.to_string(),
            bitrate_label: bitrate.label.clone(),
            bitrate_mbps: bitrate.mbps,
            elapsed_secs,
            restream_cpu_pct,
            ffmpeg_cpu_pct,
            total_cpu_pct,
            restream_rss_kb: read_proc_status_kb_checked(
                stack.restream_pid,
                "VmRSS",
                &env.restream_log,
            )?,
            ffmpeg_count: ffmpeg.count,
            ffmpeg_rss_kb: ffmpeg.rss_kb,
            total_rss_kb: read_proc_status_kb_checked(
                stack.restream_pid,
                "VmRSS",
                &env.restream_log,
            )? + ffmpeg.rss_kb,
            retained_payload_kb: accounting["retainedPayloadBytes"].as_u64().unwrap_or(0) / 1024,
            source_ring_kb: accounting["sourceRings"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap_or(0))
                .sum::<u64>()
                / 1024,
            transcoder_ring_kb: accounting["transcoderRings"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap_or(0))
                .sum::<u64>()
                / 1024,
            tsmux_ring_kb: accounting["tsMuxerRings"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap_or(0))
                .sum::<u64>()
                / 1024,
            avio_len_kb: avio["totalLenBytes"].as_u64().unwrap_or(0) / 1024,
            avio_hwm_kb: avio["inputQueues"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(avio["egressQueues"].as_array().into_iter().flatten())
                .map(|queue| queue["highWaterBytes"].as_u64().unwrap_or(0))
                .sum::<u64>()
                / 1024,
            overflow_count,
        };
        append_line(
            &env.samples_jsonl,
            &format!(
                "{}\n",
                serde_json::to_string(&bitrate_sweep_sample_json(&sample)).unwrap()
            ),
        )?;
        samples.push(sample);
    }
    Ok(samples)
}

async fn check_bitrate_stream(
    label: &str,
    url: &str,
    expected: &str,
    timeout: Duration,
) -> Result<Option<String>, String> {
    let deadline = Instant::now() + timeout;
    let mut last_observed = None;
    let mut last_error = None;
    while Instant::now() < deadline {
        match probe_dims_ramp(url).await {
            Ok(dimensions) if dimensions == expected => return Ok(None),
            Ok(dimensions) if !dimensions.is_empty() => last_observed = Some(dimensions),
            Ok(_) => {}
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let observed = last_observed
        .or(last_error)
        .unwrap_or_else(|| "none".to_string());
    println!("[bitrate-sweep] probe mismatch {label}: expected {expected}, observed {observed}");
    Ok(Some(observed))
}

#[allow(clippy::too_many_arguments)]
fn summarize_bitrate_case(
    config: SweepConfig,
    bitrate: &BitrateSpec,
    output_groups: usize,
    restream_rss_base_kb: u64,
    restream_rss_final_kb: u64,
    ffmpeg: FfmpegStats,
    correctness_ok: bool,
    correctness_failures: Vec<String>,
    samples: &[BitrateSweepSample],
) -> Result<BitrateSweepCase, String> {
    if samples.is_empty() {
        return Err("bitrate sweep produced no samples".to_string());
    }
    let retained_min_kb = samples
        .iter()
        .map(|sample| sample.retained_payload_kb)
        .min()
        .unwrap_or(0);
    let retained_max_kb = samples
        .iter()
        .map(|sample| sample.retained_payload_kb)
        .max()
        .unwrap_or(0);
    let retained_final_kb = samples
        .last()
        .map(|sample| sample.retained_payload_kb)
        .unwrap_or(0);
    let elapsed_min = (samples
        .last()
        .map(|sample| sample.elapsed_secs)
        .unwrap_or(0) as f64)
        / 60.0;
    Ok(BitrateSweepCase {
        config: config.name.to_string(),
        ingest_proto: config.ingest_proto.to_string(),
        video_codec: config.video_codec.to_string(),
        multi_audio: config.multi_audio,
        bitrate_label: bitrate.label.clone(),
        bitrate_mbps: bitrate.mbps,
        output_groups,
        outputs_total: output_groups * 4,
        restream_rss_base_kb,
        restream_rss_final_kb,
        restream_rss_delta_kb: restream_rss_final_kb.saturating_sub(restream_rss_base_kb),
        restream_rss_peak_kb: samples
            .iter()
            .map(|sample| sample.restream_rss_kb)
            .max()
            .unwrap_or(0),
        ffmpeg_count_peak: samples
            .iter()
            .map(|sample| sample.ffmpeg_count)
            .max()
            .unwrap_or(ffmpeg.count),
        ffmpeg_rss_peak_kb: samples
            .iter()
            .map(|sample| sample.ffmpeg_rss_kb)
            .max()
            .unwrap_or(ffmpeg.rss_kb),
        total_rss_peak_kb: samples
            .iter()
            .map(|sample| sample.total_rss_kb)
            .max()
            .unwrap_or(restream_rss_final_kb + ffmpeg.rss_kb),
        restream_cpu_avg_pct: round2(
            samples
                .iter()
                .map(|sample| sample.restream_cpu_pct)
                .sum::<f64>()
                / samples.len() as f64,
        ),
        restream_cpu_peak_pct: round2(
            samples
                .iter()
                .map(|sample| sample.restream_cpu_pct)
                .fold(0.0, f64::max),
        ),
        ffmpeg_cpu_avg_pct: round2(
            samples
                .iter()
                .map(|sample| sample.ffmpeg_cpu_pct)
                .sum::<f64>()
                / samples.len() as f64,
        ),
        ffmpeg_cpu_peak_pct: round2(
            samples
                .iter()
                .map(|sample| sample.ffmpeg_cpu_pct)
                .fold(0.0, f64::max),
        ),
        total_cpu_avg_pct: round2(
            samples
                .iter()
                .map(|sample| sample.total_cpu_pct)
                .sum::<f64>()
                / samples.len() as f64,
        ),
        total_cpu_peak_pct: round2(
            samples
                .iter()
                .map(|sample| sample.total_cpu_pct)
                .fold(0.0, f64::max),
        ),
        retained_payload_min_kb: retained_min_kb,
        retained_payload_max_kb: retained_max_kb,
        retained_payload_final_kb: retained_final_kb,
        retained_growth_kb_per_min: if elapsed_min > 0.0 {
            round2((retained_final_kb.saturating_sub(retained_min_kb)) as f64 / elapsed_min)
        } else {
            0.0
        },
        source_ring_peak_kb: samples
            .iter()
            .map(|sample| sample.source_ring_kb)
            .max()
            .unwrap_or(0),
        transcoder_ring_peak_kb: samples
            .iter()
            .map(|sample| sample.transcoder_ring_kb)
            .max()
            .unwrap_or(0),
        tsmux_ring_peak_kb: samples
            .iter()
            .map(|sample| sample.tsmux_ring_kb)
            .max()
            .unwrap_or(0),
        avio_len_peak_kb: samples
            .iter()
            .map(|sample| sample.avio_len_kb)
            .max()
            .unwrap_or(0),
        avio_hwm_peak_kb: samples
            .iter()
            .map(|sample| sample.avio_hwm_kb)
            .max()
            .unwrap_or(0),
        overflow_count_final: samples
            .last()
            .map(|sample| sample.overflow_count)
            .unwrap_or(0),
        correctness_ok,
        correctness_failures,
    })
}

fn bitrate_sweep_sample_json(sample: &BitrateSweepSample) -> Value {
    json!({
        "config": sample.config,
        "bitrateLabel": sample.bitrate_label,
        "bitrateMbps": sample.bitrate_mbps,
        "elapsedSecs": sample.elapsed_secs,
        "restreamCpuPct": sample.restream_cpu_pct,
        "ffmpegCpuPct": sample.ffmpeg_cpu_pct,
        "totalCpuPct": sample.total_cpu_pct,
        "restreamRssKb": sample.restream_rss_kb,
        "ffmpegCount": sample.ffmpeg_count,
        "ffmpegRssKb": sample.ffmpeg_rss_kb,
        "totalRssKb": sample.total_rss_kb,
        "retainedPayloadKb": sample.retained_payload_kb,
        "sourceRingKb": sample.source_ring_kb,
        "transcoderRingKb": sample.transcoder_ring_kb,
        "tsmuxRingKb": sample.tsmux_ring_kb,
        "avioLenKb": sample.avio_len_kb,
        "avioHwmKb": sample.avio_hwm_kb,
        "overflowCount": sample.overflow_count,
    })
}

fn bitrate_sweep_case_json(case: &BitrateSweepCase) -> Value {
    json!({
        "config": case.config,
        "ingestProto": case.ingest_proto,
        "videoCodec": case.video_codec,
        "multiAudio": case.multi_audio,
        "bitrateLabel": case.bitrate_label,
        "bitrateMbps": case.bitrate_mbps,
        "outputGroups": case.output_groups,
        "outputsTotal": case.outputs_total,
        "restreamRssBaseKb": case.restream_rss_base_kb,
        "restreamRssFinalKb": case.restream_rss_final_kb,
        "restreamRssDeltaKb": case.restream_rss_delta_kb,
        "restreamRssPeakKb": case.restream_rss_peak_kb,
        "ffmpegCountPeak": case.ffmpeg_count_peak,
        "ffmpegRssPeakKb": case.ffmpeg_rss_peak_kb,
        "totalRssPeakKb": case.total_rss_peak_kb,
        "restreamCpuAvgPct": case.restream_cpu_avg_pct,
        "restreamCpuPeakPct": case.restream_cpu_peak_pct,
        "ffmpegCpuAvgPct": case.ffmpeg_cpu_avg_pct,
        "ffmpegCpuPeakPct": case.ffmpeg_cpu_peak_pct,
        "totalCpuAvgPct": case.total_cpu_avg_pct,
        "totalCpuPeakPct": case.total_cpu_peak_pct,
        "retainedPayloadMinKb": case.retained_payload_min_kb,
        "retainedPayloadMaxKb": case.retained_payload_max_kb,
        "retainedPayloadFinalKb": case.retained_payload_final_kb,
        "retainedGrowthKbPerMin": case.retained_growth_kb_per_min,
        "sourceRingPeakKb": case.source_ring_peak_kb,
        "transcoderRingPeakKb": case.transcoder_ring_peak_kb,
        "tsmuxRingPeakKb": case.tsmux_ring_peak_kb,
        "avioLenPeakKb": case.avio_len_peak_kb,
        "avioHwmPeakKb": case.avio_hwm_peak_kb,
        "overflowCountFinal": case.overflow_count_final,
        "correctnessOk": case.correctness_ok,
        "correctnessFailures": case.correctness_failures,
    })
}

fn write_bitrate_sweep_csv(path: &Path, rows: &[BitrateSweepCase]) -> Result<(), String> {
    let mut text = String::from(
        "config,ingest_proto,video_codec,multi_audio,bitrate_label,bitrate_mbps,output_groups,outputs_total,restream_rss_base_kb,restream_rss_final_kb,restream_rss_delta_kb,restream_rss_peak_kb,ffmpeg_count_peak,ffmpeg_rss_peak_kb,total_rss_peak_kb,restream_cpu_avg_pct,restream_cpu_peak_pct,ffmpeg_cpu_avg_pct,ffmpeg_cpu_peak_pct,total_cpu_avg_pct,total_cpu_peak_pct,retained_payload_min_kb,retained_payload_max_kb,retained_payload_final_kb,retained_growth_kb_per_min,source_ring_peak_kb,transcoder_ring_peak_kb,tsmux_ring_peak_kb,avio_len_peak_kb,avio_hwm_peak_kb,overflow_count_final,correctness_ok\n",
    );
    for row in rows {
        text.push_str(&format!(
            "{},{},{},{},{},{:.2},{},{},{},{},{},{},{},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{},{},{},{:.2},{},{},{},{},{},{},{}\n",
            csv_escape(&row.config),
            csv_escape(&row.ingest_proto),
            csv_escape(&row.video_codec),
            row.multi_audio,
            csv_escape(&row.bitrate_label),
            row.bitrate_mbps,
            row.output_groups,
            row.outputs_total,
            row.restream_rss_base_kb,
            row.restream_rss_final_kb,
            row.restream_rss_delta_kb,
            row.restream_rss_peak_kb,
            row.ffmpeg_count_peak,
            row.ffmpeg_rss_peak_kb,
            row.total_rss_peak_kb,
            row.restream_cpu_avg_pct,
            row.restream_cpu_peak_pct,
            row.ffmpeg_cpu_avg_pct,
            row.ffmpeg_cpu_peak_pct,
            row.total_cpu_avg_pct,
            row.total_cpu_peak_pct,
            row.retained_payload_min_kb,
            row.retained_payload_max_kb,
            row.retained_payload_final_kb,
            row.retained_growth_kb_per_min,
            row.source_ring_peak_kb,
            row.transcoder_ring_peak_kb,
            row.tsmux_ring_peak_kb,
            row.avio_len_peak_kb,
            row.avio_hwm_peak_kb,
            row.overflow_count_final,
            row.correctness_ok,
        ));
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

async fn start_resource_sweep_stack(env: &ResourceSweepEnv) -> Result<ResourceSweepStack, String> {
    if !env.restream_bin.exists() {
        return Err(format!(
            "restream binary not found at {}",
            env.restream_bin.display()
        ));
    }
    std::fs::create_dir_all(env.work_dir.join("logs")).map_err(|e| e.to_string())?;
    cleanup_ramp_db(&env.restream_db_path);
    let mediamtx_log = std::fs::File::create(&env.mediamtx_log).map_err(|e| e.to_string())?;
    let mediamtx_err = mediamtx_log.try_clone().map_err(|e| e.to_string())?;
    std::fs::write(
        &env.mediamtx_config,
        format!(
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nrtmp: yes\nrtmpAddress: :{}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{}\nhls: no\nwebrtc: no\napi: yes\napiAddress: :{}\nmetrics: no\npaths:\n  all:\n",
            env.mtx_rtmp, env.mtx_srt, env.mtx_api
        ),
    )
    .map_err(|e| e.to_string())?;
    let mut mediamtx = Command::new("mediamtx")
        .arg(&env.mediamtx_config)
        .stdout(Stdio::from(mediamtx_log))
        .stderr(Stdio::from(mediamtx_err))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/v3/paths/list", env.mtx_api),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut mediamtx).await;
        return Err(format!("mediamtx did not become ready: {err}"));
    }

    let restream_log = std::fs::File::create(&env.restream_log).map_err(|e| e.to_string())?;
    let restream_err = restream_log.try_clone().map_err(|e| e.to_string())?;
    let mut restream_cmd = Command::new(&env.restream_bin);
    restream_cmd
        .env("RESTREAM_HTTP_PORT", env.restream_http.to_string())
        .env("RESTREAM_RTMP_PORT", env.restream_rtmp.to_string())
        .env("RESTREAM_SRT_PORT", env.restream_srt.to_string())
        .env("RESTREAM_LOG_DIR", env.work_dir.join("logs"))
        .env(
            "RESTREAM_DB_PATH",
            env.restream_db_path.to_string_lossy().to_string(),
        )
        .stdout(Stdio::from(restream_log))
        .stderr(Stdio::from(restream_err))
        .kill_on_drop(true);
    apply_srt_listener_env(&mut restream_cmd, &env.srt_crypto);
    let mut restream = restream_cmd.spawn().map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/healthz", env.restream_http),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut restream).await;
        stop_child(&mut mediamtx).await;
        return Err(format!("restream did not become ready: {err}"));
    }
    let mut api = RampApi::new(env.restream_http);
    api.login().await?;
    let restream_pid = restream.id().ok_or("restream pid missing")?;
    Ok(ResourceSweepStack {
        mediamtx,
        restream,
        api,
        restream_pid,
    })
}

async fn ensure_resource_stack<'a>(
    env: &ResourceSweepEnv,
    stack: &'a mut Option<ResourceSweepStack>,
) -> Result<&'a mut ResourceSweepStack, String> {
    if stack.is_none() {
        *stack = Some(start_resource_sweep_stack(env).await?);
    }
    stack
        .as_mut()
        .ok_or("resource sweep stack missing".to_string())
}

async fn run_resource_baseline(
    env: &ResourceSweepEnv,
    stack: &mut Option<ResourceSweepStack>,
    retained_publishers: &mut Vec<Child>,
) -> Result<ResourceAggregate, String> {
    let local_only = env.lifecycle == ResourceSweepLifecycle::Isolated;
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack).await?
    };
    let meta = ResourceScenarioMeta {
        scenario: "baseline-empty",
        label: "empty".to_string(),
        pipelines: 0,
        outputs: 0,
        ingest_types: "none".to_string(),
        egress_mix: "none".to_string(),
        transcode: "none",
    };
    let aggregate = sample_resource_window(env, active, meta).await?;
    if local_only {
        stop_child(&mut local_stack.as_mut().unwrap().restream).await;
        stop_child(&mut local_stack.as_mut().unwrap().mediamtx).await;
    }
    let _ = retained_publishers;
    Ok(aggregate)
}

async fn run_resource_ingest_only(
    env: &ResourceSweepEnv,
    stack: &mut Option<ResourceSweepStack>,
    retained_publishers: &mut Vec<Child>,
    config: SweepConfig,
) -> Result<ResourceAggregate, String> {
    let local_only = env.lifecycle == ResourceSweepLifecycle::Isolated;
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack).await?
    };
    let stream_key = format!("resource-{}", config.name);
    let pipeline_id = create_resource_pipeline(&active.api, config.name, &stream_key).await?;
    let mut publisher = spawn_resource_publisher(env, config, &stream_key)?;
    wait_for_api_input_live(&active.api, &pipeline_id, Duration::from_secs(45)).await?;
    let meta = ResourceScenarioMeta {
        scenario: "ingest-only",
        label: config.name.to_string(),
        pipelines: 1,
        outputs: 0,
        ingest_types: config.name.to_string(),
        egress_mix: "none".to_string(),
        transcode: "none",
    };
    let aggregate = sample_resource_window(env, active, meta).await?;
    if env.lifecycle == ResourceSweepLifecycle::Cumulative {
        retained_publishers.push(publisher);
    } else {
        stop_child(&mut publisher).await;
        delete_resource_pipeline(&active.api, &pipeline_id).await;
    }
    if local_only {
        stop_child(&mut local_stack.as_mut().unwrap().restream).await;
        stop_child(&mut local_stack.as_mut().unwrap().mediamtx).await;
    }
    Ok(aggregate)
}

async fn run_resource_ingest_growth(
    env: &ResourceSweepEnv,
    stack: &mut Option<ResourceSweepStack>,
    retained_publishers: &mut Vec<Child>,
    mixed: bool,
) -> Result<Vec<ResourceAggregate>, String> {
    let local_only = env.lifecycle == ResourceSweepLifecycle::Isolated;
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack).await?
    };

    let mut publishers = Vec::new();
    let mut pipeline_ids = Vec::new();
    let max_ingests = *env.ingest_counts.iter().max().unwrap_or(&1);
    let mut out = Vec::new();
    for index in 1..=max_ingests {
        let config = if mixed {
            sweep_configs()[index - 1]
        } else {
            sweep_configs()[1]
        };
        let stream_key = format!("resource-growth-{index}-{}", config.name);
        let pipeline_id = create_resource_pipeline(
            &active.api,
            &format!("{}-{index}", config.name),
            &stream_key,
        )
        .await?;
        let publisher = spawn_resource_publisher(env, config, &stream_key)?;
        wait_for_api_input_live(&active.api, &pipeline_id, Duration::from_secs(45)).await?;
        publishers.push(publisher);
        pipeline_ids.push(pipeline_id);
        if env.ingest_counts.contains(&index) {
            let ingest_types = if mixed {
                sweep_configs()
                    .iter()
                    .take(index)
                    .map(|cfg| cfg.name)
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                "h264-srt".to_string()
            };
            out.push(
                sample_resource_window(
                    env,
                    active,
                    ResourceScenarioMeta {
                        scenario: if mixed {
                            "ingest-growth-mixed"
                        } else {
                            "ingest-growth-same"
                        },
                        label: format!("{index}-pipelines"),
                        pipelines: index,
                        outputs: 0,
                        ingest_types,
                        egress_mix: "none".to_string(),
                        transcode: "none",
                    },
                )
                .await?,
            );
        }
    }
    if env.lifecycle == ResourceSweepLifecycle::Cumulative {
        retained_publishers.extend(publishers);
    } else {
        for child in &mut publishers {
            stop_child(child).await;
        }
        for pipeline_id in pipeline_ids {
            delete_resource_pipeline(&active.api, &pipeline_id).await;
        }
    }
    if local_only {
        stop_child(&mut local_stack.as_mut().unwrap().restream).await;
        stop_child(&mut local_stack.as_mut().unwrap().mediamtx).await;
    }
    Ok(out)
}

async fn run_resource_egress_growth(
    env: &ResourceSweepEnv,
    stack: &mut Option<ResourceSweepStack>,
    retained_publishers: &mut Vec<Child>,
    scenario_name: &str,
    config: SweepConfig,
    output_kinds: &[SweepOutputKind],
) -> Result<Vec<ResourceAggregate>, String> {
    let local_only = env.lifecycle == ResourceSweepLifecycle::Isolated;
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack).await?
    };
    let stream_key = format!("resource-{scenario_name}");
    let pipeline_id = create_resource_pipeline(&active.api, scenario_name, &stream_key).await?;
    let mut publisher = spawn_resource_publisher(env, config, &stream_key)?;
    wait_for_api_input_live(&active.api, &pipeline_id, Duration::from_secs(45)).await?;
    let mut output_ids = Vec::new();
    let max_outputs = *env.egress_counts.iter().max().unwrap_or(&1);
    let mut out = Vec::new();
    for index in 1..=max_outputs {
        for kind in output_kinds {
            let name = format!("{scenario_name}-{}-{index}", kind.label());
            let (url, encoding) = resource_output_url(env, config, *kind, &name);
            let output_id =
                create_output(&active.api, &pipeline_id, &name, &url, &encoding).await?;
            start_output(&active.api, &pipeline_id, &output_id).await?;
            output_ids.push(output_id);
        }
        if env.egress_counts.contains(&index) {
            let progress_timeout = resource_output_progress_timeout(output_ids.len());
            wait_for_outputs_progress(&active.api, &pipeline_id, &output_ids, progress_timeout)
                .await?;
            out.push(
                sample_resource_window(
                    env,
                    active,
                    ResourceScenarioMeta {
                        scenario: scenario_name,
                        label: format!("{index}-per-group"),
                        pipelines: 1,
                        outputs: output_ids.len(),
                        ingest_types: config.name.to_string(),
                        egress_mix: output_kinds
                            .iter()
                            .map(|kind| kind.label())
                            .collect::<Vec<_>>()
                            .join(","),
                        transcode: if output_kinds.iter().any(|kind| {
                            matches!(
                                kind,
                                SweepOutputKind::Rtmp720p
                                    | SweepOutputKind::Srt720p
                                    | SweepOutputKind::Rtmp1080p
                                    | SweepOutputKind::Srt1080p
                            )
                        }) {
                            "yes"
                        } else {
                            "no"
                        },
                    },
                )
                .await?,
            );
        }
    }
    if env.lifecycle == ResourceSweepLifecycle::Cumulative {
        retained_publishers.push(publisher);
    } else {
        stop_child(&mut publisher).await;
        delete_resource_pipeline(&active.api, &pipeline_id).await;
    }
    if local_only {
        stop_child(&mut local_stack.as_mut().unwrap().restream).await;
        stop_child(&mut local_stack.as_mut().unwrap().mediamtx).await;
    }
    Ok(out)
}

async fn create_resource_pipeline(
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
        .ok_or("pipeline create response missing pipeline.id".to_string())
}

async fn delete_resource_pipeline(api: &RampApi, pipeline_id: &str) {
    let _ = api
        .delete_json(&format!("/api/v1/pipelines/{pipeline_id}"))
        .await;
}

fn spawn_resource_publisher(
    env: &ResourceSweepEnv,
    config: SweepConfig,
    stream_key: &str,
) -> Result<Child, String> {
    spawn_resource_publisher_with_bitrate(
        env.restream_rtmp,
        env.restream_srt,
        &env.work_dir,
        &env.srt_crypto,
        config,
        stream_key,
        "1.5M",
    )
}

fn spawn_resource_publisher_with_bitrate(
    restream_rtmp: u16,
    restream_srt: u16,
    work_dir: &Path,
    srt_crypto: &HarnessSrtCrypto,
    config: SweepConfig,
    stream_key: &str,
    bitrate: &str,
) -> Result<Child, String> {
    let log_path = work_dir.join(format!("publisher-{stream_key}.log"));
    let fixture = sweep_fixture(config, bitrate)?;
    let (url, format, selection) = if config.ingest_proto == "rtmp" {
        (
            format!("rtmp://127.0.0.1:{restream_rtmp}/live/{stream_key}"),
            "flv",
            PublishTrackSelection::PrimaryAv,
        )
    } else {
        (
            append_srt_crypto(
                format!(
                    "srt://127.0.0.1:{restream_srt}?streamid=publish:live/{stream_key}&latency=200000"
                ),
                srt_crypto,
            ),
            "mpegts",
            if config.multi_audio {
                PublishTrackSelection::AllStreams
            } else {
                PublishTrackSelection::PrimaryAv
            },
        )
    };
    spawn_publisher_with_selection(&fixture, &url, format, selection, Some(&log_path))
}

fn resource_output_url(
    env: &ResourceSweepEnv,
    config: SweepConfig,
    kind: SweepOutputKind,
    name: &str,
) -> (String, String) {
    (
        kind.publish_url(env.mtx_rtmp, env.mtx_srt, name),
        kind.encoding(config.multi_audio).to_string(),
    )
}

fn resource_output_progress_timeout(output_count: usize) -> Duration {
    let base_secs = env_secs("RESOURCE_SWEEP_PROGRESS_TIMEOUT_BASE_SECS", 30);
    let per_output_secs = env_secs("RESOURCE_SWEEP_PROGRESS_TIMEOUT_PER_OUTPUT_SECS", 4);
    let cap_secs = env_secs("RESOURCE_SWEEP_PROGRESS_TIMEOUT_CAP_SECS", 240);
    scaled_output_progress_timeout(output_count, base_secs, per_output_secs, cap_secs)
}

fn scaled_output_progress_timeout(
    output_count: usize,
    base_secs: u64,
    per_output_secs: u64,
    cap_secs: u64,
) -> Duration {
    let cap_secs = cap_secs.max(base_secs);
    let extra_outputs = output_count.saturating_sub(1) as u64;
    let scaled_secs = base_secs.saturating_add(extra_outputs.saturating_mul(per_output_secs));
    Duration::from_secs(scaled_secs.min(cap_secs))
}

async fn wait_for_outputs_progress(
    api: &RampApi,
    pipeline_id: &str,
    output_ids: &[String],
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let health = api.get_json("/api/v1/engine/health").await?;
        let mut progressed = 0usize;
        let mut stalled = Vec::new();
        for output_id in output_ids {
            let entry = &health["pipelines"][pipeline_id]["outputs"][output_id];
            let bytes_out = entry["bytesOut"].as_u64().unwrap_or(0);
            let metrics_bytes = entry["metrics"]["bytesOut"].as_u64().unwrap_or(0);
            let packets_out = entry["metrics"]["packetsOut"].as_u64().unwrap_or(0);
            if bytes_out > 0 || metrics_bytes > 0 || packets_out > 0 {
                progressed += 1;
            } else {
                let name = entry["outputName"].as_str().unwrap_or("unknown");
                let encoding = entry["encoding"].as_str().unwrap_or("unknown");
                let url = entry["targetUrl"].as_str().unwrap_or("unknown");
                let phase = entry["phase"].as_str().unwrap_or("unknown");
                let terminal_stage = entry["terminalStage"].as_str().unwrap_or("none");
                let blocked_by_stage = entry["blockedBy"]["stage"].as_str().unwrap_or("none");
                let blocked_by_phase = entry["blockedBy"]["phase"].as_str().unwrap_or("none");
                let backend = entry["blockedBy"]["backend"].as_str().unwrap_or("none");
                let wait_ms = entry["blockedBy"]["capacityWaitMs"]
                    .as_u64()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "none".to_string());
                let last_error = entry["lastError"].as_str().unwrap_or("");
                stalled.push(format!(
                    "{} output_{} encoding={} url={}\n  phase={}\n  terminalStage={}\n  blockedBy={}\n  blockedByPhase={}\n  backend={} waitMs={}\n  lastError={}",
                    name, output_id, encoding, url, phase, terminal_stage, blocked_by_stage, blocked_by_phase, backend, wait_ms, last_error
                ));
            }
        }
        if progressed == output_ids.len() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "outputs did not make progress for pipeline {pipeline_id} within {:?}: {progressed}/{}; stalled={}",
                timeout,
                output_ids.len(),
                stalled.join(", ")
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn sample_resource_window(
    env: &ResourceSweepEnv,
    stack: &mut ResourceSweepStack,
    meta: ResourceScenarioMeta<'_>,
) -> Result<ResourceAggregate, String> {
    tokio::time::sleep(Duration::from_secs(env.settle_secs)).await;
    let mut samples = Vec::new();
    let mut prev_ticks = read_proc_stat_ticks(stack.restream_pid)?;
    let mut prev_ffmpeg_ticks: HashMap<u32, u64> = HashMap::new();
    let mut prev_instant = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(env.sample_secs);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(env.sample_interval_ms)).await;
        let now = Instant::now();
        let ticks = read_proc_stat_ticks(stack.restream_pid)?;
        let ffmpeg = ffmpeg_children_stats(stack.restream_pid)?;
        let interval_secs = prev_instant.elapsed().as_secs_f64().max(0.001);
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) as f64 };
        let restream_cpu_pct =
            100.0 * (ticks.saturating_sub(prev_ticks)) as f64 / clk_tck / interval_secs;
        let mut ffmpeg_delta_ticks = 0u64;
        let mut next_ffmpeg_ticks = HashMap::new();
        for pid in &ffmpeg.pids {
            if let Ok(current_ticks) = read_proc_stat_ticks(*pid) {
                let previous_ticks = prev_ffmpeg_ticks.get(pid).copied().unwrap_or(current_ticks);
                ffmpeg_delta_ticks += current_ticks.saturating_sub(previous_ticks);
                next_ffmpeg_ticks.insert(*pid, current_ticks);
            }
        }
        let ffmpeg_cpu_pct = 100.0 * ffmpeg_delta_ticks as f64 / clk_tck / interval_secs;
        let total_cpu_pct = restream_cpu_pct + ffmpeg_cpu_pct;
        prev_ticks = ticks;
        prev_ffmpeg_ticks = next_ffmpeg_ticks;
        prev_instant = now;
        let rss_kb = read_proc_status_kb_checked(stack.restream_pid, "VmRSS", &env.restream_log)?;
        let rollup = read_smaps_rollup(stack.restream_pid)?;
        let telemetry = stack.api.get_json("/api/v1/engine/telemetry").await?;
        let health = stack.api.get_json("/api/v1/engine/health").await?;
        let accounting = &telemetry["memoryAccounting"];
        let retained_kb = accounting["retainedPayloadBytes"].as_u64().unwrap_or(0) / 1024;
        let source_ring_kb = accounting["sourceRings"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap_or(0))
            .sum::<u64>()
            / 1024;
        let transcoder_ring_kb = accounting["transcoderRings"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap_or(0))
            .sum::<u64>()
            / 1024;
        let tsmux_ring_kb = accounting["tsMuxerRings"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap_or(0))
            .sum::<u64>()
            / 1024;
        let avio_queues = &accounting["avioQueues"];
        let avio_len_kb = avio_queues["totalLenBytes"].as_u64().unwrap_or(0) / 1024;
        let avio_hwm_kb = avio_queues["inputQueues"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(avio_queues["egressQueues"].as_array().into_iter().flatten())
            .map(|queue| queue["highWaterBytes"].as_u64().unwrap_or(0))
            .sum::<u64>()
            / 1024;
        let sample = ResourceSample {
            scenario: meta.scenario.to_string(),
            label: meta.label.clone(),
            lifecycle: env.lifecycle.as_str().to_string(),
            pipelines: meta.pipelines,
            outputs: meta.outputs,
            ingest_types: meta.ingest_types.clone(),
            egress_mix: meta.egress_mix.clone(),
            transcode: meta.transcode.to_string(),
            restream_cpu_pct,
            ffmpeg_cpu_pct,
            total_cpu_pct,
            rss_kb,
            ffmpeg_count: ffmpeg.count,
            ffmpeg_rss_kb: ffmpeg.rss_kb,
            anonymous_kb: rollup.anonymous_kb,
            private_dirty_kb: rollup.private_dirty_kb,
            private_clean_kb: rollup.private_clean_kb,
            shared_clean_kb: rollup.shared_clean_kb,
            shared_dirty_kb: rollup.shared_dirty_kb,
            pss_kb: rollup.pss_kb,
            swap_kb: rollup.swap_kb,
            retained_kb,
            source_ring_kb,
            transcoder_ring_kb,
            tsmux_ring_kb,
            avio_len_kb,
            avio_hwm_kb,
            active_transcoder_buffers: telemetry["activeTranscoderBuffers"].as_u64().unwrap_or(0),
            ingests: telemetry["ingests"]
                .as_array()
                .map(|v| v.len())
                .unwrap_or(0),
            egresses: telemetry["egresses"]
                .as_array()
                .map(|v| v.len())
                .unwrap_or(0),
            stages: telemetry["stages"].as_array().map(|v| v.len()).unwrap_or(0),
            pipeline_count: health["pipelines"]
                .as_object()
                .map(|v| v.len())
                .unwrap_or(0),
            unattributed_kb: rss_kb.saturating_sub(retained_kb + avio_len_kb),
        };
        append_line(
            &env.samples_jsonl,
            &format!(
                "{}\n",
                serde_json::to_string(&resource_sample_json(&sample)).unwrap()
            ),
        )?;
        samples.push(sample);
    }
    Ok(summarize_resource_samples(meta, env.lifecycle, &samples))
}

fn summarize_resource_samples(
    meta: ResourceScenarioMeta<'_>,
    lifecycle: ResourceSweepLifecycle,
    samples: &[ResourceSample],
) -> ResourceAggregate {
    let restream_cpu_sum: f64 = samples.iter().map(|s| s.restream_cpu_pct).sum();
    let ffmpeg_cpu_sum: f64 = samples.iter().map(|s| s.ffmpeg_cpu_pct).sum();
    let total_cpu_sum: f64 = samples.iter().map(|s| s.total_cpu_pct).sum();
    let rss_sum: u64 = samples.iter().map(|s| s.rss_kb).sum();
    ResourceAggregate {
        scenario: meta.scenario.to_string(),
        label: meta.label,
        lifecycle: lifecycle.as_str().to_string(),
        pipelines: meta.pipelines,
        outputs: meta.outputs,
        ingest_types: meta.ingest_types,
        egress_mix: meta.egress_mix,
        transcode: meta.transcode.to_string(),
        sample_count: samples.len(),
        restream_cpu_avg_pct: round2(restream_cpu_sum / samples.len().max(1) as f64),
        restream_cpu_peak_pct: round2(
            samples
                .iter()
                .map(|s| s.restream_cpu_pct)
                .fold(0.0, f64::max),
        ),
        ffmpeg_cpu_avg_pct: round2(ffmpeg_cpu_sum / samples.len().max(1) as f64),
        ffmpeg_cpu_peak_pct: round2(samples.iter().map(|s| s.ffmpeg_cpu_pct).fold(0.0, f64::max)),
        total_cpu_avg_pct: round2(total_cpu_sum / samples.len().max(1) as f64),
        total_cpu_peak_pct: round2(samples.iter().map(|s| s.total_cpu_pct).fold(0.0, f64::max)),
        rss_avg_kb: round2(rss_sum as f64 / samples.len().max(1) as f64),
        rss_peak_kb: samples.iter().map(|s| s.rss_kb).max().unwrap_or(0),
        ffmpeg_rss_peak_kb: samples.iter().map(|s| s.ffmpeg_rss_kb).max().unwrap_or(0),
        retained_peak_kb: samples.iter().map(|s| s.retained_kb).max().unwrap_or(0),
        source_ring_peak_kb: samples.iter().map(|s| s.source_ring_kb).max().unwrap_or(0),
        transcoder_ring_peak_kb: samples
            .iter()
            .map(|s| s.transcoder_ring_kb)
            .max()
            .unwrap_or(0),
        tsmux_ring_peak_kb: samples.iter().map(|s| s.tsmux_ring_kb).max().unwrap_or(0),
        avio_len_peak_kb: samples.iter().map(|s| s.avio_len_kb).max().unwrap_or(0),
        avio_hwm_peak_kb: samples.iter().map(|s| s.avio_hwm_kb).max().unwrap_or(0),
        anonymous_peak_kb: samples.iter().map(|s| s.anonymous_kb).max().unwrap_or(0),
        private_dirty_peak_kb: samples
            .iter()
            .map(|s| s.private_dirty_kb)
            .max()
            .unwrap_or(0),
        shared_clean_peak_kb: samples.iter().map(|s| s.shared_clean_kb).max().unwrap_or(0),
        pss_peak_kb: samples.iter().map(|s| s.pss_kb).max().unwrap_or(0),
        unattributed_peak_kb: samples.iter().map(|s| s.unattributed_kb).max().unwrap_or(0),
        active_transcoder_buffers_peak: samples
            .iter()
            .map(|s| s.active_transcoder_buffers)
            .max()
            .unwrap_or(0),
        ingests_peak: samples.iter().map(|s| s.ingests).max().unwrap_or(0),
        egresses_peak: samples.iter().map(|s| s.egresses).max().unwrap_or(0),
        stages_peak: samples.iter().map(|s| s.stages).max().unwrap_or(0),
        pipeline_count_peak: samples.iter().map(|s| s.pipeline_count).max().unwrap_or(0),
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn read_proc_stat_ticks(pid: u32) -> Result<u64, String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
    let fields: Vec<&str> = stat.split_whitespace().collect();
    let utime = fields
        .get(13)
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or("proc stat missing utime")?;
    let stime = fields
        .get(14)
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or("proc stat missing stime")?;
    Ok(utime + stime)
}

fn read_proc_status_kb(pid: u32, key: &str) -> Result<u64, String> {
    let status =
        std::fs::read_to_string(format!("/proc/{pid}/status")).map_err(|e| e.to_string())?;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix(&format!("{key}:")) {
            return value
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| format!("failed to parse {key}"));
        }
    }
    Err(format!("{key} missing in /proc/{pid}/status"))
}

fn read_proc_status_kb_checked(pid: u32, key: &str, log_path: &Path) -> Result<u64, String> {
    read_proc_status_kb(pid, key).map_err(|error| {
        let tail = file_tail_lines(log_path, 20);
        if tail.is_empty() {
            format!("restream pid {pid} unavailable while reading {key}: {error}")
        } else {
            format!(
                "restream pid {pid} unavailable while reading {key}: {error}\nrestream log tail:\n{}",
                tail.join("\n")
            )
        }
    })
}

fn read_smaps_rollup(pid: u32) -> Result<ProcMemRollup, String> {
    let text =
        std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).map_err(|e| e.to_string())?;
    let value_for = |name: &str| -> u64 {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{name}:")))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
    };
    Ok(ProcMemRollup {
        anonymous_kb: value_for("Anonymous"),
        private_dirty_kb: value_for("Private_Dirty"),
        private_clean_kb: value_for("Private_Clean"),
        shared_clean_kb: value_for("Shared_Clean"),
        shared_dirty_kb: value_for("Shared_Dirty"),
        pss_kb: value_for("Pss"),
        swap_kb: value_for("Swap"),
    })
}

fn ffmpeg_children_stats(parent_pid: u32) -> Result<FfmpegStats, String> {
    let mut count = 0u64;
    let mut rss_kb = 0u64;
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let Some(pid) = name.to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        let status_path = format!("/proc/{pid}/status");
        let Ok(status) = std::fs::read_to_string(&status_path) else {
            continue;
        };
        let ppid = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:"))
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or(0);
        if ppid != parent_pid {
            continue;
        }
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let text = String::from_utf8_lossy(&cmdline);
        if text.contains("ffmpeg") {
            count += 1;
            rss_kb += read_proc_status_kb(pid, "VmRSS").unwrap_or(0);
            pids.push(pid);
        }
    }
    Ok(FfmpegStats {
        count,
        rss_kb,
        pids,
    })
}

fn resource_sample_json(sample: &ResourceSample) -> Value {
    json!({
        "scenario": sample.scenario,
        "label": sample.label,
        "lifecycle": sample.lifecycle,
        "pipelines": sample.pipelines,
        "outputs": sample.outputs,
        "ingestTypes": sample.ingest_types,
        "egressMix": sample.egress_mix,
        "transcode": sample.transcode,
        "restreamCpuPct": sample.restream_cpu_pct,
        "ffmpegCpuPct": sample.ffmpeg_cpu_pct,
        "totalCpuPct": sample.total_cpu_pct,
        "rssKb": sample.rss_kb,
        "ffmpegCount": sample.ffmpeg_count,
        "ffmpegRssKb": sample.ffmpeg_rss_kb,
        "anonymousKb": sample.anonymous_kb,
        "privateDirtyKb": sample.private_dirty_kb,
        "privateCleanKb": sample.private_clean_kb,
        "sharedCleanKb": sample.shared_clean_kb,
        "sharedDirtyKb": sample.shared_dirty_kb,
        "pssKb": sample.pss_kb,
        "swapKb": sample.swap_kb,
        "retainedKb": sample.retained_kb,
        "sourceRingKb": sample.source_ring_kb,
        "transcoderRingKb": sample.transcoder_ring_kb,
        "tsmuxRingKb": sample.tsmux_ring_kb,
        "avioLenKb": sample.avio_len_kb,
        "avioHwmKb": sample.avio_hwm_kb,
        "activeTranscoderBuffers": sample.active_transcoder_buffers,
        "ingests": sample.ingests,
        "egresses": sample.egresses,
        "stages": sample.stages,
        "pipelineCount": sample.pipeline_count,
        "unattributedKb": sample.unattributed_kb,
    })
}

fn resource_aggregate_json(aggregate: &ResourceAggregate) -> Value {
    json!({
        "scenario": aggregate.scenario,
        "label": aggregate.label,
        "lifecycle": aggregate.lifecycle,
        "pipelines": aggregate.pipelines,
        "outputs": aggregate.outputs,
        "ingestTypes": aggregate.ingest_types,
        "egressMix": aggregate.egress_mix,
        "transcode": aggregate.transcode,
        "sampleCount": aggregate.sample_count,
        "restreamCpuAvgPct": aggregate.restream_cpu_avg_pct,
        "restreamCpuPeakPct": aggregate.restream_cpu_peak_pct,
        "ffmpegCpuAvgPct": aggregate.ffmpeg_cpu_avg_pct,
        "ffmpegCpuPeakPct": aggregate.ffmpeg_cpu_peak_pct,
        "totalCpuAvgPct": aggregate.total_cpu_avg_pct,
        "totalCpuPeakPct": aggregate.total_cpu_peak_pct,
        "rssAvgKb": aggregate.rss_avg_kb,
        "rssPeakKb": aggregate.rss_peak_kb,
        "ffmpegRssPeakKb": aggregate.ffmpeg_rss_peak_kb,
        "retainedPeakKb": aggregate.retained_peak_kb,
        "sourceRingPeakKb": aggregate.source_ring_peak_kb,
        "transcoderRingPeakKb": aggregate.transcoder_ring_peak_kb,
        "tsmuxRingPeakKb": aggregate.tsmux_ring_peak_kb,
        "avioLenPeakKb": aggregate.avio_len_peak_kb,
        "avioHwmPeakKb": aggregate.avio_hwm_peak_kb,
        "anonymousPeakKb": aggregate.anonymous_peak_kb,
        "privateDirtyPeakKb": aggregate.private_dirty_peak_kb,
        "sharedCleanPeakKb": aggregate.shared_clean_peak_kb,
        "pssPeakKb": aggregate.pss_peak_kb,
        "unattributedPeakKb": aggregate.unattributed_peak_kb,
        "activeTranscoderBuffersPeak": aggregate.active_transcoder_buffers_peak,
        "ingestsPeak": aggregate.ingests_peak,
        "egressesPeak": aggregate.egresses_peak,
        "stagesPeak": aggregate.stages_peak,
        "pipelineCountPeak": aggregate.pipeline_count_peak,
    })
}

fn write_resource_sweep_csv(path: &Path, rows: &[ResourceAggregate]) -> Result<(), String> {
    let mut text = String::from(
        "scenario,label,lifecycle,pipelines,outputs,ingest_types,egress_mix,transcode,sample_count,restream_cpu_avg_pct,restream_cpu_peak_pct,ffmpeg_cpu_avg_pct,ffmpeg_cpu_peak_pct,total_cpu_avg_pct,total_cpu_peak_pct,rss_avg_kb,rss_peak_kb,ffmpeg_rss_peak_kb,retained_peak_kb,source_ring_peak_kb,transcoder_ring_peak_kb,tsmux_ring_peak_kb,avio_len_peak_kb,avio_hwm_peak_kb,anonymous_peak_kb,private_dirty_peak_kb,shared_clean_peak_kb,pss_peak_kb,unattributed_peak_kb,active_transcoder_buffers_peak,ingests_peak,egresses_peak,stages_peak,pipeline_count_peak\n",
    );
    for row in rows {
        text.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_escape(&row.scenario),
            csv_escape(&row.label),
            csv_escape(&row.lifecycle),
            row.pipelines,
            row.outputs,
            csv_escape(&row.ingest_types),
            csv_escape(&row.egress_mix),
            csv_escape(&row.transcode),
            row.sample_count,
            row.restream_cpu_avg_pct,
            row.restream_cpu_peak_pct,
            row.ffmpeg_cpu_avg_pct,
            row.ffmpeg_cpu_peak_pct,
            row.total_cpu_avg_pct,
            row.total_cpu_peak_pct,
            row.rss_avg_kb,
            row.rss_peak_kb,
            row.ffmpeg_rss_peak_kb,
            row.retained_peak_kb,
            row.source_ring_peak_kb,
            row.transcoder_ring_peak_kb,
            row.tsmux_ring_peak_kb,
            row.avio_len_peak_kb,
            row.avio_hwm_peak_kb,
            row.anonymous_peak_kb,
            row.private_dirty_peak_kb,
            row.shared_clean_peak_kb,
            row.pss_peak_kb,
            row.unattributed_peak_kb,
            row.active_transcoder_buffers_peak,
            row.ingests_peak,
            row.egresses_peak,
            row.stages_peak,
            row.pipeline_count_peak,
        ));
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

fn write_branch_matrix_markdown(
    path: &Path,
    backend: &str,
    srt_ingest_transport: &str,
    rows: &[ResourceAggregate],
) -> Result<(), String> {
    let mut selected: Vec<&ResourceAggregate> = rows.iter().collect();
    selected.sort_by_key(|row| {
        resource_egress_scenario(&row.scenario)
            .and_then(|scenario| scenario.branch_order)
            .unwrap_or(99)
    });

    let mut text = String::new();
    text.push_str("# Branch Matrix\n\n");
    text.push_str(&format!("- Backend: `{backend}`\n"));
    text.push_str(&format!(
        "- SRT ingest transport: `{srt_ingest_transport}`\n"
    ));
    if let Some(row) = selected.first() {
        text.push_str(&format!("- Lifecycle: `{}`\n", row.lifecycle));
        text.push_str(&format!("- Fanout per group: `{}`\n", row.label));
    }
    text.push('\n');
    text.push_str("| Shape | Outputs | Restream MB | Child FFmpeg MB | Combined MB | Total CPU % | Stages |\n");
    text.push_str("|---|---:|---:|---:|---:|---:|---:|\n");
    for row in &selected {
        let combined_mb = (row.rss_peak_kb + row.ffmpeg_rss_peak_kb) as f64 / 1024.0;
        text.push_str(&format!(
            "| {} | {} | {:.1} | {:.1} | {:.1} | {:.2} | {} |\n",
            branch_shape_label(&row.scenario),
            row.outputs,
            row.rss_peak_kb as f64 / 1024.0,
            row.ffmpeg_rss_peak_kb as f64 / 1024.0,
            combined_mb,
            row.total_cpu_avg_pct,
            row.stages_peak,
        ));
    }

    if let (Some(single), Some(single_plus_source), Some(dual), Some(dual_plus_source)) = (
        selected
            .iter()
            .find(|row| row.scenario == "egress-growth-transcode-mixed"),
        selected
            .iter()
            .find(|row| row.scenario == "egress-growth-source-plus-transcode-mixed"),
        selected
            .iter()
            .find(|row| row.scenario == "egress-growth-transcode-dual-mixed"),
        selected
            .iter()
            .find(|row| row.scenario == "egress-growth-source-plus-transcode-dual-mixed"),
    ) {
        text.push_str("\n## Deltas\n\n");
        text.push_str("| Comparison | Output Delta | Combined MB Delta | Total CPU Delta |\n");
        text.push_str("|---|---:|---:|---:|\n");
        text.push_str(&format!(
            "| Add passthrough on top of one transcode family | {} | {:.1} | {:.2} |\n",
            single_plus_source.outputs.saturating_sub(single.outputs),
            ((single_plus_source.rss_peak_kb + single_plus_source.ffmpeg_rss_peak_kb)
                .saturating_sub(single.rss_peak_kb + single.ffmpeg_rss_peak_kb)) as f64
                / 1024.0,
            single_plus_source.total_cpu_avg_pct - single.total_cpu_avg_pct,
        ));
        text.push_str(&format!(
            "| Add a second transcode family | {} | {:.1} | {:.2} |\n",
            dual.outputs.saturating_sub(single.outputs),
            ((dual.rss_peak_kb + dual.ffmpeg_rss_peak_kb)
                .saturating_sub(single.rss_peak_kb + single.ffmpeg_rss_peak_kb)) as f64
                / 1024.0,
            dual.total_cpu_avg_pct - single.total_cpu_avg_pct,
        ));
        text.push_str(&format!(
            "| Add passthrough on top of two transcode families | {} | {:.1} | {:.2} |\n",
            dual_plus_source.outputs.saturating_sub(dual.outputs),
            ((dual_plus_source.rss_peak_kb + dual_plus_source.ffmpeg_rss_peak_kb)
                .saturating_sub(dual.rss_peak_kb + dual.ffmpeg_rss_peak_kb)) as f64
                / 1024.0,
            dual_plus_source.total_cpu_avg_pct - dual.total_cpu_avg_pct,
        ));
    }

    std::fs::write(path, text).map_err(|e| e.to_string())
}

fn branch_shape_label(scenario: &str) -> &'static str {
    resource_egress_scenario(scenario)
        .map(ResourceEgressScenario::branch_label)
        .unwrap_or("custom")
}

fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

async fn json_response(request: reqwest::RequestBuilder) -> Result<Value, String> {
    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "HTTP {status}: {}",
            String::from_utf8_lossy(&bytes)
        ));
    }
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
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

/// Test: SRT ingest -> HLS HTTP PUT upload for YouTube-style and path-style sinks.
/// Files written by the synthetic HLS PUT sink.
struct HlsPutArtifacts {
    youtube_playlist: PathBuf,
    youtube_segment: PathBuf,
}

/// Shared filesystem/request state for the synthetic HLS PUT sink.
struct HlsPutSinkState {
    root: PathBuf,
    requests_path: PathBuf,
    write_lock: Mutex<()>,
}

/// State for an HLS PUT sink that intentionally delays responses.
#[derive(Clone)]
struct HlsPutHangSinkState {
    cancel: CancellationToken,
    delay: Duration,
}

async fn start_hls_put_sink(
    port: u16,
    root: PathBuf,
) -> Result<(CancellationToken, tokio::task::JoinHandle<()>), String> {
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let state = Arc::new(HlsPutSinkState {
        requests_path: root.join("requests.jsonl"),
        root,
        write_lock: Mutex::new(()),
    });
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route("/*path", put(hls_put_sink_put))
        .layer(DefaultBodyLimit::disable())
        .with_state(state);
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| e.to_string())?;
    let cancel = CancellationToken::new();
    let server_cancel = cancel.clone();
    let handle = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app)
            .with_graceful_shutdown(server_cancel.cancelled_owned())
            .await
        {
            eprintln!("[hls-put-sink] server failed: {err}");
        }
    });
    Ok((cancel, handle))
}

async fn start_hls_put_hang_sink(
    port: u16,
    delay: Duration,
) -> Result<(CancellationToken, tokio::task::JoinHandle<()>), String> {
    let cancel = CancellationToken::new();
    let state = HlsPutHangSinkState {
        cancel: cancel.clone(),
        delay,
    };
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route(
            "/*path",
            put(
                |State(state): State<HlsPutHangSinkState>,
                 OriginalUri(_uri): OriginalUri,
                 _headers: HeaderMap,
                 _body: Bytes| async move {
                    tokio::select! {
                        _ = state.cancel.cancelled() => StatusCode::SERVICE_UNAVAILABLE,
                        _ = tokio::time::sleep(state.delay) => StatusCode::NO_CONTENT,
                    }
                },
            ),
        )
        .layer(DefaultBodyLimit::disable())
        .with_state(state);
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| e.to_string())?;
    let server_cancel = cancel.clone();
    let handle = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app)
            .with_graceful_shutdown(server_cancel.cancelled_owned())
            .await
        {
            eprintln!("[hls-put-hang-sink] server failed: {err}");
        }
    });
    Ok((cancel, handle))
}

async fn hls_put_sink_put(
    State(state): State<Arc<HlsPutSinkState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let name =
        hls_put_sink_file_name(uri.path(), uri.query()).unwrap_or_else(|| "index.m3u8".to_string());
    let name = name.replace('\\', "/").trim_start_matches('/').to_string();
    if name.is_empty() || name.split('/').any(|part| part == "..") {
        return StatusCode::BAD_REQUEST;
    }

    let target = state.root.join(&name);
    if let Some(parent) = target.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        eprintln!(
            "[hls-put-sink] failed to create {}: {err}",
            parent.display()
        );
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    if let Err(err) = std::fs::write(&target, &body) {
        eprintln!("[hls-put-sink] failed to write {}: {err}", target.display());
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE.as_str())
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let record = json!({
        "path": uri.to_string(),
        "file": name,
        "contentType": content_type,
        "bytes": body.len(),
    });
    let _guard = state.write_lock.lock().unwrap_or_else(|e| e.into_inner());
    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&state.requests_path)
    {
        Ok(mut file) => {
            if let Err(err) = writeln!(file, "{record}") {
                eprintln!(
                    "[hls-put-sink] failed to append {}: {err}",
                    state.requests_path.display()
                );
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        }
        Err(err) => {
            eprintln!(
                "[hls-put-sink] failed to open {}: {err}",
                state.requests_path.display()
            );
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }
    StatusCode::NO_CONTENT
}

fn hls_put_sink_file_name(path: &str, query: Option<&str>) -> Option<String> {
    query
        .and_then(|query| {
            query.split('&').find_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                (key == "file").then(|| value.to_string())
            })
        })
        .or_else(|| {
            let trimmed = path.trim_start_matches('/');
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
}

async fn wait_for_hls_put_artifacts(
    sink_dir: &Path,
    timeout: Duration,
) -> Result<HlsPutArtifacts, String> {
    let deadline = Instant::now() + timeout;
    let youtube_playlist = sink_dir.join("out.m3u8");
    loop {
        let youtube_segment = first_segment_in(sink_dir);
        if youtube_playlist.is_file()
            && file_nonempty(&youtube_playlist)
            && let Some(youtube_segment) = youtube_segment
        {
            return Ok(HlsPutArtifacts {
                youtube_playlist,
                youtube_segment,
            });
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for HLS PUT playlist/segment artifacts in {}",
                sink_dir.display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn first_segment_in(dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| is_segment_file(name, "seg"))
                && file_nonempty(path)
        })
        .collect();
    entries.sort();
    entries.into_iter().next()
}

fn file_nonempty(path: &Path) -> bool {
    path.metadata().map(|meta| meta.len() > 0).unwrap_or(false)
}

fn validate_hls_playlist(path: &Path, label: &str) -> Result<(), String> {
    let playlist = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if !playlist.contains("#EXTM3U") {
        return Err(format!("{label} HLS PUT playlist missing EXTM3U header"));
    }
    if !playlist.contains(".ts") {
        return Err(format!(
            "{label} HLS PUT playlist missing segment reference"
        ));
    }
    Ok(())
}

fn read_hls_put_requests(sink_dir: &Path) -> Result<Vec<Value>, String> {
    let path = sink_dir.join("requests.jsonl");
    let body = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    body.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|e| e.to_string()))
        .collect()
}

fn request_seen(requests: &[Value], predicate: impl Fn(&Value) -> bool) -> bool {
    requests.iter().any(predicate)
}

fn is_segment_file(file: &str, prefix: &str) -> bool {
    file.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(".ts"))
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
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

/// Stream-selection policy for FFmpeg publishers spawned by the harness.
#[derive(Clone, Copy)]
enum PublishTrackSelection {
    PrimaryAv,
    AllStreams,
}

fn sweep_fixture(config: SweepConfig, bitrate_label: &str) -> Result<PathBuf, String> {
    restream::test_fixtures::bench_transport_fixture(
        config.video_codec,
        bitrate_label,
        config.multi_audio,
    )
}

fn ramp_fixture() -> Result<PathBuf, String> {
    restream::test_fixtures::bench_transport_fixture("h264", "4M", false)
}

fn checked_h264_fixture() -> Result<PathBuf, String> {
    restream::test_fixtures::canonical_h264_ts_fixture()
}

fn spawn_publisher_with_selection(
    path: &Path,
    url: &str,
    format: &str,
    selection: PublishTrackSelection,
    log_path: Option<&Path>,
) -> Result<Child, String> {
    let ffmpeg_threads = std::env::var("HARNESS_FFMPEG_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2)
        .max(1);
    let mut cmd = command_with_optional_cgroup("ffmpeg", "publisher");
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-threads"]);
    cmd.arg(ffmpeg_threads.to_string());
    cmd.args(["-re", "-stream_loop", "-1", "-i"]);
    cmd.arg(path);
    match selection {
        PublishTrackSelection::AllStreams => {
            cmd.args(["-map", "0"]);
        }
        PublishTrackSelection::PrimaryAv => {
            cmd.args(["-map", "0:v", "-map", "0:a:0"]);
        }
    }
    if format == "mpegts" {
        cmd.args(["-mpegts_flags", "+resend_headers"]);
        cmd.args(["-bsf:v", "dump_extra=freq=keyframe"]);
    }
    cmd.args(["-c", "copy", "-f", format]).arg(url);
    if let Some(log_path) = log_path {
        let log = std::fs::File::create(log_path).map_err(|e| e.to_string())?;
        let stderr = log.try_clone().map_err(|e| e.to_string())?;
        cmd.stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true);
    } else {
        // stderr must not be piped without a consumer — the 64KB pipe buffer
        // fills and blocks ffmpeg, hanging the test. Discard it when a fixture
        // publisher does not need a dedicated log file.
        cmd.stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
    }
    cmd.spawn().map_err(|e| e.to_string())
}

async fn spawn_publisher(
    path: &Path,
    url: &str,
    format: &str,
    map_all: bool,
) -> Result<Child, String> {
    spawn_publisher_with_selection(
        path,
        url,
        format,
        if map_all {
            PublishTrackSelection::AllStreams
        } else {
            PublishTrackSelection::PrimaryAv
        },
        None,
    )
}

/// Probe a live stream URL without buffering its contents into the harness.
async fn ffprobe(url: &str) -> Result<Value, String> {
    // kill_on_drop(true) ensures the subprocess is killed when the timeout
    // drops the future, preventing orphan ffprobe processes (T2 fix).
    let child = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-probesize",
            "2M",
            "-analyzeduration",
            "2M",
            "-show_entries",
            "stream=index,codec_name,codec_type,width,height,sample_rate,channels",
            "-of",
            "json",
            url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;

    let output = tokio::time::timeout(Duration::from_secs(12), child.wait_with_output())
        .await
        .map_err(|_| format!("ffprobe timed out: {url}"))?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

async fn ffprobe_video_packets(url: &str, output_path: &Path) -> Result<Value, String> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let child = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-read_intervals",
            "%+5",
            "-select_streams",
            "v:0",
            "-show_packets",
            "-show_entries",
            "packet=pts_time,dts_time",
            "-of",
            "json",
            url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;

    let output = tokio::time::timeout(Duration::from_secs(25), child.wait_with_output())
        .await
        .map_err(|_| format!("ffprobe packet capture timed out: {url}"))?
        .map_err(|e| e.to_string())?;
    std::fs::write(output_path, &output.stdout).map_err(|e| e.to_string())?;
    let stderr_path = artifact_path("bframe-ffprobe.log");
    std::fs::write(&stderr_path, &output.stderr).map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe packet capture failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

fn packet_times(packet_probe: &Value) -> impl Iterator<Item = (Option<f64>, Option<f64>)> + '_ {
    packet_probe["packets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|packet| {
            (
                packet["pts_time"].as_str().and_then(parse_probe_time),
                packet["dts_time"].as_str().and_then(parse_probe_time),
            )
        })
}

fn parse_probe_time(value: &str) -> Option<f64> {
    if value == "N/A" {
        None
    } else {
        value.parse().ok()
    }
}

fn count_video_packets(packet_probe: &Value) -> usize {
    packet_times(packet_probe)
        .filter(|(_, dts)| dts.is_some())
        .count()
}

fn count_bframe_packets(packet_probe: &Value) -> usize {
    packet_times(packet_probe)
        .filter(|(pts, dts)| matches!((pts, dts), (Some(pts), Some(dts)) if pts > dts))
        .count()
}

fn video_dts_monotone(packet_probe: &Value) -> bool {
    let mut last = None;
    for (_, dts) in packet_times(packet_probe) {
        let Some(dts) = dts else {
            continue;
        };
        if last.is_some_and(|last| dts < last) {
            return false;
        }
        last = Some(dts);
    }
    true
}

fn normalized_streams(probe: &Value) -> Result<Value, String> {
    let streams = probe["streams"]
        .as_array()
        .ok_or("ffprobe output has no streams")?;
    let mut normalized: Vec<Value> = streams
        .iter()
        .filter_map(|stream| match stream["codec_type"].as_str() {
            Some("video") => Some(json!({
                "type": "video",
                "codec": stream["codec_name"],
                "width": stream["width"],
                "height": stream["height"],
            })),
            Some("audio") => Some(json!({
                "type": "audio",
                "codec": stream["codec_name"],
                "sampleRate": stream["sample_rate"],
                "channels": stream["channels"],
            })),
            _ => None,
        })
        .collect();
    normalized.sort_by_key(|entry| entry["type"].as_str().unwrap_or("").to_string());
    Ok(Value::Array(normalized))
}

fn assert_media_only(probe: &Value, label: &str) -> Result<(), String> {
    let streams = probe["streams"]
        .as_array()
        .ok_or_else(|| format!("{label}: ffprobe output has no streams"))?;
    let non_media: Vec<&str> = streams
        .iter()
        .filter_map(|stream| stream["codec_type"].as_str())
        .filter(|kind| !matches!(*kind, "video" | "audio"))
        .collect();
    let video_count = streams
        .iter()
        .filter(|stream| stream["codec_type"] == "video")
        .count();
    let audio_count = streams
        .iter()
        .filter(|stream| stream["codec_type"] == "audio")
        .count();
    if !non_media.is_empty() || video_count != 1 || audio_count < 1 {
        return Err(format!(
            "{label}: expected 1 video + >=1 audio, got video={video_count} \
             audio={audio_count} non_media={non_media:?}"
        ));
    }
    Ok(())
}

fn media_dir_entries(path: &Path) -> Result<HashSet<String>, String> {
    let mut files = HashSet::new();
    if !path.exists() {
        return Ok(files);
    }
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_file() {
            files.insert(entry.file_name().to_string_lossy().to_string());
        }
    }
    Ok(files)
}

async fn wait_for_new_media_file(
    media_dir: &Path,
    before: &HashSet<String>,
    extension: &str,
    timeout: Duration,
) -> Result<PathBuf, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let files = media_dir_entries(media_dir)?;
        if let Some(name) = files
            .iter()
            .find(|name| !before.contains(*name) && name.ends_with(extension))
        {
            return Ok(media_dir.join(name));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no new {extension} media file appeared in {} within {}s",
                media_dir.display(),
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn absolute_delta_secs(actual: f64, expected: f64) -> f64 {
    (actual - expected).abs()
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
    fn mixed_adaptive_ring_snapshot_accepts_capacity_or_depth_without_overflow() {
        let resized = mixed_adaptive_ring_snapshot(&json!({
            "sourceRing": {
                "capacity": 2048,
                "bufferDepthSecs": 0.5,
                "readers": [{"overflowCount": 0}]
            }
        }));
        assert!(resized.resized);
        assert!(resized.adequate);
        assert!(resized.passed);

        let deep_enough = mixed_adaptive_ring_snapshot(&json!({
            "sourceRing": {
                "capacity": 512,
                "bufferDepthSecs": 5.1,
                "readers": [{"overflowCount": 0}]
            }
        }));
        assert!(!deep_enough.resized);
        assert!(deep_enough.adequate);
        assert!(deep_enough.passed);

        let overflowed = mixed_adaptive_ring_snapshot(&json!({
            "sourceRing": {
                "capacity": 2048,
                "bufferDepthSecs": 5.1,
                "readers": [{"overflowCount": 1}]
            }
        }));
        assert!(overflowed.adequate);
        assert!(!overflowed.passed);
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
                    MixedSharedBatchGroup::from_str(&batch.group).unwrap(),
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
