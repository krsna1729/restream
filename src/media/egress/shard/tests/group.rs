use super::super::*;
use super::support::{
    BlockingBackend, Gate, Probe, ProbeBackend, ScriptBackend, config, manager, output_spec,
    spec_for_shard,
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

    assert_eq!(
        shard_zero.state().commands,
        vec!["add:out-zero", "shutdown"]
    );
    assert_eq!(shard_one.state().commands, vec!["add:out-one", "shutdown"]);
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
    assert_eq!(shard_zero.state().commands, vec!["shutdown".to_string()]);
    assert_eq!(
        shard_one.state().commands,
        vec![format!("add:{output_id}"), "shutdown".to_string()]
    );
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
    assert_eq!(shard_zero.state().commands, vec!["shutdown".to_string()]);
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

    assert_eq!(shard_one.state().commands, vec!["shutdown".to_string()]);
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

    assert_eq!(probe.state().commands, vec!["shutdown".to_string()]);
    assert_eq!(snapshots.len(), 1);
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
    let _expected_panic_silencer = crate::media::test_support::silence_expected_panics();
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
        vec![format!("add:{panicked_output_id}"), "shutdown".to_string()]
    );
    assert_eq!(
        survivor.state().commands,
        vec![format!("add:{survivor_output_id}"), "shutdown".to_string()]
    );
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.stopped && !snapshot.panicked)
    );
}

// ---------------------------------------------------------------------------
// Dynamic shard scaling: grow / shrink
// ---------------------------------------------------------------------------

#[test]
fn grow_spawns_one_handle_at_the_next_shard_id() {
    let probe = Probe::default();
    let mut group = EgressShardGroup::spawn(
        NonZeroU32::new(1).unwrap(),
        config(4, 4),
        vec![ProbeBackend {
            probe: probe.clone(),
        }],
    )
    .unwrap();

    let new_probe = Probe::default();
    let shard_id = group.grow(
        config(4, 4),
        ProbeBackend {
            probe: new_probe.clone(),
        },
    );

    assert_eq!(shard_id, ShardId::new(1));
    assert_eq!(group.shard_count(), 2);
    assert_eq!(
        group.try_send_to(shard_id, EgressCommand::Add(output_spec("out-grown"))),
        Ok(())
    );
    new_probe.wait_for_commands(1);
    let snapshots = group.shutdown_and_join();
    assert_eq!(snapshots.len(), 2);
}

#[test]
fn shrink_removes_and_shuts_down_the_highest_index_handle() {
    let survivor = Probe::default();
    let doomed = Probe::default();
    let mut group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(4, 4),
        vec![
            ProbeBackend {
                probe: survivor.clone(),
            },
            ProbeBackend {
                probe: doomed.clone(),
            },
        ],
    )
    .unwrap();

    let shard_id = group.shrink().expect("group had a shard to shrink");

    assert_eq!(shard_id, ShardId::new(1));
    assert_eq!(group.shard_count(), 1);
    // The removed shard's handle is gone: routing to it now fails instead
    // of silently succeeding against a dead thread.
    assert!(matches!(
        group.try_send_to(shard_id, EgressCommand::FeedWake),
        Err(EgressShardGroupError::UnknownShard { shard_id: unknown }) if unknown == shard_id
    ));
    // `shrink` detaches rather than joins (see `EgressShardHandle::shutdown_detached`),
    // so the doomed shard's graceful drain runs in the background --
    // poll instead of asserting synchronously.
    doomed.wait_for_commands(1);
    assert_eq!(doomed.state().commands, vec!["shutdown".to_string()]);

    let snapshots = group.shutdown_and_join();
    assert_eq!(snapshots.len(), 1);
}

#[test]
fn shrink_on_a_single_shard_group_leaves_it_empty() {
    let probe = Probe::default();
    let mut group = EgressShardGroup::spawn(
        NonZeroU32::new(1).unwrap(),
        config(4, 4),
        vec![ProbeBackend { probe }],
    )
    .unwrap();

    let shrunk = group.shrink();
    assert!(shrunk.is_some());
    assert_eq!(group.shard_count(), 0);
    assert!(group.shrink().is_none());

    let snapshots = group.shutdown_and_join();
    assert!(snapshots.is_empty());
}

mod grow_shrink_proptests {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone, Copy)]
    enum GrowShrinkOp {
        Grow,
        Shrink,
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Arbitrary interleavings of grow/shrink from a small starting
        /// group never panic, `shard_count()` always matches the net
        /// effect of the sequence, and every surviving handle's `ShardId`
        /// stays exactly `0..shard_count()` with no gaps or duplicates.
        #[test]
        fn grow_shrink_sequences_keep_shard_ids_dense(
            start in 1u32..4,
            ops in prop::collection::vec(
                prop_oneof![Just(GrowShrinkOp::Grow), Just(GrowShrinkOp::Shrink)],
                0..20,
            ),
        ) {
            let probes: Vec<Probe> = (0..start).map(|_| Probe::default()).collect();
            let mut group = EgressShardGroup::spawn(
                NonZeroU32::new(start).unwrap(),
                config(4, 4),
                probes.into_iter().map(|probe| ProbeBackend { probe }).collect(),
            )
            .unwrap();

            let mut expected_count = start as usize;
            for op in ops {
                match op {
                    GrowShrinkOp::Grow => {
                        let probe = Probe::default();
                        let shard_id = group.grow(config(4, 4), ProbeBackend { probe });
                        prop_assert_eq!(shard_id, ShardId::new(expected_count as u32));
                        expected_count += 1;
                    }
                    GrowShrinkOp::Shrink => {
                        let result = group.shrink();
                        if expected_count == 0 {
                            prop_assert!(result.is_none());
                        } else {
                            let shard_id = result.expect("expected a shard to shrink");
                            prop_assert_eq!(shard_id, ShardId::new(expected_count as u32 - 1));
                            expected_count -= 1;
                        }
                    }
                }
                prop_assert_eq!(group.shard_count(), expected_count);
            }

            group.shutdown_and_join();
        }
    }
}
