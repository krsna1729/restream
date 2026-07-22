use super::super::*;
use super::support::{
    BlockingBackend, Gate, Probe, ProbeBackend, ReadyFloodBackend, TimerBackend, config,
    output_spec,
};
use crate::media::egress::command::{EgressCommand, ShardId};
use std::time::Duration;

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

    assert!(probe.state().commands.is_empty());
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
