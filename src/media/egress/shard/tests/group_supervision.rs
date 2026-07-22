use super::super::*;
use super::support::{
    BlockingBackend, Gate, Probe, ProbeBackend, ReadyFloodBackend, ScriptBackend, config,
    output_spec,
};
use crate::media::egress::command::{EgressCommand, ShardId};
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

#[test]
fn shard_group_contains_panic_to_assigned_shard() {
    let survivor = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(4, 4),
        vec![
            ScriptBackend::Panic,
            ScriptBackend::Probe(ProbeBackend {
                probe: survivor.clone(),
            }),
        ],
    )
    .unwrap();

    assert_eq!(
        group.try_send_to(
            ShardId::new(0),
            EgressCommand::Add(output_spec("out-panic"))
        ),
        Ok(())
    );
    assert_eq!(
        group.try_send_to(
            ShardId::new(1),
            EgressCommand::Add(output_spec("out-survivor"))
        ),
        Ok(())
    );
    survivor.wait_for_commands(1);
    survivor.wait_for_media_ticks(1);
    let snapshots = group.shutdown_and_join();

    let panicked = snapshots
        .iter()
        .find(|snapshot| snapshot.shard_id == ShardId::new(0))
        .unwrap();
    let healthy = snapshots
        .iter()
        .find(|snapshot| snapshot.shard_id == ShardId::new(1))
        .unwrap();
    assert!(panicked.panicked);
    assert!(panicked.stopped);
    assert!(!healthy.panicked);
    assert!(healthy.stopped);
    assert_eq!(survivor.state().commands, vec!["add:out-survivor"]);
}

#[test]
fn panicked_shard_closes_command_path_without_stopping_other_shards() {
    let survivor = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(4, 4),
        vec![
            ScriptBackend::Panic,
            ScriptBackend::Probe(ProbeBackend {
                probe: survivor.clone(),
            }),
        ],
    )
    .unwrap();

    assert_eq!(
        group.try_send_to(
            ShardId::new(0),
            EgressCommand::Add(output_spec("out-panic"))
        ),
        Ok(())
    );
    wait_for_panicked(&group, ShardId::new(0));
    let panicked_send = group.try_send_to(
        ShardId::new(0),
        EgressCommand::Add(output_spec("out-after-panic")),
    );
    assert_eq!(
        group.try_send_to(
            ShardId::new(1),
            EgressCommand::Add(output_spec("out-survivor"))
        ),
        Ok(())
    );
    survivor.wait_for_commands(1);
    let snapshots = group.shutdown_and_join();

    assert_eq!(
        panicked_send,
        Err(EgressShardGroupError::SendFailed {
            shard_id: ShardId::new(0),
            source: EgressShardSendError::Closed,
        })
    );
    assert_eq!(survivor.state().commands, vec!["add:out-survivor"]);
    assert!(snapshots.iter().any(|snapshot| {
        snapshot.shard_id == ShardId::new(0) && snapshot.panicked && snapshot.stopped
    }));
    assert!(snapshots.iter().any(|snapshot| {
        snapshot.shard_id == ShardId::new(1) && !snapshot.panicked && snapshot.stopped
    }));
}

#[test]
fn shard_group_replaces_only_panicked_shards() {
    let survivor = Probe::default();
    let replacement = Probe::default();
    let mut group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(4, 4),
        vec![
            ScriptBackend::Panic,
            ScriptBackend::Probe(ProbeBackend {
                probe: survivor.clone(),
            }),
        ],
    )
    .unwrap();

    assert_eq!(
        group.try_send_to(
            ShardId::new(0),
            EgressCommand::Add(output_spec("out-panicked"))
        ),
        Ok(())
    );
    assert_eq!(
        group.try_send_to(
            ShardId::new(1),
            EgressCommand::Add(output_spec("out-survivor-before"))
        ),
        Ok(())
    );
    survivor.wait_for_commands(1);
    wait_for_panicked(&group, ShardId::new(0));

    let replaced = group.replace_panicked(config(4, 4), |_| ProbeBackend {
        probe: replacement.clone(),
    });

    assert_eq!(replaced, vec![ShardId::new(0)]);
    assert_eq!(
        group.try_send_to(
            ShardId::new(0),
            EgressCommand::Add(output_spec("out-replacement"))
        ),
        Ok(())
    );
    assert_eq!(
        group.try_send_to(
            ShardId::new(1),
            EgressCommand::Add(output_spec("out-survivor-after"))
        ),
        Ok(())
    );
    replacement.wait_for_commands(1);
    survivor.wait_for_commands(2);
    let snapshots = group.shutdown_and_join();

    assert_eq!(replacement.state().commands, vec!["add:out-replacement"]);
    assert_eq!(
        survivor.state().commands,
        vec!["add:out-survivor-before", "add:out-survivor-after"]
    );
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.stopped && !snapshot.panicked)
    );
}

#[test]
fn ready_flood_on_one_shard_does_not_starve_another_shard_command() {
    let flooding = Probe::default();
    let survivor = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        EgressShardConfig::new(16, 16, 2, 16, Duration::from_millis(10)).unwrap(),
        vec![
            ScriptBackend::ReadyFlood(ReadyFloodBackend {
                probe: flooding.clone(),
            }),
            ScriptBackend::Probe(ProbeBackend {
                probe: survivor.clone(),
            }),
        ],
    )
    .unwrap();

    assert_eq!(
        group.try_send_to(
            ShardId::new(0),
            EgressCommand::Add(output_spec("out-flooding"))
        ),
        Ok(())
    );
    flooding.wait_for_ready_events(2);
    assert_eq!(
        group.try_send_to(
            ShardId::new(1),
            EgressCommand::Add(output_spec("out-survivor"))
        ),
        Ok(())
    );
    survivor.wait_for_commands(1);
    survivor.wait_for_media_ticks(1);
    let snapshots = group.shutdown_and_join();

    let flooded_snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.shard_id == ShardId::new(0))
        .unwrap();
    let survivor_snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.shard_id == ShardId::new(1))
        .unwrap();
    assert!(flooding.state().ready_events >= 2);
    assert_eq!(survivor.state().commands, vec!["add:out-survivor"]);
    assert!(flooded_snapshot.metrics.ready_depth > 0);
    assert!(survivor_snapshot.commands_processed >= 1);
    assert!(snapshots.iter().all(|snapshot| snapshot.stopped));
    assert!(snapshots.iter().all(|snapshot| !snapshot.panicked));
}

#[test]
fn blocked_command_on_one_shard_does_not_starve_another_shard_command() {
    let gate = Gate::default();
    let survivor = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(4, 4),
        vec![
            ScriptBackend::Blocking(BlockingBackend { gate: gate.clone() }),
            ScriptBackend::Probe(ProbeBackend {
                probe: survivor.clone(),
            }),
        ],
    )
    .unwrap();

    assert_eq!(
        group.try_send_to(
            ShardId::new(0),
            EgressCommand::Add(output_spec("out-blocked"))
        ),
        Ok(())
    );
    gate.wait_until_entered();
    assert_eq!(
        group.try_send_to(
            ShardId::new(1),
            EgressCommand::Add(output_spec("out-survivor"))
        ),
        Ok(())
    );
    survivor.wait_for_commands(1);
    survivor.wait_for_media_ticks(1);
    gate.release();
    let snapshots = group.shutdown_and_join();

    let blocked_snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.shard_id == ShardId::new(0))
        .unwrap();
    let survivor_snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.shard_id == ShardId::new(1))
        .unwrap();
    assert_eq!(survivor.state().commands, vec!["add:out-survivor"]);
    assert!(blocked_snapshot.commands_processed >= 1);
    assert!(survivor_snapshot.commands_processed >= 1);
    assert!(snapshots.iter().all(|snapshot| snapshot.stopped));
    assert!(snapshots.iter().all(|snapshot| !snapshot.panicked));
}

#[test]
fn stalled_shard_heartbeat_does_not_trigger_panic_replacement() {
    let gate = Gate::default();
    let replacement = Probe::default();
    let mut group = EgressShardGroup::spawn(
        NonZeroU32::new(1).unwrap(),
        config(4, 4),
        vec![BlockingBackend { gate: gate.clone() }],
    )
    .unwrap();

    assert_eq!(
        group.try_send_to(
            ShardId::new(0),
            EgressCommand::Add(output_spec("out-stalled"))
        ),
        Ok(())
    );
    gate.wait_until_entered();
    let heartbeat = group.heartbeat(Instant::now(), Duration::ZERO);
    let replaced = group.replace_panicked(config(4, 4), |_| ProbeBackend {
        probe: replacement.clone(),
    });
    gate.release();
    let snapshots = group.shutdown_and_join();

    assert_eq!(heartbeat.len(), 1);
    assert_eq!(heartbeat[0].state, EgressShardHealth::Stalled);
    assert!(replaced.is_empty());
    assert!(replacement.state().commands.is_empty());
    assert!(snapshots.iter().all(|snapshot| snapshot.stopped));
    assert!(snapshots.iter().all(|snapshot| !snapshot.panicked));
}

fn wait_for_panicked(group: &EgressShardGroup, shard_id: ShardId) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if group
            .snapshots()
            .iter()
            .any(|snapshot| snapshot.shard_id == shard_id && snapshot.panicked)
        {
            return;
        }
        std::thread::yield_now();
    }
    panic!("timed out waiting for {shard_id:?} to report panic");
}
