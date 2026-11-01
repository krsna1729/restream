use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

use crate::{RampApi, cleanup_ramp_db, harness_admin_password, harness_http_client, stop_child};

use super::ports::TestPorts;
use super::setup::{absolute_path, command_with_optional_cgroup, remove_mediamtx_config_env};

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
    let (rtmps_cert, _) = restream_engine::test_fixtures::rtmps_harness_cert_fixture()?;
    let mut command = command_with_optional_cgroup(bin, &format!("restream-{}", ports.http));
    command
        .env("RESTREAM_HTTP_PORT", ports.http.to_string())
        .env("RESTREAM_RTMP_PORT", ports.rtmp.to_string())
        .env("RESTREAM_SRT_PORT", ports.srt.to_string())
        .env("RESTREAM_INITIAL_ADMIN_PASSWORD", harness_admin_password())
        .env("RESTREAM_LOG_DIR", &log_dir)
        .env("RESTREAM_DB_PATH", db_path.to_string_lossy().to_string())
        .env(
            "RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM",
            absolute_path(&rtmps_cert)?.to_string_lossy().to_string(),
        )
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

/// Spawns `ffmpeg -nostdin -hide_banner -loglevel error <argv>` in cgroup
/// `scope`, with stdout and stderr in `log_path`.
pub(crate) fn spawn_logged_ffmpeg(
    ffmpeg: &Path,
    argv: &[String],
    log_path: &Path,
    scope: &str,
) -> Result<Child, String> {
    let mut command = command_with_optional_cgroup(ffmpeg, scope);
    command.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    command.args(argv);
    let log = std::fs::File::create(log_path).map_err(|error| error.to_string())?;
    let stderr = log.try_clone().map_err(|error| error.to_string())?;
    command
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", ffmpeg.display()))
}

/// Spawns MediaMTX from `command` with `config`, both streams in `log`, then
/// waits up to `ready_timeout` for its API on `api_port`; a peer that never
/// becomes ready is stopped and reported as `"{label} did not become ready"`.
pub(crate) async fn spawn_mediamtx(
    mut command: Command,
    config: &Path,
    log: std::fs::File,
    api_port: u16,
    ready_timeout: Duration,
    label: &str,
) -> Result<Child, String> {
    let stderr_log = log.try_clone().map_err(|e| e.to_string())?;
    let mut child = remove_mediamtx_config_env(&mut command)
        .arg(config)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr_log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{api_port}/v3/paths/list"),
        ready_timeout,
    )
    .await
    {
        stop_child(&mut child).await;
        return Err(format!("{label} did not become ready: {err}"));
    }
    Ok(child)
}

/// Overrides the MediaMTX executable; the version check still applies.
const MEDIAMTX_BIN_ENV: &str = "RESTREAM_MEDIAMTX_BIN";
const MEDIAMTX_INSTALL_HINT: &str = "install the pinned release with \
     scripts/dev/bootstrap-runtime.sh --mediamtx-only or cargo xtask fetch mediamtx";

/// The one MediaMTX release every harness mode runs (`scripts/data/mediamtx.tsv`).
pub(crate) fn pinned_mediamtx_version() -> &'static str {
    include_str!("../../../../../scripts/data/mediamtx.tsv")
        .lines()
        .find(|line| !line.starts_with('#'))
        .and_then(|line| line.split('\t').next())
        .expect("scripts/data/mediamtx.tsv pins a version")
}

/// The pinned MediaMTX executable: `RESTREAM_MEDIAMTX_BIN`, else
/// `.local/bin/mediamtx` (`cargo xtask fetch mediamtx`), else `mediamtx` on
/// `PATH` (`scripts/dev/bootstrap-runtime.sh`). Refused unless it reports the
/// pinned version, so a stale host cannot run a different peer than CI.
pub(crate) fn pinned_mediamtx() -> Result<&'static Path, String> {
    static RESOLVED: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    RESOLVED
        .get_or_init(resolve_pinned_mediamtx)
        .as_deref()
        .map_err(Clone::clone)
}

fn resolve_pinned_mediamtx() -> Result<PathBuf, String> {
    let local = Path::new(".local/bin/mediamtx");
    let bin = match std::env::var_os(MEDIAMTX_BIN_ENV) {
        Some(path) => absolute_path(Path::new(&path))?,
        None if local.is_file() => absolute_path(local)?,
        None => PathBuf::from("mediamtx"),
    };
    let output = std::process::Command::new(&bin)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("{}: {error}; {MEDIAMTX_INSTALL_HINT}", bin.display()))?;
    check_mediamtx_version(&String::from_utf8_lossy(&output.stdout))
        .map_err(|error| format!("{} {error}", bin.display()))?;
    Ok(bin)
}

pub(crate) fn check_mediamtx_version(reported: &str) -> Result<(), String> {
    let pinned = pinned_mediamtx_version();
    if reported.split_whitespace().next() == Some(pinned) {
        Ok(())
    } else {
        Err(format!(
            "reports {:?}; the harness is pinned to MediaMTX {pinned}; {MEDIAMTX_INSTALL_HINT}",
            reported.trim()
        ))
    }
}

/// `udpReadBufferSize` line for every harness MediaMTX config with SRT: the
/// receive buffer Restream's own SRT sockets ask for, so MediaMTX's one SRT
/// socket (shared by every Restream SRT output) does not overflow first.
/// Capped at `net.core.rmem_max`: MediaMTX refuses to start when the kernel
/// grants less than it asked for, and a container cannot raise the limit.
pub(crate) fn mediamtx_udp_read_buffer_line() -> String {
    let rmem_max = std::fs::read_to_string("/proc/sys/net/core/rmem_max")
        .ok()
        .and_then(|value| value.trim().parse().ok());
    udp_read_buffer_line(rmem_max)
}

pub(crate) fn udp_read_buffer_line(rmem_max: Option<usize>) -> String {
    rmem_max.map_or_else(String::new, |max| {
        format!(
            "udpReadBufferSize: {}\n",
            restream_srt::DESIRED_UDP_BUF.min(max)
        )
    })
}

/// MediaMTX config with RTMP and SRT ingest and the API, nothing else.
pub(crate) fn plain_mediamtx_config(rtmp_port: u16, srt_port: u16, api_port: u16) -> String {
    let udp_buffer = mediamtx_udp_read_buffer_line();
    format!(
        "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\n{udp_buffer}rtmp: yes\nrtmpAddress: :{rtmp_port}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{srt_port}\nhls: no\nwebrtc: no\nmoq: no\napi: yes\napiAddress: :{api_port}\nmetrics: no\npaths:\n  all:\n"
    )
}

pub(crate) async fn wait_for_http_ok(url: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let client = harness_http_client();
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
