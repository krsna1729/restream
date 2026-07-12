use super::*;

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

pub(crate) fn is_bench_profile(path: &Path) -> bool {
    matches!(path_profile(path), Some("bench"))
}

pub(crate) fn default_work_db_path(work_dir: &Path, file_name: &str) -> PathBuf {
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

pub(crate) fn strip_netns_opt(raw: &[String]) -> Vec<String> {
    raw.iter()
        .filter(|arg| arg.as_str() != "--no-netns")
        .cloned()
        .collect()
}

pub(crate) fn netns_available() -> bool {
    std::process::Command::new("unshare")
        .args(["--net", "--user", "--map-root-user", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub(crate) fn maybe_reexec_in_port_namespace() -> Result<(), String> {
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

    if !status.success() {
        return Err(format!(
            "{command} could not enter the default private network namespace; rerun with --no-netns to use the host-network fallback"
        ));
    }

    let code = status.code().unwrap_or(1);
    unsafe { libc::_exit(code) };
}

pub(crate) fn ensure_measurement_profile(command: &str, raw: &[String]) -> Result<(), String> {
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
        "{command} requires bench-profile binaries for valid measurements; build them with `scripts/build/bench-harness.sh` and run `target/bench/test_harness`"
    ))
}

pub(crate) fn harness_runtime_worker_threads() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    std::env::var("HARNESS_TOKIO_WORKER_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(cpus.clamp(2, 16))
        .max(1)
}

pub(crate) fn harness_runtime_max_blocking_threads() -> usize {
    std::env::var("HARNESS_TOKIO_MAX_BLOCKING_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(256)
        .max(1)
}

pub(crate) fn default_restream_bin() -> PathBuf {
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
pub(crate) fn mixed_command_artifact_path(command: &str) -> Option<PathBuf> {
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

pub(crate) fn artifact_path(name: &str) -> PathBuf {
    std::env::var_os("TEST_HARNESS_ARTIFACT_DIR")
        .or_else(|| std::env::var_os("WORK_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".local/artifacts/latest"))
        .join(name)
}

pub(crate) fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

pub(crate) fn maybe_global_process_cleanup() {
    if !env_flag("ALLOW_GLOBAL_PROCESS_CLEANUP") {
        return;
    }
    for program in ["restream", "mediamtx", "ffmpeg"] {
        let _ = std::process::Command::new("pkill")
            .args(["-x", program])
            .status();
    }
}

pub(crate) fn maybe_prune_old_artifacts() -> Result<(), String> {
    if env_flag("KEEP_ARTIFACTS") {
        return Ok(());
    }
    let artifact_root = PathBuf::from(".local/artifacts");
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

pub(crate) fn command_with_optional_cgroup(program: impl AsRef<OsStr>, scope: &str) -> Command {
    if !env_flag("HARNESS_USE_CGROUP_WRAPPER") {
        return Command::new(program);
    }
    let mut command = Command::new("scripts/native/cgroup-wrap.sh");
    command.arg("--scope").arg(scope).arg("--").arg(program);
    command
}

pub(crate) const MEDIAMTX_CONFIG_ENV_NAMES: [&str; 4] =
    ["MTX_RTMP", "MTX_SRT", "MTX_HLS", "MTX_API"];

pub(crate) fn remove_mediamtx_config_env(command: &mut Command) -> &mut Command {
    for name in MEDIAMTX_CONFIG_ENV_NAMES {
        command.env_remove(name);
    }
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

pub(crate) fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path))
    }
}

pub(crate) fn env_secs(name: &str, default: u64) -> u64 {
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
pub(crate) struct HarnessSrtCrypto {
    pub(crate) label: String,
    pub(crate) passphrase: Option<String>,
    pub(crate) pbkeylen: Option<String>,
}

impl HarnessSrtCrypto {
    pub(crate) fn plaintext() -> Self {
        Self {
            label: "plaintext".to_string(),
            passphrase: None,
            pbkeylen: None,
        }
    }

    pub(crate) fn encrypted(pbkeylen: u32) -> Self {
        Self {
            label: format!("encrypted-{pbkeylen}"),
            passphrase: Some("0123456789abcd".to_string()),
            pbkeylen: Some(pbkeylen.to_string()),
        }
    }

    pub(crate) fn transport_label(&self) -> String {
        match (&self.passphrase, &self.pbkeylen) {
            (None, _) => "plaintext".to_string(),
            (Some(_), Some(len)) => format!("encrypted-{len}"),
            (Some(_), None) => "encrypted".to_string(),
        }
    }
}

pub(crate) fn harness_srt_crypto_from_env() -> HarnessSrtCrypto {
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

pub(crate) fn parse_srt_crypto_variants(
    name: &str,
    default: &str,
) -> Result<Vec<HarnessSrtCrypto>, String> {
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

pub(crate) fn append_srt_crypto(url: String, crypto: &HarnessSrtCrypto) -> String {
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

pub(crate) fn apply_srt_listener_env(cmd: &mut Command, crypto: &HarnessSrtCrypto) {
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

pub(crate) fn apply_harness_srt_listener_env(cmd: &mut Command) {
    apply_srt_listener_env(cmd, &harness_srt_crypto_from_env());
}

// ── Shared test infrastructure (Phase 1) ────────────────────────────────────
//
// `TestPorts` + `start_restream_child` de-duplicate the port and child-process
// setup that was previously inlined in `start_ramp_restream` and
// `start_mixed_restream`.

/// Concrete restream listener ports for one isolated harness process.
pub(crate) struct TestPorts {
    pub(crate) http: u16,
    pub(crate) rtmp: u16,
    pub(crate) srt: u16,
}

/// Synthesized non-overlapping port ranges for restream, MediaMTX, and probes.
#[derive(Clone, Copy)]
pub(crate) struct HarnessPortDefaults {
    pub(crate) restream_http: u16,
    pub(crate) restream_rtmp: u16,
    pub(crate) restream_srt: u16,
    pub(crate) mtx_rtmp: u16,
    pub(crate) mtx_srt: u16,
    pub(crate) mtx_hls: u16,
    pub(crate) mtx_api: u16,
    pub(crate) sink: u16,
    pub(crate) hls_put: u16,
    pub(crate) ffmpeg_srt_sink_base: u16,
    pub(crate) ffmpeg_signal_sink_base: u16,
}

static HARNESS_PORT_DEFAULTS: OnceLock<HarnessPortDefaults> = OnceLock::new();

impl TestPorts {
    pub(crate) fn from_env() -> Self {
        let ports = harness_port_defaults();
        Self {
            http: ports.restream_http,
            rtmp: ports.restream_rtmp,
            srt: ports.restream_srt,
        }
    }
}

pub(crate) async fn start_restream_child(
    bin: &Path,
    ports: &TestPorts,
    db_path: &Path,
    log_path: &Path,
) -> Result<Child, String> {
    start_restream_child_opts(bin, ports, db_path, log_path, true, None, &[]).await
}

pub(crate) async fn start_restream_api(
    bin: &Path,
    ports: &TestPorts,
    db_path: &Path,
    log_path: &Path,
) -> Result<(Child, RampApi), String> {
    let child = start_restream_child(bin, ports, db_path, log_path).await?;
    Ok((child, login_api(ports).await?))
}

pub(crate) async fn login_api(ports: &TestPorts) -> Result<RampApi, String> {
    let mut api = RampApi::new(ports.http);
    api.login().await?;
    Ok(api)
}

pub(crate) async fn start_restream_child_with_env(
    bin: &Path,
    ports: &TestPorts,
    db_path: &Path,
    log_path: &Path,
    env_overrides: &[(&str, String)],
) -> Result<Child, String> {
    start_restream_child_opts(bin, ports, db_path, log_path, true, None, env_overrides).await
}

pub(crate) async fn start_restream_child_in_media_dir(
    bin: &Path,
    ports: &TestPorts,
    db_path: &Path,
    log_path: &Path,
    media_dir: &Path,
) -> Result<Child, String> {
    start_restream_child_opts(bin, ports, db_path, log_path, true, Some(media_dir), &[]).await
}

pub(crate) async fn start_restream_child_opts(
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
        .env("RESTREAM_INITIAL_ADMIN_PASSWORD", harness_admin_password())
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
pub(crate) async fn wait_for_http_ok(url: &str, timeout: Duration) -> Result<(), String> {
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

pub(crate) fn proc_net_has_listening_port(contents: &str, port: u16) -> bool {
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

pub(crate) async fn wait_for_tcp_listener_ready(
    port: u16,
    timeout: Duration,
) -> Result<(), String> {
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
pub(crate) fn append_line(path: &Path, line: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(line.as_bytes()).map_err(|e| e.to_string())
}

/// Count and RSS total for external FFmpeg worker processes.
#[derive(Clone)]
pub(crate) struct FfmpegStats {
    pub(crate) count: u64,
    pub(crate) rss_kb: u64,
    pub(crate) pids: Vec<u32>,
}

pub(crate) async fn ffmpeg_pipe1_stats() -> FfmpegStats {
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

pub(crate) async fn process_cpu_pct(pid: u32) -> Option<String> {
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

pub(crate) async fn process_rss_kb(pid: u32) -> Option<u64> {
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

pub(crate) async fn get_logs(api: &RampApi, query: &str) -> Result<Vec<Value>, String> {
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

pub(crate) fn parse_log_fields(log: &Value) -> Option<Value> {
    let fields = log.get("fields")?;
    match fields {
        Value::Object(_) => Some(fields.clone()),
        Value::String(raw) if !raw.trim().is_empty() => serde_json::from_str(raw).ok(),
        _ => None,
    }
}

pub(crate) fn log_has_correlation_id(log: &Value) -> bool {
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

pub(crate) async fn verify_api_smoke_history_contract(api: &RampApi) -> Result<Value, String> {
    let lifecycle_logs = get_logs(api, "event_class=lifecycle&limit=50&order=desc").await?;

    Ok(json!({
        "logsEndpointOk": true,
        "logCount": lifecycle_logs.len(),
    }))
}

pub(crate) async fn verify_live_history_contract(
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

pub(crate) async fn verify_external_transcoder_history_contract(
    api: &RampApi,
) -> Result<Value, String> {
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

pub(crate) fn harness_port_defaults() -> HarnessPortDefaults {
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

pub(crate) fn env_or_allocated_port(name: &str, default: u16, reserved: &mut HashSet<u16>) -> u16 {
    env_or_allocated_port_range(name, default, 1, reserved)
}

pub(crate) fn env_or_allocated_port_range(
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

pub(crate) fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
