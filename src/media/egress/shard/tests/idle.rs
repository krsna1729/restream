use super::super::*;
use super::support::{Probe, ProbeBackend, TimerBackend, config, output_spec};
use crate::media::egress::command::{EgressCommand, ShardId};
use std::time::{Duration, Instant};

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

/// `drain_timeout` bounds how long a draining shard may stay parked, even
/// when `idle_wait` is far longer.
mod drain_bound {
    use super::*;
    use std::sync::{Arc, Mutex};

    const LONG_IDLE: Duration = Duration::from_secs(10);

    /// Never goes idle while draining: every idle poll asks for a far-future
    /// timer, so the shard cannot take the "fully idle, stop early" exit and
    /// only the drain deadline can end it.
    struct BusyWhileDrainingBackend;

    impl EgressShardBackend for BusyWhileDrainingBackend {
        fn on_command(&mut self, _command: EgressCommand) -> EgressShardCommandEffect {
            EgressShardCommandEffect::Continue
        }

        fn on_ready(&mut self) -> EgressShardCommandEffect {
            EgressShardCommandEffect::ScheduleTimer {
                output_id: OutputId::new("keepalive"),
                generation: 1,
                fire_at: Instant::now() + Duration::from_secs(600),
            }
        }
    }

    #[test]
    fn drain_deadline_beats_a_long_idle_wait() {
        let drain = Duration::from_millis(100);
        let config = EgressShardConfig::new(8, 4, 4, 4, LONG_IDLE)
            .unwrap()
            .with_drain_timeout(drain);
        let handle = EgressShardHandle::spawn(ShardId::new(0), config, BusyWhileDrainingBackend);

        let sent_at = Instant::now();
        assert_eq!(handle.try_send(EgressCommand::Shutdown), Ok(()));
        let snapshot = handle.shutdown_and_join();
        let elapsed = sent_at.elapsed();

        assert!(snapshot.stopped && !snapshot.panicked);
        assert!(
            elapsed >= drain,
            "stopped before the drain window: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "parked past the drain deadline (idle_wait is {LONG_IDLE:?}): {elapsed:?}"
        );
    }

    #[test]
    fn idle_drain_still_exits_before_the_drain_deadline() {
        let config = EgressShardConfig::new(8, 4, 4, 4, Duration::from_millis(10))
            .unwrap()
            .with_drain_timeout(Duration::from_secs(20));
        let probe = Probe::default();
        let handle = EgressShardHandle::spawn(
            ShardId::new(0),
            config,
            ProbeBackend {
                probe: probe.clone(),
            },
        );

        let sent_at = Instant::now();
        let snapshot = handle.shutdown_and_join();

        assert!(snapshot.stopped && !snapshot.panicked);
        assert!(
            sent_at.elapsed() < Duration::from_secs(10),
            "an idle draining shard waited out the drain window"
        );
        assert_eq!(probe.state().shutdowns, 1);
    }

    #[test]
    fn earlier_application_timer_still_wins_over_the_drain_deadline() {
        let drain = Duration::from_secs(1);
        let config = EgressShardConfig::new(8, 4, 4, 4, LONG_IDLE)
            .unwrap()
            .with_drain_timeout(drain);
        let probe = Probe::default();
        let handle = EgressShardHandle::spawn(
            ShardId::new(0),
            config,
            TimerBackend {
                probe: probe.clone(),
                delay: Duration::from_millis(100),
            },
        );
        assert_eq!(
            handle.try_send(EgressCommand::Add(output_spec("out-timer"))),
            Ok(())
        );
        probe.wait_for_commands(1);

        let sent_at = Instant::now();
        assert_eq!(handle.try_send(EgressCommand::Shutdown), Ok(()));
        probe.wait_for_timers(1);
        assert!(
            sent_at.elapsed() < drain / 2,
            "timer waited for the drain deadline: {:?}",
            sent_at.elapsed()
        );
        let snapshot = handle.shutdown_and_join();
        assert_eq!(snapshot.timers_processed, 1);
        assert!(snapshot.stopped && !snapshot.panicked);
    }

    /// Records the `max_wait` each idle wait is given once `Shutdown` has
    /// reached it, then waits on the command channel like the default.
    struct RecordingWaiter {
        shutdown_at: Option<Instant>,
        waits: Arc<Mutex<Vec<(Instant, Duration, Instant)>>>,
    }

    impl EgressShardBackend for RecordingWaiter {
        fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
            if matches!(command, EgressCommand::Shutdown) {
                self.shutdown_at = Some(Instant::now());
            }
            EgressShardCommandEffect::Continue
        }

        fn wait_idle(
            &mut self,
            commands: &flume::Receiver<EgressCommand>,
            max_wait: Duration,
        ) -> EgressShardIdleWake {
            if let Some(shutdown_at) = self.shutdown_at {
                self.waits
                    .lock()
                    .unwrap()
                    .push((Instant::now(), max_wait, shutdown_at));
            }
            match commands.recv_timeout(max_wait) {
                Ok(command) => EgressShardIdleWake::Command(command),
                Err(flume::RecvTimeoutError::Timeout) => EgressShardIdleWake::Timeout,
                Err(flume::RecvTimeoutError::Disconnected) => EgressShardIdleWake::Disconnected,
            }
        }
    }

    #[test]
    fn custom_waiter_max_wait_never_passes_the_drain_deadline() {
        let drain = Duration::from_millis(200);
        let config = EgressShardConfig::new(8, 4, 4, 4, LONG_IDLE)
            .unwrap()
            .with_drain_timeout(drain);
        let waits = Arc::new(Mutex::new(Vec::new()));
        let backend_waits = Arc::clone(&waits);
        let handle =
            EgressShardHandle::spawn_with(ShardId::new(0), config, move || RecordingWaiter {
                shutdown_at: None,
                waits: backend_waits,
            });

        assert_eq!(handle.try_send(EgressCommand::Shutdown), Ok(()));
        let snapshot = handle.shutdown_and_join();
        assert!(snapshot.stopped && !snapshot.panicked);

        let waits = waits.lock().unwrap();
        assert!(!waits.is_empty(), "no idle wait happened while draining");
        for (called_at, max_wait, shutdown_at) in waits.iter() {
            // The shard stamps `draining_until` just before the backend sees
            // `Shutdown`, so the backend's own stamp is a safe upper bound.
            // The shard computes `max_wait` a moment before it calls the
            // waiter, hence the slack (still far below `LONG_IDLE`).
            assert!(
                *called_at + *max_wait <= *shutdown_at + drain + Duration::from_millis(50),
                "max_wait {max_wait:?} runs past the drain deadline"
            );
        }
    }
}
