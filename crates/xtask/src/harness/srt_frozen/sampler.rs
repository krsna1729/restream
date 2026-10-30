//! 1 s sampler of the Restream process under `fault.srt-output-stall`: /proc
//! facts plus SRT shard and output rows from the loopback engine API.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Map, Value, json};

use super::{now, truthy};

const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE: u64 = 64 << 20;

/// What one sampling thread observes; `Engine` in production.
pub(super) trait Probe: Send + 'static {
    /// Finds and authenticates against the sampled process.
    fn discover(&mut self) -> bool;
    /// Fills one record; on error the partial record is kept.
    fn record(&mut self, record: &mut Map<String, Value>) -> Result<(), String>;
    fn process_exists(&self) -> bool;
}

#[derive(Debug, Default)]
pub(super) struct SamplerOutcome {
    pub(super) process_gone_at: Option<f64>,
    pub(super) finished_at: Option<f64>,
    pub(super) failure: Option<String>,
    pub(super) stop_requested_at: f64,
    pub(super) joined: bool,
}

impl SamplerOutcome {
    pub(super) fn to_json(&self) -> Value {
        json!({
            "process_gone_at": self.process_gone_at,
            "finished_at": self.finished_at,
            "failure": self.failure,
            "stop_requested_at": self.stop_requested_at,
            "joined": self.joined,
        })
    }
}

struct Finished {
    process_gone_at: Option<f64>,
    finished_at: f64,
    failure: Option<String>,
}

/// A sampling thread; dropping `stop` (in `finish`) ends it.
pub(super) struct Sampler {
    stop: Sender<()>,
    handle: JoinHandle<Finished>,
}

impl Sampler {
    pub(super) fn start(
        mut probe: impl Probe,
        out: PathBuf,
        interval: Duration,
    ) -> io::Result<Self> {
        let (stop, stopped) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("srt-frozen-sampler".into())
            .spawn(move || {
                let (process_gone_at, failure) = match sample(&mut probe, &out, interval, &stopped)
                {
                    Ok(gone) => (gone, None),
                    Err(error) => (None, Some(error)),
                };
                Finished {
                    process_gone_at,
                    finished_at: now(),
                    failure,
                }
            })?;
        Ok(Self { stop, handle })
    }

    #[cfg(test)]
    pub(super) fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Requests a stop and joins for at most `limit`; a sampler still busy
    /// after that is reported as not joined.
    pub(super) fn finish(self, stop_requested_at: f64, limit: Duration) -> SamplerOutcome {
        let Self { stop, handle } = self;
        drop(stop);
        let until = Instant::now() + limit;
        while !handle.is_finished() && Instant::now() < until {
            thread::sleep(Duration::from_millis(20));
        }
        let mut outcome = SamplerOutcome {
            stop_requested_at,
            ..SamplerOutcome::default()
        };
        if !handle.is_finished() {
            return outcome;
        }
        outcome.joined = true;
        match handle.join() {
            Ok(finished) => {
                outcome.process_gone_at = finished.process_gone_at;
                outcome.finished_at = Some(finished.finished_at);
                outcome.failure = finished.failure;
            }
            Err(_) => outcome.failure = Some("sampler panicked".into()),
        }
        outcome
    }
}

fn stopped(stop: &Receiver<()>) -> bool {
    !matches!(stop.try_recv(), Err(TryRecvError::Empty))
}

/// Returns when the stop is requested (`None`) or the process disappears
/// (`Some(time)`); I/O failures are the sampler's own failure.
fn sample(
    probe: &mut impl Probe,
    out: &Path,
    interval: Duration,
    stop: &Receiver<()>,
) -> Result<Option<f64>, String> {
    while !stopped(stop) && !probe.discover() {
        if !matches!(
            stop.recv_timeout(Duration::from_millis(250)),
            Err(RecvTimeoutError::Timeout)
        ) {
            break;
        }
    }
    let mut file = File::create(out).map_err(|error| format!("{}: {error}", out.display()))?;
    while !stopped(stop) {
        let started = Instant::now();
        let mut record = Map::new();
        record.insert("t".into(), json!(now()));
        if let Err(error) = probe.record(&mut record) {
            // The process may be shutting down. `t` is when the sample began;
            // `errorAt` is when it failed, which can fall after the
            // harness's capture-complete marker.
            record.insert("error".into(), json!(error));
            record.insert("errorAt".into(), json!(now()));
            if !probe.process_exists() {
                return Ok(Some(now()));
            }
        }
        writeln!(file, "{}", Value::Object(record))
            .map_err(|error| format!("{}: {error}", out.display()))?;
        let _ = stop.recv_timeout(interval.saturating_sub(started.elapsed()));
    }
    Ok(None)
}

/// The Restream process found through its log's `http_port` and the
/// listening socket's owner.
pub(super) struct Engine {
    log: PathBuf,
    password: String,
    attribution: bool,
    port: u16,
    pid: Option<u32>,
    cookie: Option<String>,
}

impl Engine {
    pub(super) fn new(log: PathBuf, password: String, attribution: bool) -> Self {
        Self {
            log,
            password,
            attribution,
            port: 0,
            pid: None,
            cookie: None,
        }
    }

    fn request(&mut self, path: &str, body: Option<&[u8]>) -> Result<Value, String> {
        let raw = http_exchange(self.port, path, self.cookie.as_deref(), body)
            .map_err(|error| format!("{path}: {error}"))?;
        let response = parse_response(&raw).map_err(|error| format!("{path}: {error}"))?;
        if body.is_some()
            && let Some(cookie) = response.cookie
        {
            self.cookie = Some(cookie);
        }
        if response.body.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&response.body).map_err(|error| error.to_string())
    }

    fn get(&mut self, path: &str) -> Result<Value, String> {
        self.request(path, None)
    }

    fn login(&mut self) -> Result<(), String> {
        let body = json!({ "password": self.password }).to_string();
        self.request("/api/v1/auth/login", Some(body.as_bytes()))
            .map(drop)
    }

    /// Read-only census; missing evidence stays explicit, never becomes zero.
    fn attribution_facts(&mut self, pid: u32) -> Value {
        let base = PathBuf::from(format!("/proc/{pid}"));
        let mut facts = Map::new();
        for name in ["smaps_rollup", "smaps"] {
            match fs::read_to_string(base.join(name)) {
                Ok(text) => facts.insert(name.into(), json!(text)),
                Err(error) => facts.insert(format!("{name}_error"), json!(error.to_string())),
            };
        }
        if let Err(error) = census(&base, &mut facts) {
            facts.insert("census_error".into(), json!(error.to_string()));
        }
        match self.get("/api/v1/engine/telemetry") {
            Ok(telemetry) => facts.insert("telemetry".into(), telemetry),
            Err(error) => facts.insert("telemetry_error".into(), json!(error)),
        };
        Value::Object(facts)
    }
}

impl Probe for Engine {
    fn discover(&mut self) -> bool {
        let Ok(log) = fs::read(&self.log) else {
            return false;
        };
        let Some(port) = log_http_port(&String::from_utf8_lossy(&log)) else {
            return false;
        };
        self.port = port;
        let Some(pid) = listening_pid(port) else {
            return false;
        };
        self.pid = Some(pid);
        if self.login().is_err() {
            self.pid = None;
            return false;
        }
        true
    }

    fn record(&mut self, record: &mut Map<String, Value>) -> Result<(), String> {
        let pid = self.pid.ok_or("sampled process not discovered")?;
        record.insert("proc".into(), proc_sample(pid)?);
        if self.attribution {
            let facts = self.attribution_facts(pid);
            record.insert("attribution".into(), facts);
        }
        let system = self.get("/metrics/system")?;
        let shards: Vec<&Value> = system
            .get("egressShards")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|shard| shard.get("protocol").and_then(Value::as_str) == Some("srt"))
            .collect();
        record.insert("shards".into(), json!(shards));
        let health = self.get("/api/v1/engine/health")?;
        record.insert("outputs".into(), output_rows(&health));
        Ok(())
    }

    fn process_exists(&self) -> bool {
        self.pid
            .is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists())
    }
}

fn census(base: &Path, facts: &mut Map<String, Value>) -> io::Result<()> {
    let mut threads = Map::new();
    for entry in fs::read_dir(base.join("task"))? {
        let entry = entry?;
        let comm = fs::read_to_string(entry.path().join("comm"))?;
        threads.insert(
            entry.file_name().to_string_lossy().into_owned(),
            json!(comm.trim()),
        );
    }
    facts.insert("threads".into(), Value::Object(threads));
    let mut sockets = Map::new();
    for entry in fs::read_dir(base.join("fd"))? {
        let entry = entry?;
        let link = fs::read_link(entry.path())?;
        let link = link.to_string_lossy();
        if link.starts_with("socket:") {
            sockets.insert(
                entry.file_name().to_string_lossy().into_owned(),
                json!(link),
            );
        }
    }
    facts.insert("socket_fds".into(), Value::Object(sockets));
    for name in ["udp", "udp6"] {
        let table = fs::read_to_string(base.join("net").join(name))?;
        facts.insert(name.into(), json!(table));
    }
    Ok(())
}

/// The first `http_port` number in the log, ANSI colour removed.
pub(super) fn log_http_port(log: &str) -> Option<u16> {
    static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").unwrap());
    static PORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"http_port\D*(\d+)").unwrap());
    let plain = ANSI.replace_all(log, "");
    PORT.captures(&plain)?.get(1)?.as_str().parse().ok()
}

/// Socket inodes listening on `port` in a `/proc/net/tcp{,6}` table.
pub(super) fn listening_inodes(table: &str, port: u16) -> Vec<u64> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let (_, local_port) = fields.get(1)?.rsplit_once(':')?;
            let listening = *fields.get(3)? == "0A";
            (listening && u16::from_str_radix(local_port, 16).ok()? == port)
                .then(|| fields.get(9)?.parse().ok())?
        })
        .filter(|inode| *inode != 0)
        .collect()
}

/// The lowest PID holding a TCP socket that listens on `port` (`ss -ltnp`).
fn listening_pid(port: u16) -> Option<u32> {
    let inodes: Vec<String> = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|table| fs::read_to_string(table).ok())
        .flat_map(|table| listening_inodes(&table, port))
        .map(|inode| format!("socket:[{inode}]"))
        .collect();
    if inodes.is_empty() {
        return None;
    }
    let mut pids: Vec<u32> = fs::read_dir("/proc")
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    pids.sort_unstable();
    pids.into_iter().find(|pid| {
        fs::read_dir(format!("/proc/{pid}/fd"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|fd| {
                fs::read_link(fd.path()).is_ok_and(|link| {
                    inodes
                        .iter()
                        .any(|inode| link.as_os_str() == inode.as_str())
                })
            })
    })
}

fn proc_sample(pid: u32) -> Result<Value, String> {
    let base = PathBuf::from(format!("/proc/{pid}"));
    let read = |name: &str| fs::read_to_string(base.join(name)).map_err(|e| e.to_string());
    let status = read("status")?;
    let stat = read("stat")?;
    let pss = read("smaps_rollup")
        .ok()
        .and_then(|rollup| field_kb(&rollup, "Pss"));
    let rss = field_kb(&status, "VmRSS").ok_or("VmRSS unavailable")?;
    let threads = field_kb(&status, "Threads").ok_or("Threads unavailable")?;
    let fds = fs::read_dir(base.join("fd"))
        .map_err(|e| e.to_string())?
        .count();
    let cpu: Vec<&str> = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.split_whitespace().collect())
        .unwrap_or_default();
    let jiffies = |index: usize| cpu.get(index)?.parse::<u64>().ok();
    let cpu_jiffies = jiffies(11)
        .zip(jiffies(12))
        .map(|(user, system)| user + system)
        .ok_or("cpu jiffies unavailable")?;
    Ok(json!({
        "rss_kb": rss,
        "pss_kb": pss,
        "threads": threads,
        "fds": fds,
        "cpu_jiffies": cpu_jiffies,
    }))
}

/// The first number of a `Name:  value` line.
fn field_kb(text: &str, name: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        line.strip_prefix(name)?
            .strip_prefix(':')?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

/// Per-output progress rows from `/api/v1/engine/health`.
fn output_rows(health: &Value) -> Value {
    let mut rows = Map::new();
    let pipelines = health.get("pipelines").and_then(Value::as_object);
    for pipeline in pipelines.into_iter().flat_map(Map::values) {
        let outputs = pipeline.get("outputs").and_then(Value::as_object);
        for (id, output) in outputs.into_iter().flatten() {
            let first = |values: [Option<&Value>; 2]| {
                values
                    .into_iter()
                    .find(|value| truthy(*value))
                    .flatten()
                    .cloned()
                    .unwrap_or(json!(0))
            };
            let metrics_bytes = output.get("metrics").and_then(|m| m.get("bytesOut"));
            rows.insert(
                id.clone(),
                json!({
                    "bytesOut": first([output.get("bytesOut"), metrics_bytes]),
                    "phase": output.get("phase"),
                    "status": output.get("status"),
                    "retryAttempts": first([output.get("retryAttempts"), None]),
                    "backpressureReason": output.get("backpressureReason"),
                    "feedLagUnits": output.get("feedLagUnits"),
                }),
            );
        }
    }
    Value::Object(rows)
}

/// One HTTP/1.1 exchange with the loopback engine: no proxy, no redirects,
/// 5 s connect/read/write timeouts.
fn http_exchange(
    port: u16,
    path: &str,
    cookie: Option<&str>,
    body: Option<&[u8]>,
) -> io::Result<Vec<u8>> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, HTTP_TIMEOUT)?;
    stream.set_read_timeout(Some(HTTP_TIMEOUT))?;
    stream.set_write_timeout(Some(HTTP_TIMEOUT))?;
    let method = if body.is_some() { "POST" } else { "GET" };
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nAccept: application/json\r\n"
    );
    if let Some(cookie) = cookie {
        head.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    if let Some(body) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if let Some(body) = body {
        stream.write_all(body)?;
    }
    let mut response = Vec::new();
    let mut reader = stream.take(MAX_RESPONSE);
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
        // Stop at a complete Content-Length body even if the server keeps
        // the connection open.
        if let Some((end, Some(length))) = header_end(&response).map(|end| {
            let head = String::from_utf8_lossy(&response[..end]);
            (
                end,
                header(&head, "content-length").and_then(|v| v.parse::<usize>().ok()),
            )
        }) && response.len() >= end + 4 + length
        {
            break;
        }
    }
    Ok(response)
}

fn header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

/// The first value of a header, case-insensitive.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.split("\r\n").skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

pub(super) struct Response {
    pub(super) cookie: Option<String>,
    pub(super) body: Vec<u8>,
}

/// A 2xx response's first `Set-Cookie` pair and decoded body.
pub(super) fn parse_response(raw: &[u8]) -> Result<Response, String> {
    let end = header_end(raw).ok_or("truncated HTTP response")?;
    let head = std::str::from_utf8(&raw[..end]).map_err(|_| "non-UTF-8 HTTP headers")?;
    let status_line = head.split("\r\n").next().unwrap_or_default();
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("bad HTTP status line: {status_line}"))?;
    if !(200..300).contains(&code) {
        return Err(format!("HTTP error: {status_line}"));
    }
    let cookie = header(head, "set-cookie")
        .and_then(|value| value.split(';').next())
        .map(str::to_owned);
    let body = &raw[end + 4..];
    let chunked = header(head, "transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"));
    let body = if chunked {
        dechunk(body)?
    } else if let Some(length) = header(head, "content-length") {
        let length: usize = length
            .parse()
            .map_err(|_| format!("bad Content-Length: {length}"))?;
        body.get(..length).ok_or("truncated HTTP body")?.to_vec()
    } else {
        body.to_vec()
    };
    Ok(Response { cookie, body })
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoded = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or("truncated chunked body")?;
        let size = std::str::from_utf8(&body[..line_end])
            .ok()
            .and_then(|line| line.split(';').next())
            .and_then(|size| usize::from_str_radix(size.trim(), 16).ok())
            .ok_or("bad chunk size")?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(decoded);
        }
        let data = body.get(..size).ok_or("truncated chunk")?;
        decoded.extend_from_slice(data);
        body = body
            .get(size..)
            .and_then(|rest| rest.strip_prefix(b"\r\n"))
            .ok_or("missing chunk terminator")?;
    }
}
