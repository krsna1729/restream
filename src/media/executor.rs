//! Fixed process-wide execution class for continuous container work.
//!
//! Tokio supplies timers, ring/queue waits and child-pipe readiness here, but
//! these workers never share the application/control scheduler. A service owns
//! its ring cursor and reusable packaging state; there is no per-packet handoff
//! and no thread per feed. Services must check cancellation and yield between
//! bounded bursts. Blocking codecs and disk writes remain on their own threads.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

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
    #[allow(dead_code)]
    Other,
}

#[derive(Default)]
struct MediaExecutorMetrics {
    spawned_total: AtomicU64,
    completed_total: AtomicU64,
    panicked_total: AtomicU64,
    active_ts_muxers: AtomicU64,
    active_hls_segmenters: AtomicU64,
    active_hls_fmp4: AtomicU64,
    active_recordings: AtomicU64,
    active_audio_routers: AtomicU64,
    active_file_ingests: AtomicU64,
    active_other: AtomicU64,
    queue_latency_samples: AtomicU64,
    queue_latency_us_sum: AtomicU64,
    queue_latency_us_max: AtomicU64,
    busy_us_sum: AtomicU64,
}

impl MediaExecutorMetrics {
    fn active_counter(&self, class: MediaServiceClass) -> &AtomicU64 {
        match class {
            MediaServiceClass::SharedTsMux => &self.active_ts_muxers,
            MediaServiceClass::HlsSegmenter => &self.active_hls_segmenters,
            MediaServiceClass::HlsFmp4 => &self.active_hls_fmp4,
            MediaServiceClass::Recording => &self.active_recordings,
            MediaServiceClass::AudioRouter => &self.active_audio_routers,
            MediaServiceClass::FileIngest => &self.active_file_ingests,
            MediaServiceClass::Other => &self.active_other,
        }
    }

    fn record_start(&self, class: MediaServiceClass, queue_latency_us: u64) {
        self.spawned_total.fetch_add(1, Ordering::Relaxed);
        self.active_counter(class).fetch_add(1, Ordering::Relaxed);
        self.queue_latency_samples.fetch_add(1, Ordering::Relaxed);
        self.queue_latency_us_sum
            .fetch_add(queue_latency_us, Ordering::Relaxed);
        self.queue_latency_us_max
            .fetch_max(queue_latency_us, Ordering::Relaxed);
    }

    fn record_completion(&self, class: MediaServiceClass, busy_us: u64) {
        self.active_counter(class).fetch_sub(1, Ordering::Relaxed);
        self.completed_total.fetch_add(1, Ordering::Relaxed);
        self.busy_us_sum.fetch_add(busy_us, Ordering::Relaxed);
    }

    fn record_failure_or_abort(&self, class: MediaServiceClass) {
        self.active_counter(class).fetch_sub(1, Ordering::Relaxed);
        self.panicked_total.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self, configured_workers: usize) -> MediaExecutorSnapshot {
        let samples = self.queue_latency_samples.load(Ordering::Relaxed);
        let sum_us = self.queue_latency_us_sum.load(Ordering::Relaxed);
        let avg_us = sum_us.checked_div(samples).unwrap_or(0);
        let active_mux = self.active_ts_muxers.load(Ordering::Relaxed);
        let active_hls = self.active_hls_segmenters.load(Ordering::Relaxed);
        let active_fmp4 = self.active_hls_fmp4.load(Ordering::Relaxed);
        let active_rec = self.active_recordings.load(Ordering::Relaxed);
        let active_audio = self.active_audio_routers.load(Ordering::Relaxed);
        let active_file = self.active_file_ingests.load(Ordering::Relaxed);
        let active_other = self.active_other.load(Ordering::Relaxed);
        let active_total = active_mux
            + active_hls
            + active_fmp4
            + active_rec
            + active_audio
            + active_file
            + active_other;

        MediaExecutorSnapshot {
            configured_workers,
            active_services: active_total,
            active_ts_muxers: active_mux,
            active_hls_segmenters: active_hls + active_fmp4,
            active_recordings: active_rec,
            active_audio_routers: active_audio,
            active_file_ingests: active_file,
            active_other,
            spawned_total: self.spawned_total.load(Ordering::Relaxed),
            completed_total: self.completed_total.load(Ordering::Relaxed),
            panicked_total: self.panicked_total.load(Ordering::Relaxed),
            queue_latency_samples: samples,
            queue_latency_avg_us: avg_us,
            queue_latency_max_us: self.queue_latency_us_max.load(Ordering::Relaxed),
            busy_time_total_ms: self.busy_us_sum.load(Ordering::Relaxed) / 1000,
        }
    }
}

static METRICS: OnceLock<MediaExecutorMetrics> = OnceLock::new();

fn metrics() -> &'static MediaExecutorMetrics {
    METRICS.get_or_init(MediaExecutorMetrics::default)
}

/// Operational snapshot of media executor thread pool and active workloads.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MediaExecutorSnapshot {
    pub configured_workers: usize,
    pub active_services: u64,
    pub active_ts_muxers: u64,
    pub active_hls_segmenters: u64,
    pub active_recordings: u64,
    pub active_audio_routers: u64,
    pub active_file_ingests: u64,
    pub active_other: u64,
    pub spawned_total: u64,
    pub completed_total: u64,
    pub panicked_total: u64,
    pub queue_latency_samples: u64,
    pub queue_latency_avg_us: u64,
    pub queue_latency_max_us: u64,
    pub busy_time_total_ms: u64,
}

/// Expose current media executor telemetry.
pub(crate) fn snapshot() -> MediaExecutorSnapshot {
    metrics().snapshot(configured_workers())
}

/// Returns the worker thread count configured for restream-media.
pub(crate) fn configured_workers() -> usize {
    crate::config::media_executor_workers_env()
        .unwrap_or_else(|| crate::system_sampling::effective_cpu_count().clamp(1, 4))
}

fn runtime() -> Result<&'static Runtime, String> {
    static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
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

struct CancelOnDrop {
    cancel: Option<CancellationToken>,
    class: MediaServiceClass,
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
        if self.started && !self.completed {
            metrics().record_failure_or_abort(self.class);
        }
    }
}

/// Start an owned media service. Tokio contains task panics; the guard also
/// cancels its lifecycle on panic or abort, even before its first poll.
/// Successful completion does not cancel: file-input EOF may start a new pass.
/// Start an owned media service with an explicit service class.
pub(crate) fn spawn_with_class<F>(
    class: MediaServiceClass,
    cancel: CancellationToken,
    future: F,
) -> Result<JoinHandle<F::Output>, String>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut guard = CancelOnDrop {
        cancel: Some(cancel),
        class,
        started: false,
        completed: false,
    };
    let runtime = runtime().inspect_err(|error| error!(%error))?;
    let queued_at = Instant::now();
    Ok(runtime.spawn(async move {
        let queue_latency_us = queued_at.elapsed().as_micros() as u64;
        metrics().record_start(class, queue_latency_us);
        guard.started = true;
        let run_start = Instant::now();
        let output = future.await;
        let busy_us = run_start.elapsed().as_micros() as u64;
        metrics().record_completion(class, busy_us);
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
    spawn_with_class(MediaServiceClass::Other, cancel, future)
}

/// Await a media service from the control runtime with an explicit service class.
pub(crate) async fn run_with_class<F>(
    class: MediaServiceClass,
    cancel: CancellationToken,
    future: F,
) -> Result<F::Output, String>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut guard = CancelOnDrop {
        cancel: Some(cancel.clone()),
        class,
        started: false,
        completed: false,
    };
    let task = spawn_with_class(class, cancel, future)?;
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

/// Await a media service from the control runtime.
#[allow(dead_code)]
pub(crate) async fn run<F>(cancel: CancellationToken, future: F) -> Result<F::Output, String>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    run_with_class(MediaServiceClass::Other, cancel, future).await
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
        let snap_before = snapshot();
        let (ready, started) = oneshot::channel();
        let (done, finish) = oneshot::channel();
        let task = spawn_with_class(MediaServiceClass::Recording, cancel.clone(), async move {
            ready.send(()).unwrap();
            finish.await.unwrap();
        })
        .unwrap();

        started.await.unwrap();
        let snap_running = snapshot();
        assert!(snap_running.active_recordings >= 1);
        assert!(snap_running.spawned_total > snap_before.spawned_total);

        done.send(()).unwrap();
        task.await.unwrap();
        let snap_after = snapshot();
        assert_eq!(
            snap_after.active_recordings,
            snap_running.active_recordings - 1
        );
        assert!(snap_after.completed_total > snap_before.completed_total);
        assert!(snap_after.queue_latency_samples > snap_before.queue_latency_samples);
    }
}
