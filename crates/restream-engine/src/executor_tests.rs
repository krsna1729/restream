//! Tests for the media executor's poll timing and lifecycle.

use super::*;
use std::time::Duration;
use tokio::sync::oneshot;

#[tokio::test(flavor = "current_thread")]
async fn media_service_progresses_while_control_thread_is_blocked() {
    let ring = std::sync::Arc::new(restream_sync::RingBuffer::new(8));
    let mut reader = restream_sync::Reader::new("media-isolation".into(), ring.clone());
    let cancel = CancellationToken::new();
    let (done, observed) = std::sync::mpsc::channel();
    let task = spawn(cancel.clone(), async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        ring.push(restream_core::MediaPacket {
            media_type: restream_core::MediaType::Video,
            format: restream_core::PayloadFormat::Raw,
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
        labelled("class-metrics", "recording"),
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
    assert!(service_row(&snap_running, "class-metrics").is_some());
    assert!(snap_running.spawned_total > snap_before.spawned_total);
    assert_eq!(snap_running.workers.len(), configured_workers());

    done.send(()).unwrap();
    task.await.unwrap();
    let snap_after = snapshot(true);
    assert!(service_row(&snap_after, "class-metrics").is_none());
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
        // A fresh private mapping faults on first touch whatever glibc's
        // arena holds; MADV_NOHUGEPAGE keeps it one fault per page.
        const LEN: usize = 64 * 4096;
        // SAFETY: an anonymous private mapping owned by this block,
        // checked, written within bounds, then unmapped.
        unsafe {
            let region = libc::mmap(
                std::ptr::null_mut(),
                LEN,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert_ne!(region, libc::MAP_FAILED);
            libc::madvise(region, LEN, libc::MADV_NOHUGEPAGE);
            std::ptr::write_bytes(region.cast::<u8>(), 1, LEN);
            libc::munmap(region, LEN);
        }
    })
    .await;
    let stats = class.read(true);
    assert_eq!(stats.polls, 1);
    assert!(stats.cpu_us >= 5_000, "{stats:?}");
    assert_eq!(stats.max_cpu_us, stats.cpu_us);
    assert_eq!((stats.polls_over_cpu_1ms, stats.polls_over_cpu_4ms), (1, 1));
    let usage = stats.usage.clone().expect("usage switched on");
    assert!(usage.all.minor_faults >= 64, "{usage:?}");
    assert_eq!(usage.over_cpu_1ms, usage.all, "the one poll is a slow one");
    assert_eq!(stats, service.read(true));
    assert!(class.read(false).usage.is_none(), "usage off is omitted");
}

#[test]
fn the_p99_bucket_follows_the_bulk_of_polls_not_the_maximum() {
    let timing = PollTiming::default();
    for _ in 0..99 {
        timing.record(&sample(50_000, 1));
    }
    timing.record(&sample(9_000_000, 1));
    let stats = timing.read(false);
    assert_eq!(stats.polls, 100);
    assert_eq!(stats.max_cpu_us, 9_000);
    assert_eq!(
        stats.cpu_p99_us, 32,
        "99 of 100 polls are in the 32-64 us bucket; one stall is not the percentile"
    );

    let idle = PollTiming::default();
    assert_eq!(idle.read(false).cpu_p99_us, 0, "no polls, no percentile");
    for _ in 0..10 {
        idle.record(&sample(0, 1));
    }
    assert_eq!(idle.read(false).cpu_p99_us, 0);
}

#[test]
fn the_p99_snapshot_ignores_a_partially_recorded_poll() {
    let timing = PollTiming::default();
    for _ in 0..98 {
        timing.record(&sample(50_000, 0));
    }
    // record() increments polls before its histogram bucket. This exact
    // interleaving must not fabricate an unobserved top bucket.
    timing.polls.fetch_add(1, Ordering::Relaxed);
    let snapshot = timing.read(false);
    assert_eq!(snapshot.polls, 99);
    assert_eq!(snapshot.max_cpu_us, 50);
    assert_eq!(snapshot.cpu_p99_us, 32);
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
    assert_eq!(stats.cpu_p99_us, 8_192, "the bucket holding poll 7 of 7");
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
    assert_eq!(row["cpuP99Us"], 4_096, "5 ms sits in the 4-8 ms bucket");
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
        labelled("attributed-busy", "attributed-busy:video"),
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
        labelled("attributed-idle", "recording"),
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
    let busy_row = service_row(&running, "attributed-busy").expect("busy service listed");
    let idle_row = service_row(&running, "attributed-idle").expect("idle service listed");
    assert_eq!(busy_row.class, MediaServiceClass::Recording);
    assert!(
        idle_row.service_id > busy_row.service_id,
        "ids follow spawn order, so a restart is a new id"
    );
    assert_eq!(busy_row.stage, "attributed-busy:video");
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
    assert!(service_row(&ended, "attributed-busy").is_none());
    assert!(service_row(&ended, "attributed-idle").is_none());
}
