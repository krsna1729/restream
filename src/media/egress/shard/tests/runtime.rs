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

        fn on_media_tick(&mut self) {
            let (lock, condvar) = &*self.probe.inner;
            *lock.lock().unwrap() += 1;
            condvar.notify_all();
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
