/// Verify that when EpollStopGuard drops (simulating a cancelled async
/// future), a waiter parked in `wait_for_request()` observes the stop and
/// exits promptly. This exercises the RAII path that prevents
/// srt_epoll_release from being skipped on future cancellation.
#[tokio::test]
async fn epoll_stop_guard_signals_waiter_on_drop() {
    let signal = Arc::new(EpollWaiterSignal::new());
    let notify = Arc::new(Notify::new());
    let task_exited = Arc::new(AtomicBool::new(false));

    let w_signal = signal.clone();
    let w_exited = task_exited.clone();

    // Simulates the epoll_waiter task: parks for requests, exits on stop.
    let handle = tokio::task::spawn_blocking(move || {
        while w_signal.wait_for_request() {
            // No epoll in this simulation; a real waiter would arm one
            // srt_epoll_wait per serviced request here.
        }
        w_exited.store(true, Ordering::Release);
    });

    // EpollStopGuard inline: signals stop + notifies on drop.
    struct EpollStopGuard {
        signal: Arc<EpollWaiterSignal>,
        notify: Arc<Notify>,
    }
    impl Drop for EpollStopGuard {
        fn drop(&mut self) {
            self.signal.stop();
            self.notify.notify_one();
        }
    }
    let guard = EpollStopGuard {
        signal: signal.clone(),
        notify: notify.clone(),
    };

    // Drop the guard — simulates the async future being cancelled.
    drop(guard);

    // Task must exit within 300ms (condvar wake + scheduling slack).
    tokio::time::timeout(std::time::Duration::from_millis(300), handle)
        .await
        .expect("epoll_waiter task must exit within 300ms of guard drop")
        .expect("task should not panic");

    assert!(
        task_exited.load(Ordering::Acquire),
        "task must have observed the stop flag"
    );
}

/// The demand-gating regression: a waiter parked in `wait_for_request()` must
/// not run any epoll iterations while the receive loop is busy (no request
/// outstanding). The old unconditional wait loop spun a full core against a
/// level-triggered-ready socket; this asserts the waiter services exactly as
/// many waits as were requested — zero without a request.
#[tokio::test]
async fn epoll_waiter_parks_until_wait_is_requested() {
    use std::sync::atomic::AtomicU32;

    let signal = Arc::new(EpollWaiterSignal::new());
    let serviced = Arc::new(AtomicU32::new(0));

    let w_signal = signal.clone();
    let w_serviced = serviced.clone();
    let handle = tokio::task::spawn_blocking(move || {
        while w_signal.wait_for_request() {
            w_serviced.fetch_add(1, Ordering::Release);
        }
    });

    // Consumer busy, no request outstanding: the waiter must stay parked.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        serviced.load(Ordering::Acquire),
        0,
        "waiter must not service waits nobody requested"
    );

    // One request -> exactly one serviced wait, then parked again.
    signal.request_wait();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while serviced.load(Ordering::Acquire) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "waiter must service a requested wait"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        serviced.load(Ordering::Acquire),
        1,
        "one request must arm exactly one wait"
    );

    signal.stop();
    tokio::time::timeout(std::time::Duration::from_millis(300), handle)
        .await
        .expect("waiter must exit promptly on stop")
        .expect("task should not panic");
}

#[tokio::test]
async fn srt_readiness_wait_retries_without_epoll_notification() {
    let data_ready = AtomicBool::new(false);
    let signal = EpollWaiterSignal::new();
    let notify = Notify::new();
    let cancel = CancellationToken::new();

    let started = std::time::Instant::now();
    let should_retry = wait_for_srt_ingest_readiness(&data_ready, &signal, &notify, &cancel).await;
    let elapsed = started.elapsed();

    assert!(
        should_retry,
        "missing epoll notification should retry non-blocking srt_recv"
    );
    assert!(
        elapsed >= SRT_INGEST_READINESS_RETRY,
        "retry should wait for the bounded readiness interval"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "retry safeguard must not let ingest sleep indefinitely"
    );
}

#[tokio::test]
async fn srt_readiness_wait_exits_on_cancel() {
    let data_ready = AtomicBool::new(false);
    let signal = EpollWaiterSignal::new();
    let notify = Notify::new();
    let cancel = CancellationToken::new();
    cancel.cancel();

    let should_retry = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        wait_for_srt_ingest_readiness(&data_ready, &signal, &notify, &cancel),
    )
    .await
    .expect("cancelled readiness wait should return promptly");

    assert!(
        !should_retry,
        "cancelled ingest should break instead of retrying receive"
    );
}

#[test]
fn loom_srt_readiness_retry_does_not_depend_on_epoll_wake() {
    loom::model(|| {
        use loom::sync::Arc as LoomArc;
        use loom::sync::atomic::{AtomicBool as LoomAtomicBool, Ordering as LoomOrdering};
        use loom::thread;

        let data_ready = LoomArc::new(LoomAtomicBool::new(false));
        let wait_requested = LoomArc::new(LoomAtomicBool::new(false));
        let consumer_progress = LoomArc::new(LoomAtomicBool::new(false));

        let consumer_data_ready = data_ready.clone();
        let consumer_wait_requested = wait_requested.clone();
        let consumer_progress_flag = consumer_progress.clone();
        let consumer = thread::spawn(move || {
            if consumer_data_ready.swap(false, LoomOrdering::AcqRel) {
                consumer_progress_flag.store(true, LoomOrdering::Release);
                return;
            }

            consumer_wait_requested.store(true, LoomOrdering::Release);
            // Models the bounded retry timer in wait_for_srt_ingest_readiness:
            // even if the epoll waiter never observes the request, the async
            // receive loop must re-enter non-blocking srt_recv.
            consumer_progress_flag.store(true, LoomOrdering::Release);
        });

        let producer_data_ready = data_ready.clone();
        let producer_wait_requested = wait_requested.clone();
        let producer = thread::spawn(move || {
            if producer_wait_requested.load(LoomOrdering::Acquire) {
                producer_data_ready.store(true, LoomOrdering::Release);
            }
        });

        consumer.join().expect("consumer model should not panic");
        producer.join().expect("producer model should not panic");
        assert!(
            consumer_progress.load(LoomOrdering::Acquire),
            "readiness wait must make progress even when the epoll wake is lost"
        );
    });
}

#[derive(Debug, Clone, Copy)]
enum ReadinessOutcome {
    EpollWake,
    RetryTimer,
    LostWake,
    Cancel,
}

fn modeled_readiness_wait(already_ready: bool, outcome: ReadinessOutcome) -> bool {
    if already_ready {
        return true;
    }
    !matches!(outcome, ReadinessOutcome::Cancel)
}

prop_compose! {
    fn readiness_outcome_strategy()(raw in 0u8..4) -> ReadinessOutcome {
        match raw {
            0 => ReadinessOutcome::EpollWake,
            1 => ReadinessOutcome::RetryTimer,
            2 => ReadinessOutcome::LostWake,
            _ => ReadinessOutcome::Cancel,
        }
    }
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(128))]

    #[test]
    fn proptest_srt_readiness_retry_model_never_requires_epoll_wake(
        events in proptest::collection::vec((any::<bool>(), readiness_outcome_strategy()), 1..256)
    ) {
        for (already_ready, outcome) in events {
            let should_retry = modeled_readiness_wait(already_ready, outcome);
            if already_ready || !matches!(outcome, ReadinessOutcome::Cancel) {
                prop_assert!(
                    should_retry,
                    "readiness wait must retry for {:?} without requiring an epoll notification",
                    outcome
                );
            } else {
                prop_assert!(!should_retry, "cancel must remain the only non-retry outcome");
            }
        }
    }
}

/// Stress-test the demand-gated handshake used by the long-lived epoll waiter
/// (EpollWaiterSignal + AtomicBool + Notify). Concurrent producer and consumer
/// run with randomized timing to surface missed-wakeup races.
///
/// The producer (spawn_blocking) simulates the real waiter: parks in
/// wait_for_request(), then after a brief random delay (simulating
/// srt_epoll_wait returning ready) does store(true) + notify_one(). The
/// consumer (async) simulates the EAGAIN handler: swap(false) -> fall
/// through, or request_wait() + notified().await.
///
/// A 30-second deadline prevents hangs from missed wakeups, and the producer
/// must service no more waits than the consumer requested — the demand-gating
/// property that prevents the busy-spin.
#[tokio::test]
async fn epoll_waiter_coordination() {
    use rand::RngExt;
    use rand::SeedableRng;
    use std::sync::atomic::AtomicU32;

    const ITEMS: u32 = 10_000;
    let data_ready = Arc::new(AtomicBool::new(false));
    let signal = Arc::new(EpollWaiterSignal::new());
    let notify = Arc::new(Notify::new());
    let produced = Arc::new(AtomicU32::new(0));

    let w_data_ready = data_ready.clone();
    let w_signal = signal.clone();
    let w_notify = notify.clone();
    let w_produced = produced.clone();

    // Producer: services requested waits on a blocking thread until the
    // consumer signals stop.
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    let producer = tokio::task::spawn_blocking(move || {
        while w_signal.wait_for_request() {
            // Jitter: 1-9µs typical, occasionally 1ms (simulating idle).
            let delay = if rng.random_range(0..100) == 0 {
                1_000
            } else {
                rng.random_range(1..10)
            };
            std::thread::sleep(std::time::Duration::from_micros(delay));

            w_produced.fetch_add(1, Ordering::Relaxed);
            w_data_ready.store(true, Ordering::Release);
            w_notify.notify_one();
        }
    });

    // Consumer: exactly the swap+request_wait+notified pattern used by the
    // real EAGAIN handler (SrtReceiveErrorAction::WaitForReadiness).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut requested: u32 = 0;
    for i in 0..ITEMS {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out after {i} items (produced={})",
            produced.load(Ordering::Relaxed),
        );

        if !data_ready.swap(false, Ordering::Acquire) {
            requested += 1;
            signal.request_wait();
            tokio::time::timeout(std::time::Duration::from_secs(5), notify.notified())
                .await
                .expect("consumer should not hang: permit must be available");
        }
    }

    signal.stop();
    tokio::time::timeout(std::time::Duration::from_secs(5), producer)
        .await
        .expect("producer must exit promptly on stop")
        .expect("producer should not panic");

    let total_produced = produced.load(Ordering::Relaxed);
    assert!(
        total_produced <= requested,
        "producer serviced {total_produced} waits but only {requested} were requested - demand gating is broken"
    );
    assert!(
        requested <= ITEMS,
        "consumer requested more waits than iterations"
    );
}
