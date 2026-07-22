use super::super::*;
use super::support::{
    Probe, ProbeBackend, ScriptBackend, config, manager, output_spec, spec_for_shard,
};
use crate::media::egress::command::{EgressCommand, ShardId};
use crate::media::egress::manager::{
    DesiredOutput, EgressManagerDispatchError, ManagerCommandOutcome,
};
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

#[test]
fn shard_group_starts_fixed_shards_and_routes_by_shard_id() {
    let shard_zero = Probe::default();
    let shard_one = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
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

    assert_eq!(group.shard_count(), 2);
    assert_eq!(
        group.try_send_to(ShardId::new(0), EgressCommand::Add(output_spec("out-zero"))),
        Ok(())
    );
    assert_eq!(
        group.try_send_to(ShardId::new(1), EgressCommand::Add(output_spec("out-one"))),
        Ok(())
    );
    shard_zero.wait_for_commands(1);
    shard_one.wait_for_commands(1);
    let snapshots = group.shutdown_and_join();

    assert_eq!(shard_zero.state().commands, vec!["add:out-zero"]);
    assert_eq!(shard_one.state().commands, vec!["add:out-one"]);
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.iter().all(|snapshot| snapshot.stopped));
}

#[test]
fn shard_group_heartbeat_reports_running_shard() {
    let probe = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(1).unwrap(),
        config(4, 4),
        vec![ProbeBackend {
            probe: probe.clone(),
        }],
    )
    .unwrap();

    probe.wait_for_media_ticks(1);
    let heartbeat = group.heartbeat(Instant::now(), Duration::from_secs(10));
    let snapshots = group.shutdown_and_join();

    assert_eq!(heartbeat.len(), 1);
    assert_eq!(heartbeat[0].state, EgressShardHealth::Healthy);
    assert_eq!(heartbeat[0].shard_id, ShardId::new(0));
    assert_eq!(snapshots.len(), 1);
}

#[test]
fn manager_dispatch_to_group_routes_add_to_assigned_thread() {
    let mut manager = manager(2);
    let shard_zero = Probe::default();
    let shard_one = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
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
    let output = spec_for_shard(&manager, ShardId::new(1));
    let output_id = output.id.clone();

    let result = manager.dispatch_to_group(EgressCommand::Add(output.clone()), &group);
    shard_one.wait_for_commands(1);
    let snapshots = group.shutdown_and_join();

    assert_eq!(
        result,
        Ok(ManagerCommandOutcome::Enqueued {
            shard_id: ShardId::new(1)
        })
    );
    assert!(shard_zero.state().commands.is_empty());
    assert_eq!(shard_one.state().commands, vec![format!("add:{output_id}")]);
    assert_eq!(
        manager.desired_output(&output_id),
        Some(&DesiredOutput {
            id: output_id,
            generation: 1,
            shard_id: ShardId::new(1),
        })
    );
    assert!(snapshots.iter().all(|snapshot| snapshot.stopped));
}

#[test]
fn manager_dispatch_to_group_preserves_state_when_group_rejects_shard() {
    let mut manager = manager(2);
    let shard_zero = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(1).unwrap(),
        config(4, 4),
        vec![ProbeBackend {
            probe: shard_zero.clone(),
        }],
    )
    .unwrap();
    let output = spec_for_shard(&manager, ShardId::new(1));
    let output_id = output.id.clone();

    let result = manager.dispatch_to_group(EgressCommand::Add(output), &group);
    let snapshots = group.shutdown_and_join();

    assert_eq!(
        result,
        Err(EgressManagerDispatchError::Dispatch {
            shard_id: ShardId::new(1),
            source: EgressShardGroupError::UnknownShard {
                shard_id: ShardId::new(1)
            },
        })
    );
    assert!(manager.desired_output(&output_id).is_none());
    assert_eq!(manager.command_depth(ShardId::new(1)), 0);
    assert!(shard_zero.state().commands.is_empty());
    assert_eq!(snapshots.len(), 1);
}

#[test]
fn shard_group_rejects_mismatched_backend_count() {
    let result = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(4, 4),
        vec![ProbeBackend {
            probe: Probe::default(),
        }],
    );

    assert!(matches!(
        result,
        Err(EgressShardGroupError::BackendCountMismatch {
            expected: 2,
            actual: 1,
        })
    ));
}

#[test]
fn shard_group_reports_unknown_shard_without_touching_live_shards() {
    let probe = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(1).unwrap(),
        config(4, 4),
        vec![ProbeBackend {
            probe: probe.clone(),
        }],
    )
    .unwrap();

    assert_eq!(
        group.try_send_to(
            ShardId::new(7),
            EgressCommand::Add(output_spec("out-missing"))
        ),
        Err(EgressShardGroupError::UnknownShard {
            shard_id: ShardId::new(7)
        })
    );
    let snapshots = group.shutdown_and_join();

    assert!(probe.state().commands.is_empty());
    assert_eq!(snapshots.len(), 1);
}

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
