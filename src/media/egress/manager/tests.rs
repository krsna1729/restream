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
        manager.desired_output(&output_spec.id),
        Some(&DesiredOutput {
            id: output_spec.id,
            generation: 1,
            shard_id: expected_shard,
        })
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
        manager.desired_output(&first.id),
        Some(&DesiredOutput {
            id: first.id,
            generation: 2,
            shard_id,
        })
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
        manager.desired_output(&output_id),
        Some(&DesiredOutput {
            id: output_id,
            generation: 1,
            shard_id: ShardId::new(0),
        })
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
        manager.desired_output(&output_id),
        Some(&DesiredOutput {
            id: output_id,
            generation: 1,
            shard_id: expected_shard,
        })
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
