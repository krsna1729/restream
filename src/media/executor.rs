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
        if let Some(usage) = sample.usage {
            self.usage.add(usage);
            if sample.cpu_ns > CPU_THRESHOLDS_NS[0] {
                self.slow_usage.add(usage);
            }
        }
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
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test(flavor = "current_thread")]
    async fn media_service_progresses_while_control_thread_is_blocked() {
        let ring = std::sync::Arc::new(crate::media::ring_buffer::RingBuffer::new(8));
        let mut reader =
            crate::media::ring_buffer::Reader::new("media-isolation".into(), ring.clone());
        let cancel = CancellationToken::new();
        let (done, observed) = std::sync::mpsc::channel();
        let task = spawn(cancel.clone(), async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            ring.push(crate::media::packet::MediaPacket {
                media_type: crate::media::packet::MediaType::Video,
                format: crate::media::packet::PayloadFormat::Raw,
                is_keyframe: true,
                track_index: 0,
                pts: 33,
                dts: 33,
                payload: bytes::Bytes::from_static(b"owned-packet"),
            });
            done.send(()).unwrap();
        })
        .unwrap();
        // Deliberately prevent the current-thread control runtime from polling.
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut packets = Vec::new();
        assert_eq!(reader.pull_burst(&mut packets, 8).unwrap(), 1);
        assert_eq!(packets[0].payload.as_ref(), b"owned-packet");
        assert_eq!((packets[0].pts, packets[0].dts), (33, 33));
        task.await.unwrap();
        assert!(
            !cancel.is_cancelled(),
            "successful pass must permit file-loop restart"
        );
    }

    #[tokio::test]
    async fn dropping_control_owner_allows_media_cleanup_to_finish() {
        let cancel = CancellationToken::new();
        let (ready, started) = oneshot::channel();
        let (done, finished) = oneshot::channel();
        let inner_cancel = cancel.clone();
        let owner_cancel = cancel.clone();
        let owner = tokio::spawn(async move {
            run(owner_cancel, async move {
                ready.send(()).unwrap();
                inner_cancel.cancelled().await;
                // Finalization is asynchronous; aborting the inner task would
                // drop this sender and fail the observable cleanup contract.
                tokio::task::yield_now().await;
                done.send(()).unwrap();
            })
            .await
        });
        started.await.unwrap();
        owner.abort();
        assert!(owner.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), finished)
            .await
            .unwrap()
            .unwrap();
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn aborting_media_service_cancels_its_lifecycle() {
        let cancel = CancellationToken::new();
        let (ready, started) = oneshot::channel();
        let task = spawn(cancel.clone(), async move {
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        })
        .unwrap();
        started.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn media_executor_metrics_tracks_service_classes_and_latency() {
        let cancel = CancellationToken::new();
        let snap_before = snapshot(false);
        let (ready, started) = oneshot::channel();
        let (done, finish) = oneshot::channel();
        let task = spawn_with_class(
            labelled("q035-classes", "recording"),
            cancel.clone(),
            async move {
                ready.send(()).unwrap();
                finish.await.unwrap();
            },
        )
        .unwrap();

        started.await.unwrap();
        let snap_running = snapshot(true);
        // Class counters are shared with tests running in parallel; this
        // service's own row is not.
        assert!(active(&snap_running, MediaServiceClass::Recording) >= 1);
        assert!(service_row(&snap_running, "q035-classes").is_some());
        assert!(snap_running.spawned_total > snap_before.spawned_total);
        assert_eq!(snap_running.workers.len(), configured_workers());

        done.send(()).unwrap();
        task.await.unwrap();
        let snap_after = snapshot(true);
        assert!(service_row(&snap_after, "q035-classes").is_none());
        assert!(snap_after.completed_total > snap_before.completed_total);
        assert!(snap_after.queue_latency_samples > snap_before.queue_latency_samples);
    }

    fn active(snapshot: &MediaExecutorSnapshot, class: MediaServiceClass) -> u64 {
        snapshot
            .classes
            .iter()
            .find(|candidate| candidate.class == class)
            .unwrap()
            .active
    }

    #[tokio::test]
    async fn poll_timed_counts_time_in_poll_not_time_parked() {
        let (parked, parked_service) = (PollTiming::default(), PollTiming::default());
        poll_timed(
            [&parked, &parked_service],
            false,
            tokio::time::sleep(Duration::from_millis(60)),
        )
        .await;
        assert!(parked.polls.load(Ordering::Relaxed) >= 2);
        assert!(
            parked.busy_ns.load(Ordering::Relaxed) < 30_000_000,
            "a service parked on a timer is not busy"
        );
        assert_eq!(parked.read(false), parked_service.read(false));

        let (blocking, blocking_service) = (PollTiming::default(), PollTiming::default());
        poll_timed([&blocking, &blocking_service], false, async {
            std::thread::sleep(Duration::from_millis(20));
        })
        .await;
        let stats = blocking.read(false);
        assert_eq!(stats.polls, 1);
        assert!(stats.busy_us >= 20_000);
        assert_eq!(stats.max_poll_us, stats.busy_us);
        assert!(
            stats.max_cpu_us < 4_000,
            "a thread asleep inside a poll spends wall time, not CPU: {stats:?}"
        );
        assert_eq!(stats.polls_over_cpu_4ms, 0);
        assert_eq!(stats, blocking_service.read(false));
    }

    /// Spin the calling thread until it has spent `cpu` of its own CPU time.
    fn spin_cpu(cpu: Duration) {
        let until = thread_cpu_ns() + cpu.as_nanos() as u64;
        while thread_cpu_ns() < until {
            std::hint::spin_loop();
        }
    }

    #[tokio::test]
    async fn poll_timed_charges_the_polling_threads_cpu_and_its_page_faults() {
        let (class, service) = (PollTiming::default(), PollTiming::default());
        poll_timed([&class, &service], true, async {
            spin_cpu(Duration::from_millis(5));
            // Fresh anonymous pages fault in on first touch; how many depends
            // on glibc reuse and THP, so only the charging is asserted.
            let pages = vec![1u8; 4 << 20];
            std::hint::black_box(&pages);
        })
        .await;
        let stats = class.read(true);
        assert_eq!(stats.polls, 1);
        assert!(stats.cpu_us >= 5_000, "{stats:?}");
        assert_eq!(stats.max_cpu_us, stats.cpu_us);
        assert_eq!((stats.polls_over_cpu_1ms, stats.polls_over_cpu_4ms), (1, 1));
        let usage = stats.usage.clone().expect("usage switched on");
        assert!(usage.all.minor_faults >= 1, "{usage:?}");
        assert_eq!(usage.over_cpu_1ms, usage.all, "the one poll is a slow one");
        assert_eq!(stats, service.read(true));
        assert!(class.read(false).usage.is_none(), "usage off is omitted");
    }

    fn sample(cpu_ns: u64, minor_faults: u64) -> PollSample {
        PollSample {
            wall_ns: cpu_ns + 1_000,
            cpu_ns,
            usage: Some(ThreadUsage {
                user_us: cpu_ns / 2_000,
                sys_us: cpu_ns / 2_000,
                minor_faults,
                major_faults: 0,
            }),
        }
    }

    #[test]
    fn polls_are_counted_strictly_above_each_cpu_threshold() {
        let timing = PollTiming::default();
        for cpu_ns in [
            1_000_000, 1_000_001, 4_000_000, 4_000_001, 16_000_000, 16_000_001,
        ] {
            timing.record(&sample(cpu_ns, 1));
        }
        timing.record(&sample(10_000, 1));
        let stats = timing.read(true);
        assert_eq!(stats.polls, 7);
        assert_eq!(
            (
                stats.polls_over_cpu_1ms,
                stats.polls_over_cpu_4ms,
                stats.polls_over_cpu_16ms
            ),
            (5, 3, 1)
        );
        assert_eq!(stats.max_cpu_us, 16_000);
        assert_eq!(stats.cpu_us, 42_010);
        assert_eq!(stats.max_poll_us, 16_001);
        let usage = stats.usage.expect("usage read");
        assert_eq!(usage.all.minor_faults, 7);
        assert_eq!(
            usage.over_cpu_1ms.minor_faults, 5,
            "only polls above 1 ms of CPU are the slow ones"
        );
        assert_eq!(usage.over_cpu_1ms.user_us, 20_500);
    }

    #[test]
    fn poll_stats_serialize_with_the_documented_names() {
        let timing = PollTiming::default();
        timing.record(&sample(5_000_000, 3));
        let row = serde_json::to_value(ClassSnapshot {
            class: MediaServiceClass::HlsIngest,
            active: 1,
            timing: timing.read(true),
        })
        .unwrap();
        assert_eq!(row["class"], "hlsIngest");
        assert_eq!(row["maxCpuUs"], 5_000);
        assert_eq!(row["pollsOverCpu1Ms"], 1);
        assert_eq!(row["pollsOverCpu4Ms"], 1);
        assert_eq!(row["pollsOverCpu16Ms"], 0);
        assert_eq!(row["usage"]["overCpu1Ms"]["minorFaults"], 3);
        assert_eq!(row["usage"]["all"]["sysUs"], 2_500);
        let without = serde_json::to_value(timing.read(false)).unwrap();
        assert!(without.get("usage").is_none());
    }

    fn labelled(pipeline_id: &str, stage: &str) -> MediaService {
        MediaService {
            class: MediaServiceClass::Recording,
            pipeline_id: pipeline_id.to_string(),
            stage: stage.to_string(),
        }
    }

    fn service_row<'a>(
        snapshot: &'a MediaExecutorSnapshot,
        pipeline_id: &str,
    ) -> Option<&'a ServiceSnapshot> {
        snapshot
            .services
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|row| row.pipeline_id == pipeline_id)
    }

    #[tokio::test]
    async fn busy_time_is_attributed_to_the_service_that_spent_it() {
        let cancel = CancellationToken::new();
        let (busy_ready, busy_started) = oneshot::channel();
        let (busy_done, busy_finish) = oneshot::channel::<()>();
        let busy = spawn_with_class(
            labelled("q035-busy", "q035-busy:video"),
            cancel.clone(),
            async move {
                std::thread::sleep(Duration::from_millis(20));
                // Signal from a later poll: the sleeping poll has returned
                // and been recorded before the test reads the row.
                tokio::task::yield_now().await;
                busy_ready.send(()).unwrap();
                let _ = busy_finish.await;
            },
        )
        .unwrap();
        let (idle_ready, idle_started) = oneshot::channel();
        let idle = spawn_with_class(
            labelled("q035-idle", "recording"),
            CancellationToken::new(),
            async move {
                idle_ready.send(()).unwrap();
                std::future::pending::<()>().await;
            },
        )
        .unwrap();
        busy_started.await.unwrap();
        idle_started.await.unwrap();

        assert!(
            snapshot(false).services.is_none(),
            "summary stays fixed-size"
        );
        let running = snapshot(true);
        let busy_row = service_row(&running, "q035-busy").expect("busy service listed");
        let idle_row = service_row(&running, "q035-idle").expect("idle service listed");
        assert_eq!(busy_row.class, MediaServiceClass::Recording);
        assert!(
            idle_row.service_id > busy_row.service_id,
            "ids follow spawn order, so a restart is a new id"
        );
        assert_eq!(busy_row.stage, "q035-busy:video");
        assert_eq!(idle_row.stage, "recording");
        assert!(busy_row.timing.busy_us >= 20_000, "{busy_row:?}");
        assert!(
            idle_row.timing.busy_us < 20_000,
            "the sibling's busy time is not charged here: {idle_row:?}"
        );

        // Both ways a service ends remove its row: completion and abort.
        busy_done.send(()).unwrap();
        busy.await.unwrap();
        idle.abort();
        assert!(idle.await.unwrap_err().is_cancelled());
        let ended = snapshot(true);
        assert!(service_row(&ended, "q035-busy").is_none());
        assert!(service_row(&ended, "q035-idle").is_none());
    }
}
