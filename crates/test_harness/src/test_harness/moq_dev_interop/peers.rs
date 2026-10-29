//! The pinned moq.dev peers: the `moq` CLI and `moq-relay` binaries, the
//! run's ports, the relay process and the CLI import/export children.

use super::*;

/// A pinned moq.dev binary: where `cargo xtask fetch moq-dev`
/// installs it, the variable overriding that path, and what `--version`
/// must print.
pub(crate) struct PinnedBin {
    pub(crate) path: &'static str,
    pub(crate) env: &'static str,
    pub(crate) version: &'static str,
}

pub(crate) const MOQ_CLI: PinnedBin = PinnedBin {
    path: ".local/bin/moq-0.14.7",
    env: "MOQ_DEV_INTEROP_MOQ_BIN",
    version: "moq 0.14.7",
};

pub(crate) const MOQ_RELAY: PinnedBin = PinnedBin {
    path: ".local/bin/moq-relay-0.17.7",
    env: "MOQ_DEV_INTEROP_RELAY_BIN",
    version: "moq-relay 0.17.7",
};

/// CLI/log spelling; the corresponding native ALPN is `moqt-19`.
pub(crate) const IETF_VERSION: &str = "moq-transport-19";
pub(crate) const LITE_VERSION: &str = "moq-lite-06";

/// Resolve and version-check a pinned binary.
pub(crate) async fn pinned_bin(pin: &PinnedBin) -> Result<PathBuf, String> {
    let bin = std::env::var_os(pin.env)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pin.path));
    let bin = absolute_path(&bin)?;
    let fetch_hint = "run cargo xtask fetch moq-dev";
    if !bin.is_file() {
        return Err(format!(
            "pinned {} not found at {}; {fetch_hint}",
            pin.version,
            bin.display()
        ));
    }
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(&bin)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("{} --version timed out", bin.display()))?
    .map_err(|error| format!("{} --version: {error}", bin.display()))?;
    let reported = String::from_utf8_lossy(&output.stdout);
    if reported.trim() != pin.version {
        return Err(format!(
            "{} reports {:?}, the interop run is pinned to {}; {fetch_hint}",
            bin.display(),
            reported.trim(),
            pin.version
        ));
    }
    Ok(bin)
}

/// Loopback ports, allocated around every port the harness already plans,
/// with `MOQ_DEV_INTEROP_*` overrides.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DevPorts {
    /// moq-relay: QUIC/WebTransport on UDP and its HTTP API on TCP.
    pub(crate) relay: u16,
    /// Restream's MoQ listener (UDP): ingest and origin.
    pub(crate) restream_moq: u16,
    /// FFmpeg's RTMP listener reading the ingest pipeline's output back.
    pub(crate) readback_rtmp: u16,
    /// Vite serving the browser specs' page.
    pub(crate) page: u16,
    /// Dedicated IETF relay forwarding a source into Restream ingest.
    pub(crate) ingest_relay: u16,
}

impl DevPorts {
    pub(crate) fn allocate() -> Self {
        let mut reserved = planned_harness_ports();
        Self {
            relay: env_or_allocated_port("MOQ_DEV_INTEROP_RELAY", 4443, &mut reserved),
            restream_moq: env_or_allocated_port(
                "MOQ_DEV_INTEROP_RESTREAM_MOQ",
                4444,
                &mut reserved,
            ),
            readback_rtmp: env_or_allocated_port(
                "MOQ_DEV_INTEROP_READBACK_RTMP",
                1955,
                &mut reserved,
            ),
            page: env_or_allocated_port("MOQ_DEV_INTEROP_PAGE", 5180, &mut reserved),
            ingest_relay: env_or_allocated_port(
                "MOQ_DEV_INTEROP_INGEST_RELAY",
                4445,
                &mut reserved,
            ),
        }
    }

    pub(crate) fn to_json(self) -> Value {
        json!({
            "relay": self.relay,
            "restreamMoq": self.restream_moq,
            "readbackRtmp": self.readback_rtmp,
            "page": self.page,
            "ingestRelay": self.ingest_relay,
        })
    }
}

/// The moq-relay process. One log across restarts (with a marker line per
/// start), so the log shows every life of the relay and
/// [`relay_sessions`] can tell them apart.
pub(crate) struct DevRelay {
    bin: PathBuf,
    port: u16,
    tls: (PathBuf, PathBuf),
    log: PathBuf,
    child: Option<Child>,
    starts: u32,
    forward: Option<(String, PathBuf)>,
}

impl DevRelay {
    pub(crate) fn new(bin: PathBuf, port: u16, tls: &InteropTls, log: PathBuf) -> Self {
        Self {
            bin,
            port,
            tls: (tls.leaf_pem.clone(), tls.leaf_key.clone()),
            log,
            child: None,
            starts: 0,
            forward: None,
        }
    }

    pub(crate) fn log_path(&self) -> &Path {
        &self.log
    }

    /// The relay itself publishes into Restream, with no CLI media bridge.
    pub(crate) fn forward_to(&mut self, url: String, ca: &Path) {
        self.forward = Some((url, ca.to_path_buf()));
    }

    pub(crate) async fn start(&mut self) -> Result<(), String> {
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .map_err(|error| format!("open {}: {error}", self.log.display()))?;
        self.starts += 1;
        let _ = writeln!(log, "{RELAY_START_MARKER}{}", self.starts);
        let stderr = log.try_clone().map_err(|error| error.to_string())?;
        let listen = format!("127.0.0.1:{}", self.port);
        let mut command =
            command_with_optional_cgroup(&self.bin, &format!("moq-relay-{}", self.port));
        if let Some((url, ca)) = &self.forward {
            command
                .args(["--cluster-connect", url, "--connect-version", IETF_VERSION])
                .arg("--connect-tls-root")
                .arg(ca)
                .arg("--connect-websocket-enabled=false");
        }
        let child = command
            .args(["--log-level", "info", "--listen", &listen])
            .arg("--listen-tls-cert")
            .arg(&self.tls.0)
            .arg("--listen-tls-key")
            .arg(&self.tls.1)
            // Same number on TCP: `/announced/<prefix>` and `/fetch/<path>`.
            .args(["--web-http-listen", &listen, "--auth-public", "**"])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("spawn {}: {error}", self.bin.display()))?;
        self.child = Some(child);
        if let Err(error) = wait_for_tcp_listener_ready(self.port, Duration::from_secs(30)).await {
            self.kill().await;
            return Err(format!(
                "moq-relay did not become ready: {error}; log={}",
                self.log.display()
            ));
        }
        Ok(())
    }

    /// SIGKILL: the relay vanishes without closing its QUIC connections,
    /// which is the crash the isolation check is about.
    pub(crate) async fn kill(&mut self) {
        if let Some(mut child) = self.child.take()
            && let Err(error) = kill_and_wait_child(&mut child, "moq-relay").await
        {
            eprintln!("moq-dev-interop: {error}");
        }
    }

    /// `https://127.0.0.1:<port>/`: the URL clients connect with. Not
    /// `localhost`: Chromium's WebTransport to `localhost` fails against a
    /// listener on IPv4 loopback only (the leaf carries the IP SAN).
    pub(crate) fn connect_url(&self) -> String {
        format!("https://127.0.0.1:{}/", self.port)
    }

    async fn get_text(&self, path: &str) -> Result<Option<String>, String> {
        let url = format!("http://127.0.0.1:{}/{path}", self.port);
        let response = harness_http_client()
            .get(&url)
            .send()
            .await
            .map_err(|error| format!("GET {url}: {error}"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(format!("GET {url}: {}", response.status()));
        }
        response
            .text()
            .await
            .map(Some)
            .map_err(|error| format!("GET {url}: {error}"))
    }

    /// Wait until the relay announces `broadcast` and serves the selected catalog
    /// for it that passes the codec/container checks; returns that catalog. On
    /// timeout the error carries the last announce list and catalog seen.
    pub(crate) async fn wait_broadcast(
        &self,
        broadcast: &str,
        timeout: Duration,
        ietf: bool,
    ) -> Result<Value, String> {
        let (prefix, name) = broadcast.rsplit_once('/').unwrap_or(("", broadcast));
        let deadline = Instant::now() + timeout;
        let mut last = json!({});
        loop {
            let announced = self
                .get_text(&format!("announced/{prefix}"))
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            let listed = announced.lines().any(|line| line.trim() == name);
            last["announced"] = Value::String(announced);
            if listed {
                let track = if ietf { "catalog" } else { CATALOG_TRACK };
                let catalog = self
                    .get_text(&format!("fetch/{broadcast}/{track}"))
                    .await
                    .ok()
                    .flatten()
                    .and_then(|body| serde_json::from_str::<Value>(&body).ok());
                if let Some(catalog) = catalog {
                    if profile_catalog_failures(&catalog, ietf).is_empty() {
                        return Ok(catalog);
                    }
                    last["catalog"] = catalog;
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "moq-relay did not serve {broadcast} with an H.264 + AAC catalog (ietf={ietf}) within {}s; last={last}",
                    timeout.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

/// How the `moq` CLI reaches a MoQ endpoint: connect URL, the CA that signs
/// its leaf, and the broadcast path.
pub(crate) struct CliTarget<'a> {
    pub(crate) url: &'a str,
    pub(crate) ca: &'a Path,
    pub(crate) broadcast: &'a str,
    pub(crate) version: &'static str,
}

/// `moq` stage arguments: publish MPEG-TS from stdin.
pub(crate) const IMPORT_TS: &[&str] = &["import", "ts"];
/// `moq` stage arguments: subscribe and write MPEG-TS to stdout. The catalog
/// format is explicit because origin broadcast names carry no `.hang`.
pub(crate) const EXPORT_TS: &[&str] = &["export", "--catalog-format", "hang", "ts"];

impl CliTarget<'_> {
    /// Global flags plus the stage. Offer exactly one version, and disable
    /// WebSocket fallback so successful media requires QUIC/WebTransport.
    pub(crate) fn args(&self, stage: &[&str]) -> Vec<String> {
        let ca = self.ca.display().to_string();
        [
            "--log-level",
            "warn",
            "--connect",
            self.url,
            "--connect-tls-root",
            ca.as_str(),
            "--connect-version",
            self.version,
            "--connect-websocket-enabled=false",
            "--broadcast",
            self.broadcast,
        ]
        .iter()
        .chain(stage)
        .map(|arg| {
            if self.version.starts_with("moq-transport-") && *arg == "hang" {
                "msf".to_string()
            } else {
                arg.to_string()
            }
        })
        .collect()
    }
}

/// The publisher recipe of moq-dev's `demo/pub/justfile` (`pub-ts`), with
/// the fixture's first audio track only: FFmpeg remuxes the fixture to
/// MPEG-TS in real time on stdout and `moq import ts` publishes it.
pub(crate) fn ts_publisher_args(fixture: &Path) -> Vec<String> {
    let fixture = fixture.display().to_string();
    [
        "-stream_loop",
        "-1",
        "-re",
        "-i",
        fixture.as_str(),
        "-map",
        "0:v",
        "-map",
        "0:a:0",
        "-c",
        "copy",
        "-f",
        "mpegts",
        "-pes_payload_size",
        "0",
        "-muxdelay",
        "0",
        "-",
    ]
    .iter()
    .map(|arg| arg.to_string())
    .collect()
}

/// Spawn `ffmpeg … - | moq … import ts`; both children are returned so the
/// caller tears both down.
pub(crate) fn spawn_ts_import(
    ffmpeg: &Path,
    moq: &Path,
    fixture: &Path,
    target: &CliTarget<'_>,
    log_dir: &Path,
    label: &str,
) -> Result<[Child; 2], String> {
    let ffmpeg_log = log_file(&log_dir.join(format!("{label}-ffmpeg.log")))?;
    let mut source = command_with_optional_cgroup(ffmpeg, &format!("moq-{label}-ffmpeg"))
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args(ts_publisher_args(fixture))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(ffmpeg_log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", ffmpeg.display()))?;
    let stdout = source
        .stdout
        .take()
        .ok_or("ffmpeg stdout was not captured")?;
    let pipe = TryInto::<Stdio>::try_into(stdout)
        .map_err(|error| format!("ffmpeg stdout pipe: {error}"))?;
    let moq_log = log_file(&log_dir.join(format!("{label}-moq.log")))?;
    let moq_stderr = moq_log.try_clone().map_err(|error| error.to_string())?;
    let import = command_with_optional_cgroup(moq, &format!("moq-{label}-import"))
        .args(target.args(IMPORT_TS))
        .stdin(pipe)
        .stdout(Stdio::from(moq_log))
        .stderr(Stdio::from(moq_stderr))
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", moq.display()))?;
    Ok([source, import])
}

/// Subscribe with `moq export ts` for `secs`, writing the MPEG-TS it
/// delivers to `out`; returns the bytes captured.
pub(crate) async fn capture_export(
    moq: &Path,
    target: &CliTarget<'_>,
    out: &Path,
    secs: u64,
) -> Result<u64, String> {
    let file = log_file(out)?;
    let log = log_file(&out.with_extension("moq.log"))?;
    let mut child = command_with_optional_cgroup(moq, "moq-export")
        .args(target.args(EXPORT_TS))
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", moq.display()))?;
    capture_for(&mut child, secs, "moq export").await?;
    captured_bytes(out)
}

/// [`EXPORT_TS`] dialling once: a refused session ends the process instead
/// of redialling with backoff.
pub(crate) const EXPORT_TS_ONCE: &[&str] =
    &["--connect-once", "export", "--catalog-format", "hang", "ts"];

/// What one subscription the endpoint should refuse did.
pub(crate) struct Refusal {
    /// The exit status, if the CLI ended within the watch window.
    pub(crate) exited: Option<String>,
    pub(crate) bytes: u64,
    /// The CLI's last stderr lines (moq-cli 0.14.7 prints `Error: unauthorized`).
    pub(crate) reason: String,
}

impl Refusal {
    /// Refused: the session ended on its own and no media came through.
    pub(crate) fn refused(&self) -> bool {
        self.exited.is_some() && self.bytes == 0
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({"exited": self.exited, "bytes": self.bytes, "reason": self.reason})
    }
}

/// Dial once with `moq export ts` and watch for `secs`.
pub(crate) async fn attempt_export(
    moq: &Path,
    target: &CliTarget<'_>,
    out: &Path,
    secs: u64,
) -> Result<Refusal, String> {
    let file = log_file(out)?;
    let log_path = out.with_extension("moq.log");
    let log = log_file(&log_path)?;
    let mut child = command_with_optional_cgroup(moq, "moq-export-once")
        .args(target.args(EXPORT_TS_ONCE))
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", moq.display()))?;
    let exited = match tokio::time::timeout(Duration::from_secs(secs), child.wait()).await {
        Ok(status) => {
            Some(status.map_or_else(|error| error.to_string(), |status| status.to_string()))
        }
        Err(_) => {
            stop_child(&mut child).await;
            None
        }
    };
    let stderr = std::fs::read_to_string(&log_path).unwrap_or_default();
    let tail: Vec<&str> = stderr.lines().rev().take(3).collect();
    Ok(Refusal {
        exited,
        bytes: std::fs::metadata(out).map_or(0, |meta| meta.len()),
        reason: tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
    })
}

/// FFmpeg as a one-shot RTMP server: accepts the ingest pipeline's RTMP
/// output and remuxes it to `out` for `secs` of media.
pub(crate) fn spawn_rtmp_readback(
    ffmpeg: &Path,
    port: u16,
    out: &Path,
    secs: u64,
) -> Result<Child, String> {
    let log = log_file(&out.with_extension("ffmpeg.log"))?;
    let listen = format!("rtmp://127.0.0.1:{port}/{READBACK_RTMP_PATH}");
    command_with_optional_cgroup(ffmpeg, "moq-readback")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-listen",
            "1",
        ])
        .args([
            "-i",
            &listen,
            "-c",
            "copy",
            "-t",
            &secs.to_string(),
            "-f",
            "mpegts",
            "-y",
        ])
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", ffmpeg.display()))
}

/// Let `child` run `secs`, then stop it. Exiting earlier is a failure: a
/// subscriber that ends on its own lost its session.
async fn capture_for(child: &mut Child, secs: u64, label: &str) -> Result<(), String> {
    match tokio::time::timeout(Duration::from_secs(secs), child.wait()).await {
        Ok(status) => Err(format!(
            "{label} exited after less than {secs}s: {}",
            status.map_or_else(|error| error.to_string(), |status| status.to_string())
        )),
        Err(_) => {
            stop_child(child).await;
            Ok(())
        }
    }
}

pub(crate) fn captured_bytes(out: &Path) -> Result<u64, String> {
    let bytes = std::fs::metadata(out)
        .map_err(|error| format!("{}: {error}", out.display()))?
        .len();
    if bytes == 0 {
        return Err(format!("{} captured no media", out.display()));
    }
    Ok(bytes)
}

fn log_file(path: &Path) -> Result<std::fs::File, String> {
    std::fs::File::create(path).map_err(|error| format!("create {}: {error}", path.display()))
}
