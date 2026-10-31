//! The pinned MediaMTX relay: binary, per-run TLS material, ports, config,
//! and the process itself (start, kill, path lookups).

use super::*;

use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;

/// Path the egress output publishes to.
pub(crate) const EGRESS_PATH: &str = "live";
/// Path FFmpeg publishes into and MediaMTX forwards to Restream over MoQ.
pub(crate) const INGEST_SOURCE_PATH: &str = "ingest-src";
/// Path the ingest pipeline's RTMP output publishes back into MediaMTX.
pub(crate) const INGEST_RETURN_PATH: &str = "ingest-back";

/// Per-run TLS material: a CA and an ECDSA P-256 leaf it signs, valid for 13
/// days with SANs `localhost` and `127.0.0.1`. MediaMTX's native QUIC
/// listener and Restream's MoQ ingest listener both serve the leaf; Restream's
/// egress trusts the CA through `?ca=`; MediaMTX's forward pins the leaf by
/// its SHA-256.
pub(crate) struct InteropTls {
    pub(crate) ca_pem: PathBuf,
    pub(crate) leaf_pem: PathBuf,
    pub(crate) leaf_key: PathBuf,
    pub(crate) leaf_sha256: String,
}

const LEAF_EXTENSIONS: &str = "subjectAltName=DNS:localhost,IP:127.0.0.1\n\
basicConstraints=critical,CA:FALSE\n\
keyUsage=critical,digitalSignature\n\
extendedKeyUsage=serverAuth\n";

impl InteropTls {
    pub(crate) async fn generate(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        let dir = absolute_path(dir)?;
        let path = |name: &str| dir.join(name);
        std::fs::create_dir_all(dir.join("ca")).map_err(|error| error.to_string())?;
        let arg = |name: &str| path(name).display().to_string();
        let (ca_key, ca_pem) = (arg("ca.key"), arg("ca/ca.pem"));
        let (leaf_key, leaf_csr) = (arg("leaf.key"), arg("leaf.csr"));
        let (leaf_pem, leaf_ext) = (arg("leaf.pem"), arg("leaf.ext"));
        std::fs::write(&leaf_ext, LEAF_EXTENSIONS).map_err(|error| error.to_string())?;
        const EC_KEY: [&str; 5] = [
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:prime256v1",
            "-nodes",
        ];

        let mut ca: Vec<&str> = vec!["req", "-x509"];
        ca.extend(EC_KEY);
        ca.extend([
            "-keyout",
            ca_key.as_str(),
            "-out",
            ca_pem.as_str(),
            "-days",
            "13",
            "-subj",
            "/CN=restream moq interop CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ]);
        run_openssl(&ca).await?;

        let mut csr: Vec<&str> = vec!["req"];
        csr.extend(EC_KEY);
        csr.extend([
            "-keyout",
            leaf_key.as_str(),
            "-out",
            leaf_csr.as_str(),
            "-subj",
            "/CN=localhost",
        ]);
        run_openssl(&csr).await?;

        // A fresh positive serial per run; the CA is new every run too.
        let serial = format!(
            "0x{:016x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos() as u64)
                .unwrap_or(1)
        );
        run_openssl(&[
            "x509",
            "-req",
            "-in",
            leaf_csr.as_str(),
            "-CA",
            ca_pem.as_str(),
            "-CAkey",
            ca_key.as_str(),
            "-set_serial",
            serial.as_str(),
            "-days",
            "13",
            "-sha256",
            "-extfile",
            leaf_ext.as_str(),
            "-out",
            leaf_pem.as_str(),
        ])
        .await?;

        let leaf = CertificateDer::from_pem_file(path("leaf.pem"))
            .map_err(|error| format!("read generated leaf certificate: {error}"))?;
        Ok(Self {
            ca_pem: path("ca/ca.pem"),
            leaf_pem: path("leaf.pem"),
            leaf_key: path("leaf.key"),
            leaf_sha256: certificate_sha256_hex(leaf.as_ref()),
        })
    }
}

async fn run_openssl(args: &[&str]) -> Result<(), String> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        Command::new("openssl")
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("openssl {} timed out", args.first().unwrap_or(&"")))?
    .map_err(|error| format!("spawn openssl: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "openssl {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Loopback ports for MediaMTX and Restream's MoQ ingest listener.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RelayPorts {
    pub(crate) api: u16,
    pub(crate) rtmp: u16,
    pub(crate) rtsp: u16,
    /// MediaMTX's MoQ web page (TCP, HTTP/2) and WebTransport (UDP, HTTP/3).
    pub(crate) moq_http: u16,
    /// MediaMTX's native MoQ-over-QUIC listener (UDP).
    pub(crate) moq_quic: u16,
    /// Restream's MoQ ingest listener (UDP).
    pub(crate) restream_moq: u16,
}

/// Every port the harness has already planned, in bands at least as wide as
/// the ones `harness_port_defaults` hands out, so MoQ interop ports allocated
/// around them never collide with another harness listener.
pub(crate) fn planned_harness_ports() -> HashSet<u16> {
    let defaults = harness_port_defaults();
    let mut reserved = HashSet::new();
    for port in [
        defaults.restream_http,
        defaults.restream_rtmp,
        defaults.restream_srt,
        defaults.mtx_hls,
    ] {
        reserve_port_range(port, 1, &mut reserved);
    }
    for port in [
        defaults.mtx_rtmp,
        defaults.mtx_rtmps,
        defaults.mtx_srt,
        defaults.mtx_api,
    ] {
        reserve_port_range(port, PEER_PORT_STRIDE.saturating_mul(16), &mut reserved);
    }
    reserve_port_range(defaults.sink, 256, &mut reserved);
    reserve_port_range(defaults.hls_put, 16, &mut reserved);
    reserve_port_range(defaults.ffmpeg_srt_sink_base, 1024, &mut reserved);
    reserve_port_range(defaults.ffmpeg_signal_sink_base, 1024, &mut reserved);
    reserved
}

impl RelayPorts {
    /// MediaMTX's API and RTMP use the harness's own MediaMTX ports; the rest
    /// are allocated around every port the harness has already planned, with
    /// `MOQ_INTEROP_*` environment overrides.
    pub(crate) fn allocate() -> Self {
        let defaults = harness_port_defaults();
        let mut reserved = planned_harness_ports();
        Self {
            api: defaults.mtx_api,
            rtmp: defaults.mtx_rtmp,
            rtsp: env_or_allocated_port("MOQ_INTEROP_MTX_RTSP", 8554, &mut reserved),
            moq_http: env_or_allocated_port("MOQ_INTEROP_MTX_MOQ_HTTP", 8892, &mut reserved),
            moq_quic: env_or_allocated_port("MOQ_INTEROP_MTX_MOQ_QUIC", 8893, &mut reserved),
            restream_moq: env_or_allocated_port("MOQ_INTEROP_RESTREAM_MOQ", 4443, &mut reserved),
        }
    }

    pub(crate) fn to_json(self) -> Value {
        json!({
            "mtxApi": self.api,
            "mtxRtmp": self.rtmp,
            "mtxRtsp": self.rtsp,
            "mtxMoqHttp": self.moq_http,
            "mtxMoqQuic": self.moq_quic,
            "restreamMoq": self.restream_moq,
        })
    }
}

/// Where MediaMTX forwards [`INGEST_SOURCE_PATH`]: Restream's MoQ ingest
/// listener and the stream key the pipeline expects.
pub(crate) struct RelayForward<'a> {
    pub(crate) stream_key: &'a str,
    pub(crate) fingerprint: &'a str,
}

/// MediaMTX config for one run. Every listener the run does not use is off,
/// so nothing binds MediaMTX's default ports; RTSP is TCP-only for the same
/// reason (no RTP/RTCP UDP pair).
pub(crate) fn render_mediamtx_config(
    ports: RelayPorts,
    tls: &InteropTls,
    forward: &RelayForward<'_>,
) -> String {
    format!(
        "logLevel: info\n\
readTimeout: 10s\n\
writeTimeout: 10s\n\
api: yes\n\
apiAddress: 127.0.0.1:{api}\n\
metrics: no\n\
pprof: no\n\
playback: no\n\
rtsp: yes\n\
rtspTransports: [tcp]\n\
rtspAddress: 127.0.0.1:{rtsp}\n\
rtmp: yes\n\
rtmpAddress: 127.0.0.1:{rtmp}\n\
hls: no\n\
webrtc: no\n\
srt: no\n\
moq: yes\n\
moqHTTP2Address: 127.0.0.1:{moq_http}\n\
moqHTTP3Address: 127.0.0.1:{moq_http}\n\
moqQUICAddress: 127.0.0.1:{moq_quic}\n\
moqServerKey: {key}\n\
moqServerCert: {cert}\n\
pathDefaults:\n\
\x20 source: publisher\n\
paths:\n\
\x20 {ingest_source}:\n\
\x20   forward:\n\
\x20     - dest: moqt://127.0.0.1:{restream_moq}/{stream_key}\n\
\x20       destFingerprint: {fingerprint}\n\
\x20       moqTransport: quic\n\
\x20 all_others:\n",
        api = ports.api,
        rtsp = ports.rtsp,
        rtmp = ports.rtmp,
        moq_http = ports.moq_http,
        moq_quic = ports.moq_quic,
        key = tls.leaf_key.display(),
        cert = tls.leaf_pem.display(),
        ingest_source = INGEST_SOURCE_PATH,
        restream_moq = ports.restream_moq,
        stream_key = forward.stream_key,
        fingerprint = forward.fingerprint,
    )
}

/// The MediaMTX process. One config and one log across restarts, so the log
/// shows both lives of the relay.
pub(crate) struct Relay {
    bin: PathBuf,
    config: PathBuf,
    log: PathBuf,
    ports: RelayPorts,
    child: Option<Child>,
    starts: u32,
}

impl Relay {
    pub(crate) fn new(bin: PathBuf, config: PathBuf, log: PathBuf, ports: RelayPorts) -> Self {
        Self {
            bin,
            config,
            log,
            ports,
            child: None,
            starts: 0,
        }
    }

    pub(crate) async fn start(&mut self) -> Result<(), String> {
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .map_err(|error| format!("open {}: {error}", self.log.display()))?;
        self.starts += 1;
        let _ = writeln!(
            log,
            "---- moq-relay-interop: MediaMTX start #{}",
            self.starts
        );
        let stderr = log.try_clone().map_err(|error| error.to_string())?;
        let mut command =
            command_with_optional_cgroup(&self.bin, &format!("mediamtx-moq-{}", self.ports.api));
        let child = remove_mediamtx_config_env(&mut command)
            .arg(&self.config)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("spawn {}: {error}", self.bin.display()))?;
        self.child = Some(child);
        let ready = async {
            wait_for_http_ok(
                &format!("http://127.0.0.1:{}/v3/paths/list", self.ports.api),
                Duration::from_secs(30),
            )
            .await?;
            wait_for_tcp_listener_ready(self.ports.rtsp, Duration::from_secs(10)).await
        };
        if let Err(error) = ready.await {
            self.kill().await;
            return Err(format!(
                "MediaMTX did not become ready: {error}; log={}",
                self.log.display()
            ));
        }
        Ok(())
    }

    /// SIGKILL: the relay vanishes without closing its QUIC connections,
    /// which is the crash the isolation check is about.
    pub(crate) async fn kill(&mut self) {
        if let Some(mut child) = self.child.take()
            && let Err(error) = kill_and_wait_child(&mut child, "mediamtx").await
        {
            eprintln!("moq-relay-interop: {error}");
        }
    }

    /// `/v3/paths/get/<name>`, or `None` while the path does not exist.
    pub(crate) async fn path(&self, name: &str) -> Result<Option<Value>, String> {
        let url = format!("http://127.0.0.1:{}/v3/paths/get/{name}", self.ports.api);
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
        let body = response
            .bytes()
            .await
            .map_err(|error| format!("GET {url}: {error}"))?;
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|error| format!("GET {url}: {error}"))
    }

    /// Wait until `name` is published by a `source_type` source with both
    /// tracks; on timeout the error carries the last path body seen.
    pub(crate) async fn wait_path_published(
        &self,
        name: &str,
        source_type: &str,
        timeout: Duration,
    ) -> Result<Value, String> {
        let deadline = Instant::now() + timeout;
        let mut last = Value::Null;
        loop {
            if let Ok(Some(path)) = self.path(name).await {
                if relay_path_published(&path, source_type) {
                    return Ok(path);
                }
                last = path;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "MediaMTX path {name} not published by {source_type} with H264 and AAC within {}s; last={last}",
                    timeout.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub(crate) fn rtsp_url(&self, path: &str) -> String {
        format!("rtsp://127.0.0.1:{}/{path}", self.ports.rtsp)
    }

    pub(crate) fn rtmp_url(&self, path: &str) -> String {
        format!("rtmp://127.0.0.1:{}/{path}", self.ports.rtmp)
    }

    pub(crate) fn moq_url(&self, path: &str, ca: &Path) -> String {
        format!(
            "moqt://127.0.0.1:{}/{path}?ca={}",
            self.ports.moq_quic,
            ca.file_name().expect("CA file name").to_string_lossy()
        )
    }
}
