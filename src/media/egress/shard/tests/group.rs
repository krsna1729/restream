use super::super::*;
use super::support::{
    BlockingBackend, Gate, Probe, ProbeBackend, ReadyFloodBackend, ScriptBackend, config, manager,
    output_spec, spec_for_shard,
};
use crate::media::egress::command::OutputSpec;
use crate::media::egress::command::{EgressCommand, ShardId};
use crate::media::egress::manager::{
    DesiredOutput, EgressManager, EgressManagerCommandError, EgressManagerDispatchError,
    ManagerCommandOutcome,
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
fn manager_dispatch_to_group_converges_after_shard_queue_full() {
    let mut manager = manager(2);
    let gate = Gate::default();
    let survivor = Probe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(1, 1),
        vec![
            ScriptBackend::Blocking(BlockingBackend { gate: gate.clone() }),
            ScriptBackend::Probe(ProbeBackend {
                probe: survivor.clone(),
            }),
        ],
    )
    .unwrap();
    let outputs = specs_for_shard(&manager, ShardId::new(0), 3);
    let rejected_id = outputs[2].id.clone();

    assert!(matches!(
        manager.dispatch_to_group(EgressCommand::Add(outputs[0].clone()), &group),
        Ok(ManagerCommandOutcome::Enqueued { shard_id }) if shard_id == ShardId::new(0)
    ));
    gate.wait_until_entered();
    assert!(matches!(
        manager.dispatch_to_group(EgressCommand::Add(outputs[1].clone()), &group),
        Ok(ManagerCommandOutcome::Enqueued { shard_id }) if shard_id == ShardId::new(0)
    ));
    let full = manager.dispatch_to_group(EgressCommand::Add(outputs[2].clone()), &group);

    assert_eq!(
        full,
        Err(EgressManagerDispatchError::Dispatch {
            shard_id: ShardId::new(0),
            source: EgressShardGroupError::SendFailed {
                shard_id: ShardId::new(0),
                source: EgressShardSendError::Full,
            },
        })
    );
    assert!(manager.desired_output(&rejected_id).is_none());
    assert_eq!(manager.command_depth(ShardId::new(0)), 2);

    gate.release();
    wait_for_command_depth_at_least(&group, ShardId::new(0), 2);
    assert!(matches!(
        manager.dispatch_to_group(EgressCommand::Add(outputs[2].clone()), &group),
        Ok(ManagerCommandOutcome::Enqueued { shard_id }) if shard_id == ShardId::new(0)
    ));
    let snapshots = group.shutdown_and_join();

    assert_eq!(
        manager.desired_output(&rejected_id),
        Some(&DesiredOutput {
            id: rejected_id,
            generation: 1,
            shard_id: ShardId::new(0),
        })
    );
    assert!(snapshots.iter().all(|snapshot| snapshot.stopped));
}

#[test]
fn manager_dispatch_to_group_rejects_new_assignments_to_draining_shard() {
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
    let outputs = specs_for_shard(&manager, ShardId::new(0), 2);
    let active_id = outputs[0].id.clone();
    let rejected_id = outputs[1].id.clone();

    assert!(matches!(
        manager.dispatch_to_group(EgressCommand::Add(outputs[0].clone()), &group),
        Ok(ManagerCommandOutcome::Enqueued { shard_id }) if shard_id == ShardId::new(0)
    ));
    shard_zero.wait_for_commands(1);
    assert!(matches!(
        manager.dispatch_to_group(EgressCommand::DrainShard(ShardId::new(0)), &group),
        Ok(ManagerCommandOutcome::Enqueued { shard_id }) if shard_id == ShardId::new(0)
    ));
    shard_zero.wait_for_commands(2);

    let rejected = manager.dispatch_to_group(EgressCommand::Add(outputs[1].clone()), &group);

    assert_eq!(
        rejected,
        Err(EgressManagerDispatchError::Command(
            EgressManagerCommandError::ShardDraining {
                shard_id: ShardId::new(0),
            },
        ))
    );
    assert!(manager.desired_output(&rejected_id).is_none());
    assert_eq!(
        shard_zero.state().commands,
        vec![format!("add:{active_id}"), "drain:0".to_string()]
    );

    assert!(matches!(
        manager.dispatch_to_group(EgressCommand::Remove(active_id.clone()), &group),
        Ok(ManagerCommandOutcome::Enqueued { shard_id }) if shard_id == ShardId::new(0)
    ));
    shard_zero.wait_for_commands(3);
    assert!(manager.desired_output(&active_id).is_none());
    assert!(matches!(
        manager.dispatch_to_group(EgressCommand::Shutdown, &group),
        Ok(ManagerCommandOutcome::Broadcast { shard_count }) if shard_count == NonZeroU32::new(2).unwrap()
    ));
    let snapshots = group.shutdown_and_join();

    assert!(shard_one.state().commands.is_empty());
    assert!(snapshots.iter().all(|snapshot| snapshot.stopped));
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

fn specs_for_shard(manager: &EgressManager, shard_id: ShardId, count: usize) -> Vec<OutputSpec> {
    let mut specs = Vec::with_capacity(count);
    for index in 0..2_000 {
        let candidate = output_spec(&format!("out-full-{index}"));
        if manager.assign_spec(&candidate) == shard_id {
            specs.push(candidate);
            if specs.len() == count {
                return specs;
            }
        }
    }
    panic!("test fixture could not find {count} outputs for {shard_id}");
}

fn wait_for_command_depth_at_least(group: &EgressShardGroup, shard_id: ShardId, target: u64) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if group
            .snapshots()
            .iter()
            .any(|snapshot| snapshot.shard_id == shard_id && snapshot.commands_processed >= target)
        {
            return;
        }
        std::thread::yield_now();
    }
    panic!("timed out waiting for {shard_id:?} to process {target} commands");
}

#[test]
fn manager_replays_only_replaced_shard_outputs_after_panic() {
    let mut manager = manager(2);
    let survivor = Probe::default();
    let replacement = Probe::default();
    let panicked_output = spec_for_shard(&manager, ShardId::new(0));
    let survivor_output = spec_for_shard(&manager, ShardId::new(1));
    let panicked_output_id = panicked_output.id.clone();
    let survivor_output_id = survivor_output.id.clone();
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

    assert!(matches!(
        manager.dispatch_to_group(EgressCommand::Add(panicked_output), &group),
        Ok(ManagerCommandOutcome::Enqueued { shard_id }) if shard_id == ShardId::new(0)
    ));
    assert!(matches!(
        manager.dispatch_to_group(EgressCommand::Add(survivor_output), &group),
        Ok(ManagerCommandOutcome::Enqueued { shard_id }) if shard_id == ShardId::new(1)
    ));
    survivor.wait_for_commands(1);
    wait_for_panicked(&group, ShardId::new(0));

    assert_eq!(
        group.replace_panicked(config(4, 4), |_| ProbeBackend {
            probe: replacement.clone(),
        }),
        vec![ShardId::new(0)]
    );
    let replay = manager.dispatch_recreate_shard(ShardId::new(0), |shard_id, command| {
        group.try_send_to(shard_id, command)
    });

    assert_eq!(
        replay,
        Ok(ManagerCommandOutcome::Replayed {
            shard_id: ShardId::new(0),
            output_count: 1,
        })
    );
    replacement.wait_for_commands(1);
    let snapshots = group.shutdown_and_join();

    assert_eq!(
        replacement.state().commands,
        vec![format!("add:{panicked_output_id}")]
    );
    assert_eq!(
        survivor.state().commands,
        vec![format!("add:{survivor_output_id}")]
    );
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.stopped && !snapshot.panicked)
    );
}
