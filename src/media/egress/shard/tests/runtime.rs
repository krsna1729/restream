use super::super::*;
use super::support::{
    BlockingBackend, Gate, Probe, ProbeBackend, ReadyFloodBackend, TimerBackend, config,
    output_spec,
};
use crate::media::egress::command::{EgressCommand, ShardId};
use std::time::{Duration, Instant};

#[test]
fn config_rejects_zero_capacity_and_budget() {
    assert_eq!(
        EgressShardConfig::new(0, 1, 1, 1, Duration::ZERO),
        Err(EgressShardConfigError::ZeroCommandCapacity)
    );
    assert_eq!(
        EgressShardConfig::new(1, 0, 1, 1, Duration::ZERO),
        Err(EgressShardConfigError::ZeroCommandBatch)
    );
    assert_eq!(
        EgressShardConfig::new(1, 1, 0, 1, Duration::ZERO),
        Err(EgressShardConfigError::ZeroReadyBatch)
    );
    assert_eq!(
        EgressShardConfig::new(1, 1, 1, 0, Duration::ZERO),
        Err(EgressShardConfigError::ZeroTimerBatch)
    );
}

#[test]
fn command_channel_is_bounded() {
    let gate = Gate::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(1, 1),
        BlockingBackend { gate: gate.clone() },
    );

    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-a"))),
        Ok(())
    );
    gate.wait_until_entered();
    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-b"))),
        Ok(())
    );
    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-c"))),
        Err(EgressShardSendError::Full)
    );
    gate.release();
    let snapshot = handle.shutdown_and_join();

    assert_eq!(snapshot.shard_id, ShardId::new(0));
    assert!(snapshot.stopped);
}

#[test]
fn failed_feed_wake_delivery_does_not_stick_the_coalescing_gate() {
    let gate = Gate::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(1, 1),
        BlockingBackend { gate: gate.clone() },
    );

    handle
        .try_send(EgressCommand::Add(output_spec("out-a")))
        .unwrap();
    gate.wait_until_entered();
    handle
        .try_send(EgressCommand::Add(output_spec("out-b")))
        .unwrap();

    assert_eq!(handle.deliver_feed_wake(), Err(EgressShardSendError::Full));
    assert!(!handle.wake_gate().is_pending());

    gate.release();
    let snapshot = handle.shutdown_and_join();
    assert!(snapshot.stopped);
}

#[test]
fn command_batch_budget_allows_media_ticks_during_flood() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(16, 2),
        ProbeBackend {
            probe: probe.clone(),
        },
    );

    for i in 0..6 {
        assert_eq!(
            handle.try_send(EgressCommand::Add(output_spec(&format!("out-{i}")))),
            Ok(())
        );
    }
    probe.wait_for_media_ticks(2);
    let snapshot = handle.shutdown_and_join();

    assert!(snapshot.commands_processed >= 4);
    assert_eq!(
        snapshot.metrics.commands_processed,
        snapshot.commands_processed
    );
    assert_eq!(snapshot.metrics.media_ticks, snapshot.media_ticks);
    assert_eq!(snapshot.metrics.shard_id, Some(ShardId::new(0)));
    assert!(snapshot.media_ticks >= 2);
    assert!(snapshot.stopped);
}

#[test]
fn command_batch_budget_services_ready_work_during_command_flood() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        EgressShardConfig::new(32, 2, 2, 16, Duration::from_millis(10)).unwrap(),
        ReadyFloodBackend {
            probe: probe.clone(),
        },
    );

    for i in 0..8 {
        assert_eq!(
            handle.try_send(EgressCommand::Add(output_spec(&format!(
                "out-ready-during-command-{i}"
            )))),
            Ok(())
        );
    }
    probe.wait_for_ready_events(2);
    let snapshot = handle.shutdown_and_join();

    assert!(probe.state().commands.len() >= 2);
    assert!(probe.state().ready_events >= 2);
    assert!(snapshot.metrics.ready_depth > 0);
    assert!(snapshot.commands_processed >= 2);
    assert!(snapshot.stopped);
    assert!(!snapshot.panicked);
}

#[test]
fn timer_batch_budget_allows_media_ticks_during_timer_flood() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        EgressShardConfig::new(16, 16, 16, 2, Duration::from_millis(10)).unwrap(),
        TimerBackend {
            probe: probe.clone(),
            delay: Duration::ZERO,
        },
    );

    for index in 0..6 {
        assert_eq!(
            handle.try_send(EgressCommand::Add(output_spec(&format!(
                "out-timer-{index}"
            )))),
            Ok(())
        );
    }
    probe.wait_for_timers(4);
    let running_snapshot = handle.snapshot();
    let snapshot = handle.shutdown_and_join();

    assert!(snapshot.timers_processed >= 4);
    assert_eq!(snapshot.metrics.timers_processed, snapshot.timers_processed);
    assert_eq!(
        snapshot.metrics.pending_timers,
        u32::try_from(snapshot.pending_timers).unwrap()
    );
    assert!(running_snapshot.media_ticks >= 1);
    assert!(snapshot.stopped);
}

#[test]
fn timer_batch_budget_allows_remove_during_timer_flood() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        EgressShardConfig::new(16, 16, 16, 2, Duration::from_millis(10)).unwrap(),
        TimerBackend {
            probe: probe.clone(),
            delay: Duration::ZERO,
        },
    );
    let removed = output_spec("out-timer-remove");
    let removed_id = removed.id.clone();

    assert_eq!(handle.try_send(EgressCommand::Add(removed)), Ok(()));
    for index in 0..6 {
        assert_eq!(
            handle.try_send(EgressCommand::Add(output_spec(&format!(
                "out-timer-flood-{index}"
            )))),
            Ok(())
        );
    }
    probe.wait_for_timers(4);
    assert_eq!(handle.try_send(EgressCommand::Remove(removed_id)), Ok(()));
    probe.wait_for_commands(8);
    let snapshot = handle.shutdown_and_join();

    assert!(
        probe
            .state()
            .commands
            .iter()
            .any(|command| command == "remove:out-timer-remove")
    );
    assert!(snapshot.timers_processed >= 4);
    assert!(snapshot.stopped);
    assert!(!snapshot.panicked);
}

#[test]
fn readiness_batch_budget_allows_shutdown_during_ready_flood() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        EgressShardConfig::new(16, 16, 2, 16, Duration::from_millis(10)).unwrap(),
        ReadyFloodBackend {
            probe: probe.clone(),
        },
    );

    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-ready-flood"))),
        Ok(())
    );
    probe.wait_for_ready_events(2);
    assert_eq!(handle.try_send(EgressCommand::Shutdown), Ok(()));
    let snapshot = handle.shutdown_and_join();

    assert!(probe.state().ready_events >= 2);
    assert_eq!(probe.state().shutdowns, 1);
    assert_eq!(
        snapshot.metrics.ready_depth,
        snapshot.metrics.ready_depth_hwm
    );
    assert!(snapshot.metrics.ready_depth > 0);
    assert!(snapshot.stopped);
}

#[test]
fn shutdown_keeps_the_loop_alive_for_the_drain_window_instead_of_stopping_immediately() {
    // Before the graceful-drain change, `Shutdown` made `process_command`
    // return `Stop` directly — the loop exited on the very next iteration,
    // and the backend never even saw the `Shutdown` command via
    // `on_command`. `ReadyFloodBackend` never goes idle on its own, so the
    // only way this shard can ever stop is the bounded drain deadline
    // actually being enforced — proving the loop keeps servicing ready
    // work for real wall-clock time after `Shutdown`, not zero.
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        EgressShardConfig::new(16, 16, 2, 16, Duration::from_millis(10))
            .unwrap()
            .with_drain_timeout(Duration::from_millis(100)),
        ReadyFloodBackend {
            probe: probe.clone(),
        },
    );

    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-ready-flood"))),
        Ok(())
    );
    probe.wait_for_ready_events(2);
    let ready_events_before_shutdown = probe.state().ready_events;

    let shutdown_sent_at = Instant::now();
    assert_eq!(handle.try_send(EgressCommand::Shutdown), Ok(()));
    let snapshot = handle.shutdown_and_join();
    let drain_elapsed = shutdown_sent_at.elapsed();

    assert!(
        probe.state().ready_events > ready_events_before_shutdown,
        "the shard must keep processing ready work after Shutdown, not stop on the next iteration"
    );
    assert!(
        drain_elapsed >= Duration::from_millis(50),
        "a backend that never goes idle must be kept alive for close to the drain window \
         (got {drain_elapsed:?}), not stopped immediately"
    );
    assert!(
        drain_elapsed < Duration::from_secs(2),
        "the drain deadline must still bound shutdown — a backend that never goes idle must \
         not hang it forever (got {drain_elapsed:?})"
    );
    assert!(snapshot.stopped);
    assert!(!snapshot.panicked);
}

#[test]
fn readiness_batch_budget_allows_remove_during_ready_flood() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        EgressShardConfig::new(16, 16, 2, 16, Duration::from_millis(10)).unwrap(),
        ReadyFloodBackend {
            probe: probe.clone(),
        },
    );
    let output = output_spec("out-ready-remove");
    let output_id = output.id.clone();

    assert_eq!(handle.try_send(EgressCommand::Add(output)), Ok(()));
    probe.wait_for_ready_events(2);
    assert_eq!(handle.try_send(EgressCommand::Remove(output_id)), Ok(()));
    probe.wait_for_commands(2);
    let snapshot = handle.shutdown_and_join();

    assert_eq!(
        probe.state().commands,
        vec![
            "add:out-ready-remove",
            "remove:out-ready-remove",
            "shutdown"
        ]
    );
    assert!(probe.state().ready_events >= 2);
    assert!(snapshot.metrics.ready_depth > 0);
    assert!(snapshot.stopped);
    assert!(!snapshot.panicked);
}

#[test]
fn stale_timer_generation_is_ignored_on_shard_thread() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(8, 4),
        TimerBackend {
            probe: probe.clone(),
            delay: Duration::from_millis(20),
        },
    );
    let mut first = output_spec("out-stale-timer");
    first.generation = 1;
    let mut second = output_spec("out-stale-timer");
    second.generation = 2;

    assert_eq!(handle.try_send(EgressCommand::Add(first)), Ok(()));
    assert_eq!(handle.try_send(EgressCommand::Update(second)), Ok(()));
    probe.wait_for_timers(1);
    let snapshot = handle.shutdown_and_join();

    assert_eq!(probe.state().timers, vec!["out-stale-timer:2"]);
    assert_eq!(snapshot.timers_processed, 1);
    assert_eq!(snapshot.metrics.timers_processed, 1);
    assert!(snapshot.stopped);
}

#[test]
fn earliest_timer_wakes_an_idle_shard_without_waiting_for_idle_poll() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        EgressShardConfig::new(8, 4, 4, 4, Duration::from_secs(5)).unwrap(),
        TimerBackend {
            probe: probe.clone(),
            delay: Duration::from_millis(20),
        },
    );
    let sent_at = Instant::now();

    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-deadline-wake"))),
        Ok(())
    );
    probe.wait_for_timers(1);
    assert!(
        sent_at.elapsed() < Duration::from_secs(1),
        "deadline waited for the fixed idle interval"
    );

    let snapshot = handle.shutdown_and_join();
    assert_eq!(snapshot.timers_processed, 1);
    assert!(snapshot.stopped);
}

#[test]
fn removed_output_timer_is_ignored_on_shard_thread() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(8, 4),
        TimerBackend {
            probe: probe.clone(),
            delay: Duration::from_millis(20),
        },
    );
    let output = output_spec("out-removed-timer");
    let output_id = output.id.clone();

    assert_eq!(handle.try_send(EgressCommand::Add(output)), Ok(()));
    assert_eq!(handle.try_send(EgressCommand::Remove(output_id)), Ok(()));
    probe.wait_for_commands(2);
    std::thread::sleep(Duration::from_millis(40));
    let snapshot = handle.shutdown_and_join();

    assert!(probe.state().timers.is_empty());
    assert_eq!(snapshot.timers_processed, 0);
    assert_eq!(snapshot.metrics.timers_processed, 0);
    assert!(snapshot.stopped);
}

#[test]
fn drain_for_other_shard_is_ignored_locally() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(1),
        config(4, 4),
        ProbeBackend {
            probe: probe.clone(),
        },
    );

    assert_eq!(
        handle.try_send(EgressCommand::DrainShard(ShardId::new(0))),
        Ok(())
    );
    probe.wait_for_media_ticks(1);
    let snapshot = handle.shutdown_and_join();

    // `DrainShard` for another shard is ignored locally, but this shard's
    // own `Shutdown` now reaches the backend (see `run_shard_thread`'s
    // graceful-drain change) — the other shard's drain command must not
    // show up, but this shard's own shutdown command does.
    assert_eq!(probe.state().commands, vec!["shutdown".to_string()]);
    assert_eq!(snapshot.shard_id, ShardId::new(1));
}

#[test]
fn shutdown_joins_without_leaking_thread() {
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(4, 4),
        ProbeBackend {
            probe: probe.clone(),
        },
    );

    let snapshot = handle.shutdown_and_join();

    assert!(snapshot.stopped);
    assert!(!snapshot.panicked);
    assert_eq!(probe.state().shutdowns, 1);
}

#[test]
fn repeated_shard_group_startup_shutdown_joins_every_thread() {
    for iteration in 0..8 {
        let shard_zero = Probe::default();
        let shard_one = Probe::default();
        let group = EgressShardGroup::spawn(
            std::num::NonZeroU32::new(2).unwrap(),
            config(4, 4),
            vec![
                ProbeBackend {
                    probe: shard_zero.clone(),
                },
                ProbeBackend {
                    probe: shard_one.clone(),
                },
            ],
        )
        .unwrap();

        shard_zero.wait_for_media_ticks(1);
        shard_one.wait_for_media_ticks(1);
        let snapshots = group.shutdown_and_join();

        assert_eq!(snapshots.len(), 2, "iteration {iteration}");
        assert!(snapshots.iter().all(|snapshot| snapshot.stopped));
        assert!(snapshots.iter().all(|snapshot| !snapshot.panicked));
        assert_eq!(shard_zero.state().shutdowns, 1);
        assert_eq!(shard_one.state().shutdowns, 1);
    }
}

/// Feed-wake delivery ends the idle sleep promptly: with a long idle wait,
/// media ticks only advance when the shard is woken, and a coalesced
/// `deliver_feed_wake` produces a tick well before the idle timeout.
#[test]
fn feed_wake_delivery_ends_idle_sleep_promptly() {
    use std::sync::{Arc, Condvar, Mutex};

    #[derive(Clone, Default)]
    struct TickProbe {
        inner: Arc<(Mutex<u64>, Condvar)>,
    }

    struct TickBackend {
        probe: TickProbe,
    }

    impl EgressShardBackend for TickBackend {
        fn on_command(&mut self, _command: EgressCommand) -> EgressShardCommandEffect {
            EgressShardCommandEffect::Continue
        }

        fn on_media_tick(&mut self) -> EgressShardCommandEffect {
            let (lock, condvar) = &*self.probe.inner;
            *lock.lock().unwrap() += 1;
            condvar.notify_all();
            EgressShardCommandEffect::Continue
        }
    }

    let idle_wait = Duration::from_secs(5);
    let config = EgressShardConfig::new(16, 4, 4, 4, idle_wait).unwrap();
    let probe = TickProbe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config,
        TickBackend {
            probe: probe.clone(),
        },
    );

    // Let the startup iterations drain into the idle sleep.
    std::thread::sleep(Duration::from_millis(200));
    let ticks_before = *probe.inner.0.lock().unwrap();

    let delivered_at = Instant::now();
    handle.deliver_feed_wake().unwrap();

    let (lock, condvar) = &*probe.inner;
    let guard = lock.lock().unwrap();
    let (guard, timeout) = condvar
        .wait_timeout_while(guard, Duration::from_secs(2), |ticks| {
            *ticks <= ticks_before
        })
        .unwrap();
    assert!(
        !timeout.timed_out(),
        "wake did not end the idle sleep: {} ticks before and after",
        *guard
    );
    drop(guard);
    assert!(
        delivered_at.elapsed() < idle_wait,
        "tick arrived only after the idle timeout — wake was lost"
    );

    handle.shutdown_and_join();
}

#[test]
fn idle_shard_polls_on_ready_periodically_without_any_external_trigger() {
    // A leaf waiting on real I/O readiness that isn't feed-related (a
    // handshake or negotiation response, most concretely) has no other way
    // to ever be rediscovered on an otherwise-quiet shard: `on_ready` --
    // the only thing that calls the native poller -- previously only ran
    // when something scheduled it via `EgressShardCommandEffect::ScheduleReady`
    // (a `FeedWake`, or a backend's own self-perpetuating request). A shard
    // with nothing scheduling that ever, ever again, would sit registered
    // but never actually polled. This proves the fix: `on_ready` now fires
    // on every idle-wait cycle even with zero commands, zero timers, and
    // zero feed activity -- no `Add`, no `FeedWake`, nothing at all sent to
    // this shard.
    let probe = Probe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(16, 2),
        ProbeBackend {
            probe: probe.clone(),
        },
    );

    probe.wait_for_ready_events(3);

    let snapshot = handle.shutdown_and_join();
    assert!(snapshot.stopped);
}

/// A backend that owns `Rc` state is `!Send`. It records the thread of its
/// construction, every command and its drop, so the test can prove all three
/// happened on the one shard thread.
mod non_send_backend {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};
    use std::thread::{self, ThreadId};

    type Seen = Arc<Mutex<Vec<(&'static str, ThreadId)>>>;

    struct RcBackend {
        commands: Rc<RefCell<u32>>,
        seen: Seen,
    }

    impl RcBackend {
        fn new(seen: Seen) -> Self {
            seen.lock().unwrap().push(("new", thread::current().id()));
            Self {
                commands: Rc::new(RefCell::new(0)),
                seen,
            }
        }
    }

    impl EgressShardBackend for RcBackend {
        fn on_command(&mut self, _command: EgressCommand) -> EgressShardCommandEffect {
            *self.commands.borrow_mut() += 1;
            self.seen
                .lock()
                .unwrap()
                .push(("command", thread::current().id()));
            EgressShardCommandEffect::Continue
        }
    }

    impl Drop for RcBackend {
        fn drop(&mut self) {
            self.seen
                .lock()
                .unwrap()
                .push(("drop", thread::current().id()));
        }
    }

    // Compile-time proof that `RcBackend` is `!Send`: if it ever became
    // `Send`, `AmbiguousIfSend<_>` would have two applicable impls and this
    // would fail to infer.
    trait AmbiguousIfSend<A> {
        fn probe() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
    const _: fn() = || <RcBackend as AmbiguousIfSend<_>>::probe();

    #[test]
    fn non_send_backend_runs_through_the_production_shard_loop() {
        let seen = Seen::default();
        let factory_seen = Arc::clone(&seen);
        let handle = EgressShardHandle::spawn_with(ShardId::new(0), config(4, 4), move || {
            RcBackend::new(factory_seen)
        });

        assert_eq!(
            handle.try_send(EgressCommand::Add(output_spec("out-a"))),
            Ok(())
        );
        let snapshot = handle.shutdown_and_join();
        assert!(snapshot.stopped);
        assert!(!snapshot.panicked);

        let seen = seen.lock().unwrap();
        let labels: Vec<_> = seen.iter().map(|(label, _)| *label).collect();
        assert_eq!(labels.first(), Some(&"new"));
        assert!(labels.contains(&"command"));
        assert_eq!(labels.last(), Some(&"drop"));
        let shard_thread = seen[0].1;
        assert_ne!(shard_thread, thread::current().id());
        assert!(
            seen.iter().all(|(_, thread)| *thread == shard_thread),
            "construction, use and drop must share one shard thread: {seen:?}"
        );
    }

    #[test]
    fn non_send_backends_build_on_their_own_group_shard_threads() {
        let seen = Seen::default();
        let group = EgressShardGroup::spawn_with(
            std::num::NonZeroU32::new(2).unwrap(),
            config(4, 4),
            |_| {
                let seen = Arc::clone(&seen);
                move || RcBackend::new(seen)
            },
        )
        .unwrap();
        let _ = group.shutdown_and_join();

        let seen = seen.lock().unwrap();
        let mut built: Vec<_> = seen
            .iter()
            .filter(|(label, _)| *label == "new")
            .map(|(_, thread)| *thread)
            .collect();
        built.sort_by_key(|thread| format!("{thread:?}"));
        built.dedup();
        assert_eq!(built.len(), 2, "each shard builds on its own thread");
        assert!(!built.contains(&thread::current().id()));
    }

    #[test]
    fn fallible_factory_error_is_returned_and_starts_no_shard() {
        let result = EgressShardHandle::try_spawn_with(ShardId::new(0), config(4, 4), || {
            Err::<RcBackend, _>("poller unavailable")
        });
        assert_eq!(result.err(), Some("poller unavailable"));
    }
}

/// The idle-wait seam: commands and backend activity both wake a shard that
/// is parked in a very long idle wait, and neither bypasses the shared loop.
mod idle_wait {
    use super::*;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    const LONG_IDLE: Duration = Duration::from_secs(30);
    /// Far below `LONG_IDLE`; generous enough not to flake on a loaded host.
    const PROMPT: Duration = Duration::from_secs(5);

    fn long_idle_config() -> EgressShardConfig {
        EgressShardConfig::new(16, 4, 4, 4, LONG_IDLE).unwrap()
    }

    #[derive(Default)]
    struct Seen {
        commands: usize,
        ready: usize,
        waits: usize,
    }

    /// Custom waiter: announces each wait, then either reports activity it
    /// was told about or waits on the command channel like the default.
    struct FakeIoBackend {
        seen: Arc<Mutex<Seen>>,
        entered: mpsc::Sender<Duration>,
        activity: mpsc::Receiver<()>,
        on_ready: mpsc::Sender<()>,
    }

    impl EgressShardBackend for FakeIoBackend {
        fn on_command(&mut self, _command: EgressCommand) -> EgressShardCommandEffect {
            self.seen.lock().unwrap().commands += 1;
            EgressShardCommandEffect::Continue
        }

        fn on_ready(&mut self) -> EgressShardCommandEffect {
            self.seen.lock().unwrap().ready += 1;
            let _ = self.on_ready.send(());
            EgressShardCommandEffect::Continue
        }

        fn wait_idle(
            &mut self,
            commands: &flume::Receiver<EgressCommand>,
            max_wait: Duration,
        ) -> EgressShardIdleWake {
            self.seen.lock().unwrap().waits += 1;
            let _ = self.entered.send(max_wait);
            let deadline = Instant::now() + max_wait;
            loop {
                if self.activity.try_recv().is_ok() {
                    return EgressShardIdleWake::BackendActivity;
                }
                let slice = deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(5));
                match commands.recv_timeout(slice) {
                    Ok(command) => return EgressShardIdleWake::Command(command),
                    Err(flume::RecvTimeoutError::Disconnected) => {
                        return EgressShardIdleWake::Disconnected;
                    }
                    Err(flume::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                        return EgressShardIdleWake::Timeout;
                    }
                    Err(flume::RecvTimeoutError::Timeout) => {}
                }
            }
        }
    }

    struct Fixture {
        handle: EgressShardHandle,
        seen: Arc<Mutex<Seen>>,
        entered: mpsc::Receiver<Duration>,
        activity: mpsc::Sender<()>,
        ready: mpsc::Receiver<()>,
    }

    fn spawn_fake() -> Fixture {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (entered_tx, entered) = mpsc::channel();
        let (activity, activity_rx) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        let backend_seen = Arc::clone(&seen);
        let handle =
            EgressShardHandle::spawn_with(ShardId::new(0), long_idle_config(), move || {
                FakeIoBackend {
                    seen: backend_seen,
                    entered: entered_tx,
                    activity: activity_rx,
                    on_ready: ready_tx,
                }
            });
        Fixture {
            handle,
            seen,
            entered,
            activity,
            ready,
        }
    }

    /// The shard has reached its idle wait and is bounded by the long idle
    /// deadline, not by any application timer.
    fn wait_until_parked(fixture: &Fixture) {
        let bound = fixture.entered.recv_timeout(PROMPT).expect("shard idles");
        assert!(bound > Duration::from_secs(10), "max_wait was {bound:?}");
    }

    #[test]
    fn command_wakes_a_long_idle_shard_promptly() {
        let fixture = spawn_fake();
        wait_until_parked(&fixture);
        // The idle poll after startup already ran; only count what follows.
        let started = Instant::now();

        assert_eq!(
            fixture
                .handle
                .try_send(EgressCommand::Add(output_spec("out-a"))),
            Ok(())
        );
        // The command reaches the backend through the ordinary path, and the
        // shard loops back into a fresh wait.
        fixture
            .entered
            .recv_timeout(PROMPT)
            .expect("shard re-idles");
        assert!(started.elapsed() < PROMPT, "idle_wait is {LONG_IDLE:?}");
        assert_eq!(fixture.seen.lock().unwrap().commands, 1);
        // Dropping the last sender wakes the wait as `Disconnected`; a joined
        // `Shutdown` would sit out one full idle wait first.
        drop(fixture.handle);
    }

    #[test]
    fn default_waiter_wakes_a_long_idle_shard_promptly() {
        let probe = Probe::default();
        let handle = EgressShardHandle::spawn(
            ShardId::new(0),
            long_idle_config(),
            ProbeBackend {
                probe: probe.clone(),
            },
        );
        // The first loop iteration idles before it ticks, so wake it once and
        // wait for that iteration to finish; the shard is then parked again.
        assert_eq!(
            handle.try_send(EgressCommand::Add(output_spec("out-a"))),
            Ok(())
        );
        probe.wait_for_media_ticks(1);

        let started = Instant::now();
        assert_eq!(
            handle.try_send(EgressCommand::Add(output_spec("out-b"))),
            Ok(())
        );
        probe.wait_for_commands(2);
        assert!(started.elapsed() < PROMPT, "idle_wait is {LONG_IDLE:?}");
        drop(handle);
    }

    #[test]
    fn backend_activity_runs_on_ready_without_any_command_or_timer() {
        let fixture = spawn_fake();
        wait_until_parked(&fixture);
        // Drain the single idle poll that follows the startup wait, if any.
        while fixture.ready.try_recv().is_ok() {}
        let ready_before = fixture.seen.lock().unwrap().ready;

        fixture.activity.send(()).unwrap();
        fixture.ready.recv_timeout(PROMPT).expect("on_ready ran");

        let seen = fixture.seen.lock().unwrap();
        assert!(seen.ready > ready_before);
        assert_eq!(seen.commands, 0, "no command, and no FeedWake, was sent");
        drop(seen);
        drop(fixture.handle);
    }

    #[test]
    fn backend_activity_does_not_bypass_the_ready_budget() {
        // One activity wake schedules exactly one ready visit.
        let fixture = spawn_fake();
        wait_until_parked(&fixture);
        while fixture.ready.try_recv().is_ok() {}
        let ready_before = fixture.seen.lock().unwrap().ready;

        fixture.activity.send(()).unwrap();
        fixture.ready.recv_timeout(PROMPT).expect("on_ready ran");
        // The shard re-enters its idle wait after one visit.
        fixture
            .entered
            .recv_timeout(PROMPT)
            .expect("shard re-idles");
        assert_eq!(fixture.seen.lock().unwrap().ready, ready_before + 1);
        // Dropping the last sender wakes the wait as `Disconnected`; a joined
        // `Shutdown` would sit out one full idle wait first.
        drop(fixture.handle);
    }

    /// Waits by polling `recv_async()` by hand, the way a backend awaiting
    /// inside its own runtime would, instead of using the blocking receive.
    struct AsyncWaitBackend {
        commands: mpsc::Sender<()>,
    }

    impl EgressShardBackend for AsyncWaitBackend {
        fn on_command(&mut self, _command: EgressCommand) -> EgressShardCommandEffect {
            let _ = self.commands.send(());
            EgressShardCommandEffect::Continue
        }

        fn wait_idle(
            &mut self,
            commands: &flume::Receiver<EgressCommand>,
            max_wait: Duration,
        ) -> EgressShardIdleWake {
            use std::future::Future;
            use std::task::{Context, Poll, Waker};
            let mut cx = Context::from_waker(Waker::noop());
            let mut recv = std::pin::pin!(commands.recv_async());
            let deadline = Instant::now() + max_wait;
            loop {
                match recv.as_mut().poll(&mut cx) {
                    Poll::Ready(Ok(command)) => return EgressShardIdleWake::Command(command),
                    Poll::Ready(Err(_)) => return EgressShardIdleWake::Disconnected,
                    Poll::Pending if Instant::now() >= deadline => {
                        return EgressShardIdleWake::Timeout;
                    }
                    Poll::Pending => std::thread::sleep(Duration::from_millis(1)),
                }
            }
        }
    }

    #[test]
    fn command_receiver_supports_future_based_receive() {
        let (tx, rx) = mpsc::channel();
        let handle =
            EgressShardHandle::spawn_with(ShardId::new(0), long_idle_config(), move || {
                AsyncWaitBackend { commands: tx }
            });
        assert_eq!(
            handle.try_send(EgressCommand::Add(output_spec("out-a"))),
            Ok(())
        );
        rx.recv_timeout(PROMPT)
            .expect("command delivered via recv_async");
        drop(handle);
    }

    #[test]
    fn command_channel_reports_closed_once_the_shard_has_stopped() {
        let probe = Probe::default();
        let handle =
            EgressShardHandle::spawn(ShardId::new(0), config(2, 2), ProbeBackend { probe });
        let wake = handle.feed_wake_handle();
        let sender_handle = handle;
        let snapshot = sender_handle.shutdown_and_join();
        assert!(snapshot.stopped);

        assert_eq!(wake.deliver(), Err(EgressShardSendError::Closed));
        // A failed delivery leaves the coalescing gate clear for a retry.
        assert_eq!(wake.deliver(), Err(EgressShardSendError::Closed));
    }
}
