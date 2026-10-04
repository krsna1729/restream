use super::*;
use crate::media::egress::command::{FeedId, ProtocolSpec};
use crate::media::egress::policy::LeafPolicy;

fn manager(shards: u32) -> EgressManager {
    EgressManager::new(EgressManagerConfig::new(shards, 128).unwrap())
}

fn spec(id: &str) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation: 1,
        feed: FeedId::new("feed-1"),
        protocol: ProtocolSpec::Rtmp {
            url: "rtmp://localhost/live".to_string(),
            tls: false,
        },
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}

fn sink_spec(id: &str, generation: u64) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation,
        feed: FeedId::new("feed-1"),
        protocol: ProtocolSpec::Sink,
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}

#[test]
fn config_rejects_zero_shards() {
    assert_eq!(
        EgressManagerConfig::new(0, 128),
        Err(EgressManagerConfigError::ZeroShardCount)
    );
}

#[test]
fn config_rejects_zero_command_capacity() {
    assert_eq!(
        EgressManagerConfig::new(1, 0),
        Err(EgressManagerConfigError::ZeroCommandCapacity)
    );
}

#[test]
fn same_output_is_assigned_to_the_same_shard() {
    let manager = manager(8);
    let first = manager.assign_output(&OutputId::new("out-a"));
    let second = manager.assign_output(&OutputId::new("out-a"));

    assert_eq!(first, second);
}

#[test]
fn assignment_is_bounded_by_configured_shard_count() {
    let manager = manager(3);

    for i in 0..1_000 {
        let shard = manager.assign_output(&OutputId::new(format!("out-{i}")));
        assert!(shard.index() < 3);
    }
}

#[test]
fn assignment_uses_output_identity_not_feed_identity() {
    let manager = manager(16);
    let first = manager.assign_output(&OutputId::new("pipeline-a/out-1"));
    let second = manager.assign_output(&OutputId::new("pipeline-a/out-2"));

    assert_ne!(first, second);
}

#[test]
fn spec_assignment_uses_spec_output_id() {
    let manager = manager(8);
    let output_spec = spec("out-from-spec");

    assert_eq!(
        manager.assign_spec(&output_spec),
        manager.assign_output(&output_spec.id)
    );
}

#[test]
fn add_command_records_desired_output_and_enqueues_to_assigned_shard() {
    let mut manager = manager(8);
    let output_spec = spec("out-add");
    let expected_shard = manager.assign_spec(&output_spec);

    let outcome = manager.apply_command(EgressCommand::Add(output_spec.clone()));

    assert_eq!(
        outcome,
        Ok(ManagerCommandOutcome::Enqueued {
            shard_id: expected_shard
        })
    );
    assert_eq!(
        manager.desired_output(&output_spec.id).map(|desired| (
            desired.id().clone(),
            desired.generation(),
            desired.shard_id()
        )),
        Some((output_spec.id, 1, expected_shard))
    );
    assert_eq!(manager.command_depth(expected_shard), 1);
}

#[test]
fn duplicate_generation_is_idempotent_without_reenqueue() {
    let mut manager = manager(4);
    let output_spec = spec("out-dup");
    let shard_id = manager.assign_spec(&output_spec);

    assert!(matches!(
        manager.apply_command(EgressCommand::Add(output_spec.clone())),
        Ok(ManagerCommandOutcome::Enqueued { .. })
    ));
    let duplicate = manager.apply_command(EgressCommand::Update(output_spec));

    assert_eq!(
        duplicate,
        Ok(ManagerCommandOutcome::AlreadyCurrent { shard_id })
    );
    assert_eq!(manager.command_depth(shard_id), 1);
}

#[test]
fn duplicate_add_is_idempotent_without_reenqueue() {
    let mut manager = manager(4);
    let output_spec = spec("out-add-dup");
    let shard_id = manager.assign_spec(&output_spec);

    assert!(matches!(
        manager.apply_command(EgressCommand::Add(output_spec.clone())),
        Ok(ManagerCommandOutcome::Enqueued { .. })
    ));
    let duplicate = manager.apply_command(EgressCommand::Add(output_spec));

    assert_eq!(
        duplicate,
        Ok(ManagerCommandOutcome::AlreadyCurrent { shard_id })
    );
    assert_eq!(manager.command_depth(shard_id), 1);
}

#[test]
fn stale_generation_is_ignored_without_reenqueue() {
    let mut manager = manager(4);
    let mut current = spec("out-stale");
    current.generation = 3;
    let mut stale = spec("out-stale");
    stale.generation = 2;
    let shard_id = manager.assign_spec(&current);

    assert!(matches!(
        manager.apply_command(EgressCommand::Add(current)),
        Ok(ManagerCommandOutcome::Enqueued { .. })
    ));
    let stale_result = manager.apply_command(EgressCommand::Update(stale));

    assert_eq!(
        stale_result,
        Ok(ManagerCommandOutcome::IgnoredStale { shard_id })
    );
    assert_eq!(manager.command_depth(shard_id), 1);
}

#[test]
fn newer_generation_replaces_desired_output_and_enqueues_once() {
    let mut manager = manager(4);
    let mut first = spec("out-update");
    first.generation = 1;
    let mut second = spec("out-update");
    second.generation = 2;
    let shard_id = manager.assign_spec(&first);

    assert!(matches!(
        manager.apply_command(EgressCommand::Add(first.clone())),
        Ok(ManagerCommandOutcome::Enqueued { .. })
    ));
    assert!(matches!(
        manager.apply_command(EgressCommand::Update(second.clone())),
        Ok(ManagerCommandOutcome::Enqueued { .. })
    ));

    assert_eq!(
        manager.desired_output(&first.id).map(|desired| (
            desired.id().clone(),
            desired.generation(),
            desired.shard_id()
        )),
        Some((first.id, 2, shard_id))
    );
    assert_eq!(manager.command_depth(shard_id), 2);
}

#[test]
fn remove_command_is_idempotent_after_first_enqueue() {
    let mut manager = manager(4);
    let output_spec = spec("out-remove");
    let output_id = output_spec.id.clone();
    let shard_id = manager.assign_spec(&output_spec);

    assert!(matches!(
        manager.apply_command(EgressCommand::Add(output_spec)),
        Ok(ManagerCommandOutcome::Enqueued { .. })
    ));
    let removed = manager.apply_command(EgressCommand::Remove(output_id.clone()));
    let duplicate = manager.apply_command(EgressCommand::Remove(output_id.clone()));

    assert_eq!(removed, Ok(ManagerCommandOutcome::Enqueued { shard_id }));
    assert_eq!(duplicate, Ok(ManagerCommandOutcome::AlreadyRemoved));
    assert!(manager.desired_output(&output_id).is_none());
    assert_eq!(manager.command_depth(shard_id), 2);
}

#[test]
fn sink_spec_uses_common_lifecycle_command_contract() {
    let mut manager = manager(4);
    let first = sink_spec("out-sink-lifecycle", 2);
    let duplicate = sink_spec("out-sink-lifecycle", 2);
    let stale = sink_spec("out-sink-lifecycle", 1);
    let updated = sink_spec("out-sink-lifecycle", 3);
    let output_id = first.id.clone();
    let shard_id = manager.assign_spec(&first);

    assert_eq!(
        manager.apply_command(EgressCommand::Add(first)),
        Ok(ManagerCommandOutcome::Enqueued { shard_id })
    );
    assert_eq!(
        manager.apply_command(EgressCommand::Update(duplicate)),
        Ok(ManagerCommandOutcome::AlreadyCurrent { shard_id })
    );
    assert_eq!(
        manager.apply_command(EgressCommand::Update(stale)),
        Ok(ManagerCommandOutcome::IgnoredStale { shard_id })
    );
    assert_eq!(
        manager.apply_command(EgressCommand::Update(updated)),
        Ok(ManagerCommandOutcome::Enqueued { shard_id })
    );
    assert_eq!(
        manager.desired_output(&output_id).map(|desired| (
            desired.id().clone(),
            desired.generation(),
            desired.shard_id()
        )),
        Some((output_id.clone(), 3, shard_id))
    );
    assert_eq!(
        manager.apply_command(EgressCommand::Remove(output_id.clone())),
        Ok(ManagerCommandOutcome::Enqueued { shard_id })
    );
    assert_eq!(
        manager.apply_command(EgressCommand::Remove(output_id.clone())),
        Ok(ManagerCommandOutcome::AlreadyRemoved)
    );
    assert!(manager.desired_output(&output_id).is_none());
    assert_eq!(
        manager.apply_command(EgressCommand::Shutdown),
        Ok(ManagerCommandOutcome::Broadcast {
            shard_count: NonZeroU32::new(4).unwrap()
        })
    );
}

#[test]
fn remove_preserves_desired_output_when_channel_is_full() {
    let mut manager = EgressManager::new(EgressManagerConfig::new(1, 1).unwrap());
    let output_spec = spec("out-remove-full");
    let output_id = output_spec.id.clone();

    assert!(matches!(
        manager.apply_command(EgressCommand::Add(output_spec.clone())),
        Ok(ManagerCommandOutcome::Enqueued { .. })
    ));
    assert_eq!(
        manager.apply_command(EgressCommand::Remove(output_id.clone())),
        Err(EgressManagerCommandError::CommandChannelFull {
            shard_id: ShardId::new(0)
        })
    );

    assert_eq!(
        manager.desired_output(&output_id).map(|desired| (
            desired.id().clone(),
            desired.generation(),
            desired.shard_id()
        )),
        Some((output_id, 1, ShardId::new(0)))
    );
}

#[test]
fn full_command_channel_fails_visibly_without_state_change() {
    let mut manager = EgressManager::new(EgressManagerConfig::new(1, 1).unwrap());
    let first = spec("out-first");
    let second = spec("out-second");

    assert_eq!(
        manager.apply_command(EgressCommand::Add(first.clone())),
        Ok(ManagerCommandOutcome::Enqueued {
            shard_id: ShardId::new(0)
        })
    );
    assert_eq!(
        manager.apply_command(EgressCommand::Add(second.clone())),
        Err(EgressManagerCommandError::CommandChannelFull {
            shard_id: ShardId::new(0)
        })
    );

    assert!(manager.desired_output(&first.id).is_some());
    assert!(manager.desired_output(&second.id).is_none());
}

#[test]
fn completing_command_capacity_allows_next_admission() {
    let mut manager = EgressManager::new(EgressManagerConfig::new(1, 1).unwrap());
    let first = spec("out-first");
    let second = spec("out-second");

    assert!(matches!(
        manager.apply_command(EgressCommand::Add(first)),
        Ok(ManagerCommandOutcome::Enqueued { .. })
    ));
    manager.complete_one_command(ShardId::new(0));
    assert!(matches!(
        manager.apply_command(EgressCommand::Add(second)),
        Ok(ManagerCommandOutcome::Enqueued { .. })
    ));
}

#[test]
fn shutdown_broadcasts_once_to_every_shard() {
    let mut manager = manager(3);

    let first = manager.apply_command(EgressCommand::Shutdown);
    let second = manager.apply_command(EgressCommand::Shutdown);

    assert_eq!(
        first,
        Ok(ManagerCommandOutcome::Broadcast {
            shard_count: NonZeroU32::new(3).unwrap()
        })
    );
    assert_eq!(second, Ok(ManagerCommandOutcome::AlreadyShuttingDown));
    assert_eq!(manager.command_depth(ShardId::new(0)), 1);
    assert_eq!(manager.command_depth(ShardId::new(1)), 1);
    assert_eq!(manager.command_depth(ShardId::new(2)), 1);
}

#[test]
fn shutdown_does_not_partially_broadcast_when_any_shard_is_full() {
    let mut manager = EgressManager::new(EgressManagerConfig::new(3, 1).unwrap());

    assert!(matches!(
        manager.apply_command(EgressCommand::DrainShard(ShardId::new(1))),
        Ok(ManagerCommandOutcome::Enqueued { .. })
    ));
    let shutdown = manager.apply_command(EgressCommand::Shutdown);

    assert_eq!(
        shutdown,
        Err(EgressManagerCommandError::CommandChannelFull {
            shard_id: ShardId::new(1)
        })
    );
    assert_eq!(manager.command_depth(ShardId::new(0)), 0);
    assert_eq!(manager.command_depth(ShardId::new(1)), 1);
    assert_eq!(manager.command_depth(ShardId::new(2)), 0);
    assert_eq!(
        manager.apply_command(EgressCommand::Shutdown),
        Err(EgressManagerCommandError::CommandChannelFull {
            shard_id: ShardId::new(1)
        })
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendFailure {
    Closed,
}

#[test]
fn failed_add_dispatch_preserves_manager_state() {
    let mut manager = manager(4);
    let output_spec = spec("out-dispatch-add");
    let output_id = output_spec.id.clone();
    let expected_shard = manager.assign_spec(&output_spec);

    let result = manager.dispatch_command(EgressCommand::Add(output_spec), |shard_id, command| {
        assert_eq!(shard_id, expected_shard);
        assert!(matches!(command, EgressCommand::Add(_)));
        Err(SendFailure::Closed)
    });

    assert_eq!(
        result,
        Err(EgressManagerDispatchError::Dispatch {
            shard_id: expected_shard,
            source: SendFailure::Closed,
        })
    );
    assert!(manager.desired_output(&output_id).is_none());
    assert_eq!(manager.command_depth(expected_shard), 0);
}

#[test]
fn failed_remove_dispatch_preserves_desired_output() {
    let mut manager = manager(4);
    let output_spec = spec("out-dispatch-remove");
    let output_id = output_spec.id.clone();
    let expected_shard = manager.assign_spec(&output_spec);

    assert!(matches!(
        manager.apply_command(EgressCommand::Add(output_spec.clone())),
        Ok(ManagerCommandOutcome::Enqueued { .. })
    ));
    let result = manager.dispatch_command(
        EgressCommand::Remove(output_id.clone()),
        |shard_id, command| {
            assert_eq!(shard_id, expected_shard);
            assert!(matches!(command, EgressCommand::Remove(_)));
            Err(SendFailure::Closed)
        },
    );

    assert_eq!(
        result,
        Err(EgressManagerDispatchError::Dispatch {
            shard_id: expected_shard,
            source: SendFailure::Closed,
        })
    );
    assert_eq!(
        manager.desired_output(&output_id).map(|desired| (
            desired.id().clone(),
            desired.generation(),
            desired.shard_id()
        )),
        Some((output_id, 1, expected_shard))
    );
    assert_eq!(manager.command_depth(expected_shard), 1);
}

#[test]
fn failed_shutdown_dispatch_preserves_shutdown_state() {
    let mut manager = manager(3);

    let result = manager.dispatch_command(EgressCommand::Shutdown, |shard_id, command| {
        assert!(matches!(command, EgressCommand::Shutdown));
        if shard_id == ShardId::new(1) {
            Err(SendFailure::Closed)
        } else {
            Ok(())
        }
    });

    assert_eq!(
        result,
        Err(EgressManagerDispatchError::Dispatch {
            shard_id: ShardId::new(1),
            source: SendFailure::Closed,
        })
    );
    assert!(!manager.shutting_down);
    assert_eq!(manager.command_depth(ShardId::new(0)), 0);
    assert_eq!(manager.command_depth(ShardId::new(1)), 0);
    assert_eq!(manager.command_depth(ShardId::new(2)), 0);
}

// ---------------------------------------------------------------------------
// Dynamic shard scaling: rendezvous placement for NEW outputs only
// ---------------------------------------------------------------------------

#[test]
fn resizing_shard_count_moves_only_a_minority_of_outputs() {
    // The whole point of rendezvous hashing over `hash % shard_count`:
    // growing the shard count by one must not remap (almost) everything.
    let ids: Vec<OutputId> = (0..2000)
        .map(|i| OutputId::new(format!("out-{i}")))
        .collect();
    let before = NonZeroU32::new(4).unwrap();
    let after = NonZeroU32::new(5).unwrap();

    let moved = ids
        .iter()
        .filter(|id| assign_output_to_shard(id, before) != assign_output_to_shard(id, after))
        .count();

    // Expected fraction is ~1/5 (the new shard's share); allow generous
    // slack for hash noise at this sample size, but this must never
    // approach "nearly everything moved" (`hash % N`'s failure mode).
    let moved_fraction = moved as f64 / ids.len() as f64;
    assert!(
        moved_fraction < 0.35,
        "expected roughly 1/5 of outputs to move on a 4->5 resize, got {moved_fraction} ({moved}/{})",
        ids.len()
    );
}

fn roomy(shards: u32) -> EgressManager {
    EgressManager::new(EgressManagerConfig::new(shards, 8192).unwrap())
}

fn add(manager: &mut EgressManager, id: &str) -> ShardId {
    manager
        .dispatch_command(
            EgressCommand::Add(spec(id)),
            |_, _| Ok::<_, SendFailure>(()),
        )
        .unwrap();
    manager
        .desired_output(&OutputId::new(id))
        .unwrap()
        .shard_id()
}

#[test]
fn growth_keeps_every_live_output_and_places_only_new_ones_on_new_shards() {
    let mut manager = roomy(2);
    let before: Vec<(String, ShardId)> = (0..200)
        .map(|i| (format!("old-{i}"), add(&mut manager, &format!("old-{i}"))))
        .collect();
    manager.grow_to(NonZeroU32::new(4).unwrap());
    for (id, shard) in &before {
        assert_eq!(
            manager
                .desired_output(&OutputId::new(id))
                .unwrap()
                .shard_id(),
            *shard
        );
    }
    let fresh: Vec<ShardId> = (0..400)
        .map(|i| add(&mut manager, &format!("new-{i}")))
        .collect();
    for shard in 0..4 {
        assert!(
            fresh.iter().filter(|s| s.index() == shard).count() > 40,
            "new outputs use every shard, including shard {shard}"
        );
    }
}

#[test]
fn updating_a_live_output_stays_on_its_recorded_shard_after_resizing() {
    let mut manager = roomy(2);
    let original = add(&mut manager, "stable");
    manager.grow_to(NonZeroU32::new(8).unwrap());
    let mut update = spec("stable");
    update.generation = 2;
    let mut sent = Vec::new();
    manager
        .dispatch_command(EgressCommand::Update(update), |shard, command| {
            sent.push((shard, command));
            Ok::<_, SendFailure>(())
        })
        .unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, original);
    assert_eq!(
        manager
            .desired_output(&OutputId::new("stable"))
            .unwrap()
            .shard_id(),
        original
    );
}

#[test]
fn retiring_a_shard_stops_new_placement_and_waits_for_its_last_output() {
    let mut manager = roomy(3);
    let live: Vec<(String, ShardId)> = (0..60)
        .map(|i| (format!("live-{i}"), add(&mut manager, &format!("live-{i}"))))
        .collect();
    let tail: Vec<&String> = live
        .iter()
        .filter(|(_, shard)| shard.index() == 2)
        .map(|(id, _)| id)
        .collect();
    assert!(tail.len() > 1, "hash puts several outputs on shard 2");
    manager.set_placement(NonZeroU32::new(2).unwrap());
    assert!(
        !manager.retire_empty_tail(),
        "live outputs keep the tail alive"
    );
    for i in 0..200 {
        assert!(add(&mut manager, &format!("after-{i}")).index() < 2);
    }
    let remove = |manager: &mut EgressManager, id: &String| {
        manager
            .dispatch_command(EgressCommand::Remove(OutputId::new(id)), |_, _| {
                Ok::<_, SendFailure>(())
            })
            .unwrap();
    };
    for id in &tail[..tail.len() - 1] {
        remove(&mut manager, id);
    }
    assert!(
        !manager.retire_empty_tail(),
        "one live output remains on the tail"
    );
    remove(&mut manager, tail[tail.len() - 1]);
    assert!(manager.retire_empty_tail());
    assert_eq!(manager.config().shard_count().get(), 2);
    assert!(!manager.retire_empty_tail(), "placement shards stay");
}

#[test]
fn output_count_reflects_live_desired_outputs() {
    let mut manager = manager(2);
    assert_eq!(manager.output_count(), 0);

    manager
        .dispatch_command(EgressCommand::Add(spec("out-1")), |_, _| {
            Ok::<_, SendFailure>(())
        })
        .unwrap();
    manager
        .dispatch_command(EgressCommand::Add(spec("out-2")), |_, _| {
            Ok::<_, SendFailure>(())
        })
        .unwrap();
    assert_eq!(manager.output_count(), 2);

    manager
        .dispatch_command(EgressCommand::Remove(OutputId::new("out-1")), |_, _| {
            Ok::<_, SendFailure>(())
        })
        .unwrap();
    assert_eq!(manager.output_count(), 1);
}

mod proptests {
    use super::*;
    use proptest::prelude::*;

    fn output_id_strategy() -> impl Strategy<Value = OutputId> {
        "[a-z]{1,12}".prop_map(OutputId::new)
    }

    proptest! {
        /// Rendezvous assignment never produces an out-of-range shard, and
        /// is a pure function of its inputs (repeated calls agree).
        #[test]
        fn assignment_is_in_range_and_deterministic(
            id in output_id_strategy(),
            shard_count in 1u32..16,
        ) {
            let shard_count = NonZeroU32::new(shard_count).unwrap();
            let first = assign_output_to_shard(&id, shard_count);
            let second = assign_output_to_shard(&id, shard_count);
            prop_assert_eq!(first, second);
            prop_assert!(first.index() < shard_count.get());
        }

        /// Growing the shard count by one moves at most a
        /// `1/shard_count`-bounded fraction of a random output-id sample —
        /// the property that makes resizing cheap. Sampled with generous
        /// slack (3x the theoretical share) to stay robust to hash noise
        /// at moderate sample sizes while still catching a regression to
        /// `hash % N` (which would move nearly all of them).
        #[test]
        fn growing_by_one_moves_a_bounded_fraction(
            shard_count in 2u32..8,
            ids in prop::collection::vec(output_id_strategy(), 100..300),
        ) {
            let before = NonZeroU32::new(shard_count).unwrap();
            let after = NonZeroU32::new(shard_count + 1).unwrap();
            let moved = ids
                .iter()
                .filter(|id| assign_output_to_shard(id, before) != assign_output_to_shard(id, after))
                .count();
            let moved_fraction = moved as f64 / ids.len() as f64;
            let theoretical_share = 1.0 / f64::from(after.get());
            prop_assert!(
                moved_fraction <= (theoretical_share * 3.0).min(1.0),
                "moved_fraction={moved_fraction} theoretical_share={theoretical_share}"
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Arbitrary Add/Remove/resize sequences never move a live output,
        /// never leave one on a shard that no longer exists, and never place a
        /// new output outside the placement shards.
        #[test]
        fn resizing_never_moves_or_strands_outputs(
            ops in prop::collection::vec(
                prop_oneof![
                    (0usize..8).prop_map(ProptestOp::Add),
                    (0usize..8).prop_map(ProptestOp::Remove),
                    (1u32..6).prop_map(ProptestOp::Resize),
                ],
                0..120,
            ),
        ) {
            let mut manager = roomy(3);
            let mut recorded = std::collections::HashMap::new();
            for op in ops {
                match op {
                    ProptestOp::Add(index) => {
                        let id = format!("out-{index}");
                        let placement = manager.placement_count().get();
                        if manager
                            .dispatch_command(EgressCommand::Add(spec(&id)), |_, _| Ok::<_, SendFailure>(()))
                            .is_ok()
                        {
                            let shard = manager.desired_output(&OutputId::new(&id)).unwrap().shard_id();
                            recorded.entry(id).or_insert_with(|| {
                                assert!(shard.index() < placement);
                                shard
                            });
                        }
                    }
                    ProptestOp::Remove(index) => {
                        let id = format!("out-{index}");
                        let _ = manager.dispatch_command(
                            EgressCommand::Remove(OutputId::new(&id)),
                            |_, _| Ok::<_, SendFailure>(()),
                        );
                        recorded.remove(&id);
                    }
                    ProptestOp::Resize(count) => {
                        let count = NonZeroU32::new(count).unwrap();
                        if count > manager.config().shard_count() {
                            manager.grow_to(count);
                        } else {
                            manager.set_placement(count);
                        }
                        while manager.retire_empty_tail() {}
                    }
                }
                let shard_count = manager.config().shard_count().get();
                for index in 0..8 {
                    let id = format!("out-{index}");
                    if let Some(desired) = manager.desired_output(&OutputId::new(&id)) {
                        prop_assert_eq!(Some(&desired.shard_id), recorded.get(&id));
                        prop_assert!(
                            desired.shard_id.index() < shard_count,
                            "an output was stranded on a retired shard"
                        );
                    }
                }
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum ProptestOp {
        Add(usize),
        Remove(usize),
        Resize(u32),
    }
}
