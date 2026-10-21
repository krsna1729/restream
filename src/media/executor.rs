//! Fixed process-wide execution class for continuous container work.
//!
//! Tokio supplies timers, ring/queue waits and child-pipe readiness here, but
//! these workers never share the application/control scheduler. A service owns
//! its ring cursor and reusable packaging state; there is no per-packet handoff
//! and no thread per feed. Services must check cancellation and yield between
//! bounded bursts. Blocking codecs and disk writes remain on their own threads.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Instant;

use crate::media::timing::{ThreadUsage, thread_cpu_ns, thread_usage};
use crate::sync::lock;

use serde::Serialize;
use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::error;

/// Categorized media service classes running on the media executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum MediaServiceClass {
    SharedTsMux,
    HlsSegmenter,
    HlsFmp4,
    Recording,
    AudioRouter,
    FileIngest,
    HlsIngest,
    #[allow(dead_code)]
    Other,
}

impl MediaServiceClass {
    /// Declaration order; `class as usize` indexes `MediaExecutorMetrics::classes`.
    const ALL: [Self; 8] = [
        Self::SharedTsMux,
        Self::HlsSegmenter,
        Self::HlsFmp4,
        Self::Recording,
        Self::AudioRouter,
        Self::FileIngest,
        Self::HlsIngest,
        Self::Other,
    ];
}

/// Who a media service works for. Labels its counters; built once per
/// service start, never per packet.
pub(crate) struct MediaService {
    pub class: MediaServiceClass,
    pub pipeline_id: String,
    /// Stage key, ingest id, or the work it does for the pipeline.
    pub stage: String,
}

impl MediaService {
    fn other() -> Self {
        Self {
            class: MediaServiceClass::Other,
            pipeline_id: String::new(),
            stage: String::new(),
        }
    }
}

/// Polls are counted above these thread-CPU times: 1, 4 and 16 ms.
const CPU_THRESHOLDS_NS: [u64; 3] = [1_000_000, 4_000_000, 16_000_000];

/// Log2 microsecond buckets for the 99th percentile: bucket `0` is a poll
/// under 1 us, bucket `n` holds `[2^(n-1), 2^n)` us, except the final bucket
/// which has no upper bound. This summarizes a distribution, not a bound on
/// individual polls; an outlier can determine p99 with fewer than 100 samples.
const P99_BUCKETS: usize = 25;

fn cpu_bucket(cpu_ns: u64) -> usize {
    let us = cpu_ns / 1_000;
    if us == 0 {
        0
    } else {
        (us.ilog2() as usize + 1).min(P99_BUCKETS - 1)
    }
}

/// One poll's cost: its wall time, the polling thread's own CPU time, and its
/// resource usage when per-poll usage is switched on.
struct PollSample {
    wall_ns: u64,
    cpu_ns: u64,
    usage: Option<ThreadUsage>,
}

/// Poll counters. `busy_ns` is wall time spent inside the services' `poll`
/// calls, not service lifetime: services run for the life of their pipeline
/// and are parked on rings, timers and pipes almost all of that time. Wall
/// time includes hypervisor vCPU descheduling inside a poll, which a VM guest
/// may not report as steal; `max_poll_ns` exposes such stalls (one 20–120 ms
/// poll among ~µs polls), so read `busy` next to it. `cpu_ns` is the polling
/// thread's own CPU time inside the same polls: it excludes time the guest
/// scheduler gave other threads, but not vCPU time the hypervisor took that
/// the guest does not account as steal (the guest charges that to whatever
/// thread was running).
#[derive(Default)]
struct PollTiming {
    polls: AtomicU64,
    busy_ns: AtomicU64,
    max_poll_ns: AtomicU64,
    cpu_ns: AtomicU64,
    max_cpu_ns: AtomicU64,
    /// Polls above each of `CPU_THRESHOLDS_NS`.
    cpu_over: [AtomicU64; 3],
    /// Log2 microsecond histogram of poll CPU time (`P99_BUCKETS`).
    cpu_hist: [AtomicU64; P99_BUCKETS],
    /// Every poll's resource usage, when switched on.
    usage: UsageCounters,
    /// The same over the polls above the first CPU threshold, which is
    /// where the multi-millisecond tail lives.
    slow_usage: UsageCounters,
}

impl PollTiming {
    fn record(&self, sample: &PollSample) {
        self.polls.fetch_add(1, Ordering::Relaxed);
        self.busy_ns.fetch_add(sample.wall_ns, Ordering::Relaxed);
        self.max_poll_ns
            .fetch_max(sample.wall_ns, Ordering::Relaxed);
        self.cpu_ns.fetch_add(sample.cpu_ns, Ordering::Relaxed);
        self.max_cpu_ns.fetch_max(sample.cpu_ns, Ordering::Relaxed);
        for (count, threshold) in self.cpu_over.iter().zip(CPU_THRESHOLDS_NS) {
            if sample.cpu_ns > threshold {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.cpu_hist[cpu_bucket(sample.cpu_ns)].fetch_add(1, Ordering::Relaxed);
        if let Some(usage) = sample.usage {
            self.usage.add(usage);
            if sample.cpu_ns > CPU_THRESHOLDS_NS[0] {
                self.slow_usage.add(usage);
            }
        }
    }

    /// Lower bound of the bucket holding the 99th percentile of the captured
    /// histogram. Derive the rank from that snapshot rather than the separate
    /// poll counter, which can include a partially recorded poll.
    fn p99_us(&self) -> u64 {
        let counts = self
            .cpu_hist
            .each_ref()
            .map(|count| count.load(Ordering::Relaxed));
        let polls = counts.iter().copied().sum::<u64>();
        if polls == 0 {
            return 0;
        }
        let target = polls.saturating_mul(99).div_ceil(100).max(1);
        let mut seen = 0u64;
        for (bucket, count) in counts.into_iter().enumerate() {
            seen = seen.saturating_add(count);
            if seen >= target {
                return if bucket == 0 { 0 } else { 1u64 << (bucket - 1) };
            }
        }
        0
    }

    /// The counters in microseconds; `usage` only when it is switched on.
    fn read(&self, usage: bool) -> PollStats {
        let [over_1ms, over_4ms, over_16ms] = self
            .cpu_over
            .each_ref()
            .map(|count| count.load(Ordering::Relaxed));
        PollStats {
            polls: self.polls.load(Ordering::Relaxed),
            busy_us: self.busy_ns.load(Ordering::Relaxed) / 1000,
            max_poll_us: self.max_poll_ns.load(Ordering::Relaxed) / 1000,
            cpu_us: self.cpu_ns.load(Ordering::Relaxed) / 1000,
            max_cpu_us: self.max_cpu_ns.load(Ordering::Relaxed) / 1000,
            polls_over_cpu_1ms: over_1ms,
            polls_over_cpu_4ms: over_4ms,
            polls_over_cpu_16ms: over_16ms,
            cpu_p99_us: self.p99_us(),
            usage: usage.then(|| PollUsage {
                all: self.usage.read(),
                over_cpu_1ms: self.slow_usage.read(),
            }),
        }
    }
}

#[derive(Default)]
struct UsageCounters {
    user_us: AtomicU64,
    sys_us: AtomicU64,
    minor_faults: AtomicU64,
    major_faults: AtomicU64,
}

impl UsageCounters {
    fn add(&self, usage: ThreadUsage) {
        self.user_us.fetch_add(usage.user_us, Ordering::Relaxed);
        self.sys_us.fetch_add(usage.sys_us, Ordering::Relaxed);
        self.minor_faults
            .fetch_add(usage.minor_faults, Ordering::Relaxed);
        self.major_faults
            .fetch_add(usage.major_faults, Ordering::Relaxed);
    }

    fn read(&self) -> ThreadUsage {
        ThreadUsage {
            user_us: self.user_us.load(Ordering::Relaxed),
            sys_us: self.sys_us.load(Ordering::Relaxed),
            minor_faults: self.minor_faults.load(Ordering::Relaxed),
            major_faults: self.major_faults.load(Ordering::Relaxed),
        }
    }
}

#[derive(Default)]
struct ClassMetrics {
    active: AtomicU64,
    timing: PollTiming,
}

struct ServiceMetrics {
    id: u64,
    service: MediaService,
    timing: PollTiming,
}

#[derive(Default)]
struct MediaExecutorMetrics {
    spawned_total: AtomicU64,
    completed_total: AtomicU64,
    panicked_total: AtomicU64,
    queue_latency_samples: AtomicU64,
    queue_latency_us_sum: AtomicU64,
    queue_latency_us_max: AtomicU64,
    classes: [ClassMetrics; 8],
    /// Running services; touched at start, end and snapshot only.
    services: Mutex<Vec<Arc<ServiceMetrics>>>,
    next_service_id: AtomicU64,
}

impl MediaExecutorMetrics {
    fn class(&self, class: MediaServiceClass) -> &ClassMetrics {
        &self.classes[class as usize]
    }

    fn record_start(&self, service: &Arc<ServiceMetrics>, queue_latency_us: u64) {
        self.spawned_total.fetch_add(1, Ordering::Relaxed);
        self.class(service.service.class)
            .active
            .fetch_add(1, Ordering::Relaxed);
        self.queue_latency_samples.fetch_add(1, Ordering::Relaxed);
        self.queue_latency_us_sum
            .fetch_add(queue_latency_us, Ordering::Relaxed);
        self.queue_latency_us_max
            .fetch_max(queue_latency_us, Ordering::Relaxed);
        lock(&self.services).push(service.clone());
    }

    fn unregister(&self, service: &Arc<ServiceMetrics>) {
        let mut services = lock(&self.services);
        if let Some(index) = services
            .iter()
            .position(|running| Arc::ptr_eq(running, service))
        {
            services.swap_remove(index);
        }
    }

    fn record_completion(&self, class: MediaServiceClass) {
        self.class(class).active.fetch_sub(1, Ordering::Relaxed);
        self.completed_total.fetch_add(1, Ordering::Relaxed);
    }

    fn record_failure_or_abort(&self, class: MediaServiceClass) {
        self.class(class).active.fetch_sub(1, Ordering::Relaxed);
        self.panicked_total.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(
        &self,
        configured_workers: usize,
        runtime: Option<&Runtime>,
        with_services: bool,
        poll_usage: bool,
    ) -> MediaExecutorSnapshot {
        let samples = self.queue_latency_samples.load(Ordering::Relaxed);
        let sum_us = self.queue_latency_us_sum.load(Ordering::Relaxed);
        let classes: Vec<ClassSnapshot> = MediaServiceClass::ALL
            .iter()
            .map(|&class| {
                let metrics = self.class(class);
                ClassSnapshot {
                    class,
                    active: metrics.active.load(Ordering::Relaxed),
                    timing: metrics.timing.read(poll_usage),
                }
            })
            .collect();
        let services = with_services.then(|| {
            lock(&self.services)
                .iter()
                .map(|running| ServiceSnapshot {
                    service_id: running.id,
                    class: running.service.class,
                    pipeline_id: running.service.pipeline_id.clone(),
                    stage: running.service.stage.clone(),
                    timing: running.timing.read(poll_usage),
                })
                .collect()
        });
        let (workers, global_queue_depth) = runtime.map_or((Vec::new(), 0), |runtime| {
            let metrics = runtime.metrics();
            let workers = (0..metrics.num_workers())
                .map(|worker| WorkerSnapshot {
                    busy_us: metrics.worker_total_busy_duration(worker).as_micros() as u64,
                    parks: metrics.worker_park_count(worker),
                })
                .collect();
            (workers, metrics.global_queue_depth())
        });

        MediaExecutorSnapshot {
            configured_workers,
            active_services: classes.iter().map(|class| class.active).sum(),
            spawned_total: self.spawned_total.load(Ordering::Relaxed),
            completed_total: self.completed_total.load(Ordering::Relaxed),
            panicked_total: self.panicked_total.load(Ordering::Relaxed),
            queue_latency_samples: samples,
            queue_latency_avg_us: sum_us.checked_div(samples).unwrap_or(0),
            queue_latency_max_us: self.queue_latency_us_max.load(Ordering::Relaxed),
            global_queue_depth,
            classes,
            workers,
            services,
        }
    }
}

static METRICS: OnceLock<MediaExecutorMetrics> = OnceLock::new();
static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

fn metrics() -> &'static MediaExecutorMetrics {
    METRICS.get_or_init(MediaExecutorMetrics::default)
}

/// Cumulative poll counters of a class or a service. Utilization over a
/// window is `Δbusy_us / (Δwall_us × configured_workers)`; demand per wake is
/// `Δbusy_us / Δpolls`, and CPU per poll `Δcpu_us / Δpolls`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PollStats {
    pub polls: u64,
    /// Wall time inside polls.
    pub busy_us: u64,
    pub max_poll_us: u64,
    /// The polling threads' own CPU time inside polls.
    pub cpu_us: u64,
    pub max_cpu_us: u64,
    #[serde(rename = "pollsOverCpu1Ms")]
    pub polls_over_cpu_1ms: u64,
    #[serde(rename = "pollsOverCpu4Ms")]
    pub polls_over_cpu_4ms: u64,
    #[serde(rename = "pollsOverCpu16Ms")]
    pub polls_over_cpu_16ms: u64,
    /// Lower bound of the power-of-two microsecond bucket holding the 99th
    /// percentile of poll CPU time.
    #[serde(rename = "cpuP99Us")]
    pub cpu_p99_us: u64,
    /// Present only with `RESTREAM_MEDIA_POLL_USAGE` switched on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<PollUsage>,
}

/// `getrusage(RUSAGE_THREAD)` deltas summed over polls: every poll, and only
/// the polls above 1 ms of thread CPU.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PollUsage {
    pub all: ThreadUsage,
    #[serde(rename = "overCpu1Ms")]
    pub over_cpu_1ms: ThreadUsage,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClassSnapshot {
    pub class: MediaServiceClass,
    pub active: u64,
    #[serde(flatten)]
    pub timing: PollStats,
}

/// Cumulative counters of one running service, from its first poll.
/// `service_id` is unique per service start: a restarted service (a new
/// file-ingest attempt) is a new id that counts from zero, and a run being
/// replaced can be listed beside its successor with the same labels.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServiceSnapshot {
    pub service_id: u64,
    pub class: MediaServiceClass,
    pub pipeline_id: String,
    pub stage: String,
    #[serde(flatten)]
    pub timing: PollStats,
}

/// Cumulative per-worker counters from the Tokio runtime.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkerSnapshot {
    pub busy_us: u64,
    pub parks: u64,
}

/// Operational snapshot of media executor thread pool and active workloads.
/// `workers` is empty and `global_queue_depth` zero until the first service
/// starts the runtime. `services` is present only when asked for.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MediaExecutorSnapshot {
    pub configured_workers: usize,
    pub active_services: u64,
    pub spawned_total: u64,
    pub completed_total: u64,
    pub panicked_total: u64,
    pub queue_latency_samples: u64,
    pub queue_latency_avg_us: u64,
    pub queue_latency_max_us: u64,
    pub global_queue_depth: usize,
    pub classes: Vec<ClassSnapshot>,
    pub workers: Vec<WorkerSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<ServiceSnapshot>>,
}

/// Expose current media executor telemetry; `services` adds one row per
/// running service (its pipeline and stage), for attribution.
pub(crate) fn snapshot(services: bool) -> MediaExecutorSnapshot {
    let runtime = RUNTIME.get().and_then(|runtime| runtime.as_ref().ok());
    metrics().snapshot(configured_workers(), runtime, services, poll_usage())
}

/// Returns the worker thread count configured for restream-media.
pub(crate) fn configured_workers() -> usize {
    crate::config::media_executor_workers_env()
        .unwrap_or_else(|| crate::capacity::HostCapacity::get().media_workers())
}

/// Whether each poll also reads `getrusage(RUSAGE_THREAD)`
/// (`RESTREAM_MEDIA_POLL_USAGE`), read once. A diagnostic: two more
/// syscalls per poll for the user/system split and page faults.
static POLL_USAGE: LazyLock<bool> = LazyLock::new(crate::config::media_poll_usage_env);

fn poll_usage() -> bool {
    *POLL_USAGE
}

fn runtime() -> Result<&'static Runtime, String> {
    RUNTIME
        .get_or_init(|| {
            let workers = configured_workers();
            Builder::new_multi_thread()
                .worker_threads(workers)
                .thread_name("restream-media")
                .enable_all()
                .build()
                .map_err(|error| format!("media executor initialization failed: {error}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Drive `future`, adding the wall time and the polling thread's CPU time of
/// each of its `poll` calls to its class and to its own service. Two
/// monotonic and two thread-CPU clock reads per poll (two `getrusage` more
/// with `usage`); services poll once per wake or yielded burst, never per
/// packet.
async fn poll_timed<F: Future>(timings: [&PollTiming; 2], usage: bool, future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| {
        let usage_before = usage.then(thread_usage);
        let started = Instant::now();
        let cpu_started = thread_cpu_ns();
        let poll = future.as_mut().poll(cx);
        let cpu_ns = thread_cpu_ns().saturating_sub(cpu_started);
        let wall_ns = started.elapsed().as_nanos() as u64;
        let sample = PollSample {
            wall_ns,
            cpu_ns,
            usage: usage_before.map(|before| thread_usage().since(before)),
        };
        for timing in timings {
            timing.record(&sample);
        }
        poll
    })
    .await
}

struct CancelOnDrop {
    cancel: Option<CancellationToken>,
    class: MediaServiceClass,
    /// Registered at first poll; unregistered when the service ends.
    service: Option<Arc<ServiceMetrics>>,
    started: bool,
    completed: bool,
}

impl CancelOnDrop {
    fn disarm(&mut self) {
        self.cancel = None;
        self.completed = true;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
            if std::thread::panicking() {
                error!("media service panicked; cancelling its lifecycle");
            }
        }
        if self.started {
            if let Some(service) = &self.service {
                metrics().unregister(service);
            }
            if !self.completed {
                metrics().record_failure_or_abort(self.class);
            }
        }
    }
}

/// Start an owned media service. Tokio contains task panics; the guard also
/// cancels its lifecycle on panic or abort, even before its first poll.
/// Successful completion does not cancel: file-input EOF may start a new pass.
pub(crate) fn spawn_with_class<F>(
    service: MediaService,
    cancel: CancellationToken,
    future: F,
) -> Result<JoinHandle<F::Output>, String>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let class = service.class;
    let service = Arc::new(ServiceMetrics {
        id: metrics().next_service_id.fetch_add(1, Ordering::Relaxed),
        service,
        timing: PollTiming::default(),
    });
    let mut guard = CancelOnDrop {
        cancel: Some(cancel),
        class,
        service: Some(service.clone()),
        started: false,
        completed: false,
    };
    let runtime = runtime().inspect_err(|error| error!(%error))?;
    let queued_at = Instant::now();
    Ok(runtime.spawn(async move {
        let queue_latency_us = queued_at.elapsed().as_micros() as u64;
        metrics().record_start(&service, queue_latency_us);
        guard.started = true;
        let timings = [&metrics().class(class).timing, &service.timing];
        let output = poll_timed(timings, poll_usage(), future).await;
        metrics().record_completion(class);
        guard.disarm();
        output
    }))
}

/// Start an owned media service with default class `Other`.
#[allow(dead_code)]
pub(crate) fn spawn<F>(
    cancel: CancellationToken,
    future: F,
) -> Result<JoinHandle<F::Output>, String>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    spawn_with_class(MediaService::other(), cancel, future)
}

/// Await a media service from the control runtime.
pub(crate) async fn run_with_class<F>(
    service: MediaService,
    cancel: CancellationToken,
    future: F,
) -> Result<F::Output, String>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut guard = CancelOnDrop {
        cancel: Some(cancel.clone()),
        class: service.class,
        service: None,
        started: false,
        completed: false,
    };
    let task = spawn_with_class(service, cancel, future)?;
    match task.await {
        Ok(output) => {
            guard.disarm();
            Ok(output)
        }
        Err(error) => {
            error!(%error, "media service failed");
            Err(error.to_string())
        }
    }
}

/// Await a media service from the control runtime with default class `Other`.
#[allow(dead_code)]
pub(crate) async fn run<F>(cancel: CancellationToken, future: F) -> Result<F::Output, String>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    run_with_class(MediaService::other(), cancel, future).await
}

#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
