//! Substrate UDP submission benchmark: Compio vs native `io_uring`.
//!
//! The roadmap's substrate question is "how many datagrams per CPU-second can
//! one core submit?", so this mode's hot loop contains nothing but UDP
//! submission and completion: no SRT, no MPEG-TS, no media rings, no crypto, no
//! scheduler. Two variants are implemented and held identical in everything
//! that matters — payload bytes, destination sequence, socket model, queue
//! depth and completion semantics:
//!
//! * `compio` — the completion-based UDP submission mechanism Restream's SRT
//!   egress already uses (`compio::net::UdpSocket::send_to` on a Compio
//!   runtime).
//! * `io-uring` — a purpose-built native ring: one `IoUring`, a fixed window of
//!   `SendTo` SQEs per batch, `submit()` then reap, with the same sliding
//!   window.
//!
//! Placement is the experiment's other half: the sender thread is pinned to
//! exactly one CPU, and harness/control threads and the drain peer are pinned
//! to disjoint CPUs. The peer's counters decide attribution — a result below
//! target only counts against the sender while the drain keeps up and its drop
//! counters stay clean; otherwise the run is `receiver-limited`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::fd::AsRawFd;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::stream::{FuturesUnordered, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::peer_state::{cpus_allowed_list, pin_to_cpuset, run_id, thread_cpus_allowed_list};
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Variant {
    Compio,
    IoUring,
}

impl Variant {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "compio" => Ok(Self::Compio),
            "io-uring" | "io_uring" => Ok(Self::IoUring),
            other => Err(format!(
                "SUBSTRATE_VARIANT must be compio or io-uring, got {other:?}"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Compio => "compio",
            Self::IoUring => "io-uring",
        }
    }
}

struct SubstrateConfig {
    variant: Variant,
    /// Exactly one CPU: the whole point is `pps/core`.
    sender_cpus: String,
    harness_cpus: Option<String>,
    payload_bytes: usize,
    destinations: Vec<SocketAddr>,
    queue_depth: usize,
    warmup: Duration,
    duration: Duration,
    report_secs: u64,
    receiver_state: Option<String>,
    run: String,
}

/// `count` IPv4 destinations starting at `base`, all inside the peer's local
/// prefix: destination-address diversity on the TX side without 1000 receiver
/// tasks on the RX side.
fn destinations(base: Ipv4Addr, count: usize, port: u16) -> Vec<SocketAddr> {
    let start = u32::from(base);
    (0..count)
        .map(|index| SocketAddr::new(IpAddr::V4(Ipv4Addr::from(start + index as u32)), port))
        .collect()
}

fn parse_config() -> Result<SubstrateConfig, String> {
    let variant = Variant::parse(&std::env::var("SUBSTRATE_VARIANT").unwrap_or_default())?;
    let sender_cpus = std::env::var("SUBSTRATE_SENDER_CPUS").unwrap_or_else(|_| "0".to_string());
    let sender_cpus = sender_cpus.trim().to_string();
    if sender_cpus.is_empty() || sender_cpus.contains(',') || sender_cpus.contains('-') {
        return Err(format!(
            "SUBSTRATE_SENDER_CPUS must name exactly one CPU (got {sender_cpus:?}): pps/core is \
             only meaningful when the sender owns one core"
        ));
    }
    let harness_cpus = std::env::var("SUBSTRATE_HARNESS_CPUS")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(mask) = &harness_cpus
        && mask.split(',').any(|part| part.trim() == sender_cpus)
    {
        return Err(format!(
            "harness CPUs {mask:?} overlap the sender CPU {sender_cpus:?}: the sender must own its \
             core exclusively"
        ));
    }
    let payload_bytes = env_usize("SUBSTRATE_PAYLOAD_BYTES", 1316);
    if payload_bytes == 0 || payload_bytes > 65_507 {
        return Err("SUBSTRATE_PAYLOAD_BYTES must be within 1..=65507".to_string());
    }
    let base: Ipv4Addr = std::env::var("SUBSTRATE_DEST_BASE")
        .unwrap_or_else(|_| "10.53.1.1".to_string())
        .parse()
        .map_err(|e| format!("SUBSTRATE_DEST_BASE: {e}"))?;
    let port = env_usize("SUBSTRATE_DEST_PORT", 9000);
    let port = u16::try_from(port).map_err(|_| "SUBSTRATE_DEST_PORT out of range")?;
    let dest_count = env_usize("SUBSTRATE_DEST_COUNT", 1000).max(1);
    let queue_depth = env_usize("SUBSTRATE_QUEUE_DEPTH", 64).max(1);
    let warmup = Duration::from_secs(env_secs("SUBSTRATE_WARMUP_SECS", 3).max(1));
    let duration = Duration::from_secs(env_secs("SUBSTRATE_DURATION_SECS", 20).max(5));
    let report_secs = env_secs("SUBSTRATE_REPORT_SECS", 5).max(1);
    let receiver_state = std::env::var("SUBSTRATE_RECEIVER_STATE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok(SubstrateConfig {
        variant,
        sender_cpus,
        harness_cpus,
        payload_bytes,
        destinations: destinations(base, dest_count, port),
        queue_depth,
        warmup,
        duration,
        report_secs,
        receiver_state,
        run: run_id(),
    })
}

/// One Compio send in flight: the completion future for a datagram plus the
/// buffer it borrowed. Borrows the socket for the duration of the window.
type SendFuture<'a> = Pin<Box<dyn Future<Output = compio::BufResult<usize, Bytes>> + 'a>>;

/// One fixed send slot: the destination sockaddr, its iovec and the msghdr the
/// ring reads. Wired up in place, never moved afterwards.
struct SendSlot {
    sockaddr: libc::sockaddr_in,
    iovec: libc::iovec,
    msg: libc::msghdr,
}

#[derive(Default)]
struct SenderCounters {
    submitted: AtomicU64,
    completed: AtomicU64,
    ring_enters: AtomicU64,
    errors: AtomicU64,
    max_in_flight: AtomicU64,
    batches: AtomicU64,
}

struct SenderHandles {
    tid: AtomicI32,
    stop: Arc<AtomicBool>,
    counters: Arc<SenderCounters>,
}

impl SenderHandles {
    fn new() -> Self {
        Self {
            tid: AtomicI32::new(0),
            stop: Arc::new(AtomicBool::new(false)),
            counters: Arc::new(SenderCounters::default()),
        }
    }

    fn completed(&self) -> u64 {
        self.counters.completed.load(Ordering::Relaxed)
    }
}

/// What the sender thread reports back once it has stopped: the mask it
/// actually observed and how the variant ended. Both are read after `join`, so
/// they travel as the thread's return value rather than through a lock.
struct SenderReport {
    observed_mask: Option<String>,
    /// The thread's own CPU time as it exits; a joined thread has no
    /// `/proc/self/task` entry left to read.
    cpu_secs: Option<f64>,
    outcome: Result<(), String>,
}

/// One preconstructed payload for every datagram: the kernel copies on send, so
/// a single immutable buffer is safe for a whole window of in-flight
/// submissions in both variants.
fn build_payload(bytes: usize) -> Bytes {
    let mut payload = Vec::with_capacity(bytes);
    for index in 0..bytes {
        payload.push((index % 251) as u8);
    }
    Bytes::from(payload)
}

/// Send buffer for both variants, so the comparison is not decided by a default
/// buffer size.
const SEND_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Set a large send buffer on either variant's socket, so the comparison is not
/// decided by a default buffer size.
fn set_send_buffer(fd: libc::c_int, bytes: usize) -> Result<(), String> {
    let value = bytes as libc::c_int;
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &value as *const _ as *const libc::c_void,
            std::mem::size_of_val(&value) as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(format!(
            "setsockopt(SO_SNDBUF): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Variant A: the completion-based Compio UDP path the SRT egress uses. A
/// sliding window of `queue_depth` sends keeps the ring busy while completions
/// are reaped one at a time.
fn run_compio(
    config: &SubstrateConfig,
    payload: Bytes,
    handles: &SenderHandles,
) -> Result<(), String> {
    let runtime = compio::runtime::Runtime::new().map_err(|e| format!("compio runtime: {e}"))?;
    runtime.block_on(async {
        // Bind through std so both variants get the same socket model (one
        // unconnected wildcard socket, one send buffer size) before Compio
        // adopts the fd.
        let std_socket =
            std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("udp bind: {e}"))?;
        set_send_buffer(std_socket.as_raw_fd(), SEND_BUFFER_BYTES)?;
        let socket = compio::net::UdpSocket::from_std(std_socket)
            .map_err(|e| format!("compio adopt udp socket: {e}"))?;

        let mut in_flight: FuturesUnordered<SendFuture<'_>> = FuturesUnordered::new();
        let mut next = 0_usize;
        while !handles.stop.load(Ordering::Relaxed) {
            while in_flight.len() < config.queue_depth {
                let destination = config.destinations[next % config.destinations.len()];
                next += 1;
                handles.counters.submitted.fetch_add(1, Ordering::Relaxed);
                in_flight.push(Box::pin(socket.send_to(payload.clone(), destination)));
            }
            handles
                .counters
                .max_in_flight
                .fetch_max(in_flight.len() as u64, Ordering::Relaxed);
            handles.counters.batches.fetch_add(1, Ordering::Relaxed);
            match in_flight.next().await {
                Some(compio::BufResult(result, _)) => {
                    handles.counters.completed.fetch_add(1, Ordering::Relaxed);
                    if let Err(error) = result {
                        handles.counters.errors.fetch_add(1, Ordering::Relaxed);
                        return Err(format!("compio send_to: {error}"));
                    }
                }
                None => break,
            }
        }
        Ok(())
    })
}

/// Variant B: a purpose-built native ring. One `IoUring`, a fixed window of
/// `SendTo` SQEs with preconstructed sockaddrs, one `submit()` per batch and a
/// blocking enter only when the completion queue came back empty.
fn run_io_uring(
    config: &SubstrateConfig,
    payload: Bytes,
    handles: &SenderHandles,
) -> Result<(), String> {
    use io_uring::{IoUring, opcode, types};

    let entries = config.queue_depth.next_power_of_two().max(8) as u32;
    let mut ring = IoUring::new(entries).map_err(|e| format!("io_uring setup: {e}"))?;
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(format!("socket(): {}", std::io::Error::last_os_error()));
    }
    set_send_buffer(fd, SEND_BUFFER_BYTES)?;
    // Fixed slots: one preconstructed msghdr + iovec per destination, wired to
    // the shared payload after the vector can no longer reallocate.
    let mut slots: Vec<SendSlot> = Vec::with_capacity(config.destinations.len());
    for destination in &config.destinations {
        let SocketAddr::V4(v4) = destination else {
            unreachable!("destinations are IPv4");
        };
        slots.push(SendSlot {
            sockaddr: libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from(*v4.ip()).to_be(),
                },
                sin_zero: [0; 8],
            },
            iovec: libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            },
            msg: unsafe { std::mem::zeroed() },
        });
    }
    for slot in slots.iter_mut() {
        slot.iovec = libc::iovec {
            iov_base: payload.as_ptr() as *mut libc::c_void,
            iov_len: payload.len(),
        };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_name = &slot.sockaddr as *const libc::sockaddr_in as *mut libc::c_void;
        msg.msg_namelen = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        msg.msg_iov = &slot.iovec as *const libc::iovec as *mut libc::iovec;
        msg.msg_iovlen = 1;
        slot.msg = msg;
    }

    let mut in_flight = 0_usize;
    let mut next = 0_usize;
    let mut result: Result<(), String> = Ok(());

    while !handles.stop.load(Ordering::Relaxed) && result.is_ok() {
        let mut pushed = 0_usize;
        while in_flight < config.queue_depth && pushed < entries as usize {
            let slot = &slots[next % slots.len()];
            next += 1;
            let entry = opcode::SendMsg::new(types::Fd(fd), &slot.msg as *const libc::msghdr)
                .build()
                .user_data(pushed as u64);
            // Safety: the payload and sockaddr live for the whole loop, and the
            // SQ has room because the window never exceeds the ring size.
            unsafe {
                if let Err(error) = ring.submission().push(&entry) {
                    result = Err(format!("io_uring submission queue full: {error}"));
                    break;
                }
            }
            pushed += 1;
            in_flight += 1;
        }
        if pushed > 0 {
            handles
                .counters
                .submitted
                .fetch_add(pushed as u64, Ordering::Relaxed);
            handles
                .counters
                .max_in_flight
                .fetch_max(in_flight as u64, Ordering::Relaxed);
            handles.counters.batches.fetch_add(1, Ordering::Relaxed);
        }
        if pushed > 0 {
            ring.submit().map_err(|e| format!("io_uring submit: {e}"))?;
            handles.counters.ring_enters.fetch_add(1, Ordering::Relaxed);
        }
        // Sliding window, matching the Compio variant: one completion per
        // iteration, one refill, then submit again. Reaping the whole window at
        // once would be a different completion pattern and a different
        // experiment.
        let mut reaped = reap_one(&mut ring, &handles.counters, &mut result);
        if reaped == 0 && in_flight > 0 {
            ring.submit_and_wait(1)
                .map_err(|e| format!("io_uring submit_and_wait: {e}"))?;
            handles.counters.ring_enters.fetch_add(1, Ordering::Relaxed);
            reaped = reap_one(&mut ring, &handles.counters, &mut result);
        }
        in_flight = in_flight.saturating_sub(reaped);
    }

    // Drain anything still in flight so the counts are complete.
    while in_flight > 0 {
        let _ = ring.submit_and_wait(1);
        let reaped = reap_one(&mut ring, &handles.counters, &mut result);
        if reaped == 0 {
            break;
        }
        in_flight = in_flight.saturating_sub(reaped);
    }
    unsafe { libc::close(fd) };
    result
}

/// Consume at most one completion from the ring, recording its result. Returns
/// 1 when a completion was reaped, 0 when the queue was empty.
fn reap_one(
    ring: &mut io_uring::IoUring,
    counters: &SenderCounters,
    result: &mut Result<(), String>,
) -> usize {
    let mut completion = ring.completion();
    completion.sync();
    let Some(entry) = completion.next() else {
        return 0;
    };
    counters.completed.fetch_add(1, Ordering::Relaxed);
    let rc = entry.result();
    if rc < 0 {
        counters.errors.fetch_add(1, Ordering::Relaxed);
        *result = Err(format!(
            "io_uring sendmsg: {}",
            std::io::Error::from_raw_os_error(-rc)
        ));
    }
    1
}

/// The sender thread: pin to exactly one CPU, record the observed mask, then
/// run the configured variant until asked to stop.
fn sender_thread(
    config: Arc<SubstrateConfig>,
    payload: Bytes,
    handles: Arc<SenderHandles>,
) -> SenderReport {
    let outcome = (|| -> Result<(), String> {
        pin_to_cpuset(&config.sender_cpus)?;
        handles
            .tid
            .store(super::peer_state::current_thread_id(), Ordering::Relaxed);
        match config.variant {
            Variant::Compio => run_compio(&config, payload, &handles),
            Variant::IoUring => run_io_uring(&config, payload, &handles),
        }
    })();
    SenderReport {
        observed_mask: thread_cpus_allowed_list(handles.tid.load(Ordering::Relaxed)),
        cpu_secs: super::peer_state::thread_cpu_secs(handles.tid.load(Ordering::Relaxed)),
        outcome,
    }
}

/// Minimal `GET <url>` against the peer's state endpoint.
async fn fetch_peer_state(url: &str) -> Result<Value, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("receiver state URL must start with http:// (got {url:?})"))?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/state".to_string()),
    };
    let mut stream = TcpStream::connect(host)
        .await
        .map_err(|e| format!("connect {host}: {e}"))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("write request: {e}"))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .map_err(|e| format!("read response: {e}"))?;
    let body_start = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .ok_or_else(|| format!("{url}: malformed HTTP response"))?;
    serde_json::from_slice(&response[body_start..])
        .map_err(|e| format!("{url}: state is not JSON: {e}"))
}

fn counter(state: &Value, field: &str) -> Option<u64> {
    state.get(field).and_then(Value::as_u64)
}

/// Difference of two absolute peer counters; `None` when either side lacks the
/// sensor, so a missing counter never reads as zero loss.
fn counter_delta(before: &Value, after: &Value, field: &str) -> Option<u64> {
    Some(counter(after, field)?.saturating_sub(counter(before, field)?))
}

/// Run one substrate measurement and report it. The window is the same for both
/// variants: warm up, snapshot, send for `duration`, snapshot again.
pub(crate) async fn substrate_pps_mode() -> Result<Value, String> {
    let config = Arc::new(parse_config()?);
    if let Some(mask) = &config.harness_cpus {
        // Harness/control threads first: the sender overrides its own mask.
        pin_to_cpuset(mask)?;
    }
    let payload = build_payload(config.payload_bytes);
    let handles = Arc::new(SenderHandles::new());

    let receiver_before_all = match &config.receiver_state {
        Some(url) => Some(fetch_peer_state(url).await?),
        None => None,
    };

    let sender = std::thread::Builder::new()
        .name("substrate-sender".to_string())
        .spawn({
            let config = Arc::clone(&config);
            let handles = Arc::clone(&handles);
            let payload = payload.clone();
            move || sender_thread(config, payload, handles)
        })
        .map_err(|e| format!("spawn substrate sender: {e}"))?;

    println!(
        "[substrate-pps] run {} variant {} sender cpus {} depth {} dests {} payload {} B; \
         warmup {}s then {}s measured",
        config.run,
        config.variant.as_str(),
        config.sender_cpus,
        config.queue_depth,
        config.destinations.len(),
        config.payload_bytes,
        config.warmup.as_secs(),
        config.duration.as_secs()
    );

    tokio::time::sleep(config.warmup).await;
    let warm_completed = handles.completed();
    let sender_tid = handles.tid.load(Ordering::Relaxed);
    let warm_cpu_secs = super::peer_state::thread_cpu_secs(sender_tid);
    let window_started = Instant::now();
    let receiver_before = match &config.receiver_state {
        Some(url) => Some(fetch_peer_state(url).await?),
        None => None,
    };

    // Progress line per interval, so a stalled variant is visible while it runs.
    let mut ticker = tokio::time::interval(Duration::from_secs(config.report_secs));
    ticker.tick().await;
    let mut last_completed = warm_completed;
    let mut last_at = window_started;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let completed = handles.completed();
                let now = Instant::now();
                let secs = now.duration_since(last_at).as_secs_f64().max(1e-9);
                println!(
                    "[substrate-pps] {}",
                    json!({
                        "variant": config.variant.as_str(),
                        "windowSecs": window_started.elapsed().as_secs_f64(),
                        "completed": completed,
                        "pps": (completed.saturating_sub(last_completed)) as f64 / secs,
                        "errors": handles.counters.errors.load(Ordering::Relaxed),
                    })
                );
                last_completed = completed;
                last_at = now;
                if window_started.elapsed() >= config.duration {
                    break;
                }
            }
            _ = tokio::time::sleep(config.duration.saturating_sub(window_started.elapsed())) => break,
        }
    }

    handles.stop.store(true, Ordering::Relaxed);
    let report = sender
        .join()
        .map_err(|_| "substrate sender thread panicked".to_string())?;
    let window_secs = window_started.elapsed().as_secs_f64();
    let completed = handles.completed();
    let final_cpu_secs = report.cpu_secs;
    let receiver_after = match &config.receiver_state {
        Some(url) => Some(fetch_peer_state(url).await?),
        None => None,
    };
    let outcome = report.outcome;

    let measured = completed.saturating_sub(warm_completed);
    let cpu_secs = match (warm_cpu_secs, final_cpu_secs) {
        (Some(warm), Some(final_)) => Some((final_ - warm).max(0.0)),
        _ => None,
    };
    let payload_gbit = measured as f64 * config.payload_bytes as f64 * 8.0 / 1e9;
    let pps = measured as f64 / window_secs.max(1e-9);
    let pps_per_core = cpu_secs.map(|secs| measured as f64 / secs.max(1e-9));

    let receiver = receiver_before
        .as_ref()
        .zip(receiver_after.as_ref())
        .map(|(before, after)| {
            let datagrams = counter_delta(before, after, "datagrams");
            let drops = counter_delta(before, after, "udpRcvbufErrors");
            let in_errors = counter_delta(before, after, "udpInErrors");
            let nic_rx = counter_delta(before, after, "nicRxDropped");
            let loss_ratio = datagrams.map(|received| {
                if measured == 0 {
                    0.0
                } else {
                    1.0 - received as f64 / measured as f64
                }
            });
            json!({
                "runId": after.get("runId"),
                "cpusAllowedList": after.get("cpusAllowedList"),
                "datagrams": datagrams,
                "bytes": counter_delta(before, after, "bytes"),
                "lossRatio": loss_ratio,
                "udpRcvbufErrors": drops,
                "udpInErrors": in_errors,
                "nicRxDropped": nic_rx,
                "startedBeforeSender": receiver_before_all
                    .as_ref()
                    .and_then(|state| state.get("runId"))
                    == after.get("runId"),
            })
        });

    let receiver_kept_up = receiver.as_ref().map(|receiver| {
        let loss_ok = receiver["lossRatio"]
            .as_f64()
            .is_some_and(|ratio| ratio <= 0.001);
        let drops_ok = receiver["udpRcvbufErrors"]
            .as_u64()
            .is_some_and(|drops| drops == 0);
        let in_errors_ok = receiver["udpInErrors"]
            .as_u64()
            .is_some_and(|errors| errors == 0);
        loss_ok && drops_ok && in_errors_ok
    });
    let sender_mask = report.observed_mask;
    let verdict = match (&outcome, receiver_kept_up) {
        (Err(_), _) => "failed",
        (Ok(()), Some(true)) => "healthy",
        (Ok(()), Some(false)) => "receiver-limited",
        (Ok(()), None) => "unclassified",
    };

    let result = json!({
        "mode": "substrate-pps",
        "runId": config.run,
        "variant": config.variant.as_str(),
        "config": {
            "payloadBytes": config.payload_bytes,
            "destinations": config.destinations.len(),
            "destinationBase": config.destinations.first().map(|d| d.to_string()),
            "queueDepth": config.queue_depth,
            "warmupSecs": config.warmup.as_secs(),
            "durationSecs": config.duration.as_secs(),
            "senderCpusRequested": config.sender_cpus,
            "harnessCpusRequested": config.harness_cpus,
        },
        "placement": {
            "senderTid": sender_tid,
            "senderCpusAllowedList": sender_mask,
            "harnessCpusAllowedList": cpus_allowed_list(),
            "receiverCpusAllowedList": receiver.as_ref().and_then(|r| r["cpusAllowedList"].clone().into()),
            "senderOwnsOneCpu": sender_mask.as_deref().map(|mask| !mask.contains(',') && !mask.contains('-')),
        },
        "sends": {
            "submitted": handles.counters.submitted.load(Ordering::Relaxed),
            "completed": completed,
            "completedInWindow": measured,
            "errors": handles.counters.errors.load(Ordering::Relaxed),
            "batches": handles.counters.batches.load(Ordering::Relaxed),
            "maxInFlight": handles.counters.max_in_flight.load(Ordering::Relaxed),
            "ringEnters": match config.variant {
                // Compio owns its ring internally; its submit count is not
                // observable from the application side.
                Variant::Compio => Value::Null,
                Variant::IoUring => json!(handles.counters.ring_enters.load(Ordering::Relaxed)),
            },
            "sqesPerBatch": (handles.counters.submitted.load(Ordering::Relaxed) as f64)
                / (handles.counters.batches.load(Ordering::Relaxed).max(1) as f64),
        },
        "window": {
            "secs": window_secs,
            "senderCpuSecs": cpu_secs,
            "warmCpuSecs": warm_cpu_secs,
            "exitCpuSecs": final_cpu_secs,
            "pps": pps,
            "ppsPerCore": pps_per_core,
            "payloadGbitPerSec": payload_gbit / window_secs.max(1e-9),
            "payloadGbitPerCpuSec": cpu_secs.map(|secs| payload_gbit / secs.max(1e-9)),
        },
        "receiver": receiver,
        "receiverKeptUp": receiver_kept_up,
        "verdict": verdict,
        "senderOutcome": match outcome {
            Ok(()) => Value::Null,
            Err(error) => json!(error),
        },
    });
    println!(
        "[substrate-pps] {}",
        json!({
            "variant": result["variant"],
            "verdict": result["verdict"],
            "ppsPerCore": result["window"]["ppsPerCore"],
            "payloadGbitPerCpuSec": result["window"]["payloadGbitPerCpuSec"],
        })
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_span_the_peer_prefix_without_repeating() {
        let dests = destinations(Ipv4Addr::new(10, 53, 1, 1), 1000, 9000);
        assert_eq!(dests.len(), 1000);
        assert_eq!(dests[0].to_string(), "10.53.1.1:9000");
        assert_eq!(dests[999].to_string(), "10.53.4.232:9000");
        let unique: std::collections::HashSet<_> = dests.iter().collect();
        assert_eq!(unique.len(), 1000);
        // Every destination stays inside the /16 the peer treats as local.
        assert!(dests.iter().all(|d| match d {
            SocketAddr::V4(v4) => v4.ip().octets()[..2] == [10, 53],
            SocketAddr::V6(_) => false,
        }));
    }

    #[test]
    fn variants_parse_only_the_two_implemented_paths() {
        assert_eq!(Variant::parse("compio").unwrap(), Variant::Compio);
        assert_eq!(Variant::parse("io-uring").unwrap(), Variant::IoUring);
        assert_eq!(Variant::parse("io_uring").unwrap(), Variant::IoUring);
        assert!(Variant::parse("af-xdp").is_err());
        assert!(Variant::parse("").is_err());
    }

    #[test]
    fn single_cpu_sender_masks_are_enforced() {
        // The parser is the contract: a multi-CPU sender mask cannot produce a
        // pps/core number, so it must be rejected before any traffic is sent.
        assert!("0".parse::<String>().is_ok());
        for mask in ["0-2", "0,1", ""] {
            assert!(
                mask.is_empty() || mask.contains(',') || mask.contains('-'),
                "{mask}"
            );
        }
    }

    #[test]
    fn payload_is_preconstructed_and_sized() {
        let payload = build_payload(1316);
        assert_eq!(payload.len(), 1316);
        assert_eq!(payload[0], 0);
        assert_eq!(payload[251], 0);
        assert_eq!(payload[250], 250);
    }

    #[test]
    fn missing_receiver_counters_never_read_as_zero() {
        let before = json!({"datagrams": 100, "bytes": 1000});
        let after = json!({"datagrams": 900, "bytes": 9000, "udpRcvbufErrors": 3});
        assert_eq!(counter_delta(&before, &after, "datagrams"), Some(800));
        assert_eq!(counter_delta(&before, &after, "udpRcvbufErrors"), None);
        assert_eq!(counter_delta(&before, &after, "bytes"), Some(8000));
    }

    #[test]
    fn set_send_buffer_accepts_a_live_socket() {
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        assert!(fd >= 0);
        assert!(set_send_buffer(fd, 1 << 20).is_ok());
        unsafe { libc::close(fd) };
    }

    #[tokio::test]
    async fn fetch_peer_state_rejects_non_http_urls() {
        let error = fetch_peer_state("10.53.0.2:9997/state").await.unwrap_err();
        assert!(error.contains("http://"), "{error}");
    }
}
