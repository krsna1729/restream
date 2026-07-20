use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

use crate::{RampApi, cleanup_ramp_db, harness_admin_password, stop_child};

use super::ports::TestPorts;
use super::setup::{absolute_path, command_with_optional_cgroup};

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
