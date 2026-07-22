use std::collections::HashMap;
use std::num::{NonZeroU32, NonZeroUsize};

use crate::media::egress::command::{EgressCommand, OutputId, OutputSpec, ShardId};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressManagerConfigError {
    ZeroShardCount,
    ZeroCommandCapacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressManagerConfig {
    shard_count: NonZeroU32,
    command_channel_capacity: NonZeroUsize,
}

impl EgressManagerConfig {
    pub fn new(
        shard_count: u32,
        command_channel_capacity: usize,
    ) -> Result<Self, EgressManagerConfigError> {
        let shard_count =
            NonZeroU32::new(shard_count).ok_or(EgressManagerConfigError::ZeroShardCount)?;
        let command_channel_capacity = NonZeroUsize::new(command_channel_capacity)
            .ok_or(EgressManagerConfigError::ZeroCommandCapacity)?;
        Ok(Self {
            shard_count,
            command_channel_capacity,
        })
    }

    pub fn shard_count(self) -> NonZeroU32 {
        self.shard_count
    }

    pub fn command_channel_capacity(self) -> NonZeroUsize {
        self.command_channel_capacity
    }
}

#[derive(Debug, Clone)]
pub struct EgressManager {
    config: EgressManagerConfig,
    desired: HashMap<OutputId, DesiredOutput>,
    command_depths: Vec<usize>,
    shutting_down: bool,
}

impl EgressManager {
    pub fn new(config: EgressManagerConfig) -> Self {
        Self {
            command_depths: vec![0; config.shard_count.get() as usize],
            config,
            desired: HashMap::new(),
            shutting_down: false,
        }
    }

    pub fn config(&self) -> EgressManagerConfig {
        self.config
    }

    pub fn assign_output(&self, output_id: &OutputId) -> ShardId {
        assign_output_to_shard(output_id, self.config.shard_count)
    }

    pub fn assign_spec(&self, spec: &OutputSpec) -> ShardId {
        self.assign_output(&spec.id)
    }

    pub fn desired_output(&self, output_id: &OutputId) -> Option<&DesiredOutput> {
        self.desired.get(output_id)
    }

    pub fn command_depth(&self, shard_id: ShardId) -> usize {
        self.command_depths
            .get(shard_id.index() as usize)
            .copied()
            .unwrap_or(0)
    }

    pub fn complete_one_command(&mut self, shard_id: ShardId) {
        if let Some(depth) = self.command_depths.get_mut(shard_id.index() as usize) {
            *depth = depth.saturating_sub(1);
        }
    }

    pub fn apply_command(
        &mut self,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, EgressManagerCommandError> {
        self.dispatch_command(command, |_, _| Ok(())).map_err(
            |error: EgressManagerDispatchError<()>| match error {
                EgressManagerDispatchError::Command(command_error) => command_error,
                EgressManagerDispatchError::Dispatch { .. } => {
                    unreachable!("infallible dispatch failed")
                }
            },
        )
    }

    pub fn dispatch_command<E, F>(
        &mut self,
        command: EgressCommand,
        mut dispatch: F,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<E>>
    where
        F: FnMut(ShardId, EgressCommand) -> Result<(), E>,
    {
        match command {
            EgressCommand::Add(spec) => {
                self.dispatch_spec(EgressCommand::Add(spec.clone()), spec, dispatch)
            }
            EgressCommand::Update(spec) => {
                self.dispatch_spec(EgressCommand::Update(spec.clone()), spec, dispatch)
            }
            EgressCommand::Remove(output_id) => self.dispatch_remove(output_id, dispatch),
            EgressCommand::DrainShard(shard_id) => {
                self.check_command_slot(shard_id)
                    .map_err(EgressManagerDispatchError::Command)?;
                dispatch(shard_id, EgressCommand::DrainShard(shard_id))
                    .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
                self.reserve_command_slot(shard_id)
                    .map_err(EgressManagerDispatchError::Command)?;
                Ok(ManagerCommandOutcome::Enqueued { shard_id })
            }
            EgressCommand::Shutdown => self.dispatch_shutdown(dispatch),
        }
    }

    fn dispatch_spec<E, F>(
        &mut self,
        command: EgressCommand,
        spec: OutputSpec,
        mut dispatch: F,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<E>>
    where
        F: FnMut(ShardId, EgressCommand) -> Result<(), E>,
    {
        let shard_id = self.assign_spec(&spec);
        if let Some(current) = self.desired.get(&spec.id) {
            if spec.generation < current.generation {
                return Ok(ManagerCommandOutcome::IgnoredStale {
                    shard_id: current.shard_id,
                });
            }
            if spec.generation == current.generation {
                return Ok(ManagerCommandOutcome::AlreadyCurrent {
                    shard_id: current.shard_id,
                });
            }
        }

        self.check_command_slot(shard_id)
            .map_err(EgressManagerDispatchError::Command)?;
        dispatch(shard_id, command)
            .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
        self.reserve_command_slot(shard_id)
            .map_err(EgressManagerDispatchError::Command)?;
        let desired = DesiredOutput {
            id: spec.id.clone(),
            generation: spec.generation,
            shard_id,
        };
        self.desired.insert(spec.id, desired.clone());
        Ok(ManagerCommandOutcome::Enqueued { shard_id })
    }

    fn dispatch_remove<E, F>(
        &mut self,
        output_id: OutputId,
        mut dispatch: F,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<E>>
    where
        F: FnMut(ShardId, EgressCommand) -> Result<(), E>,
    {
        let Some(current) = self.desired.get(&output_id) else {
            return Ok(ManagerCommandOutcome::AlreadyRemoved);
        };
        let shard_id = current.shard_id;
        self.check_command_slot(shard_id)
            .map_err(EgressManagerDispatchError::Command)?;
        dispatch(shard_id, EgressCommand::Remove(output_id.clone()))
            .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
        self.reserve_command_slot(shard_id)
            .map_err(EgressManagerDispatchError::Command)?;
        self.desired.remove(&output_id);
        Ok(ManagerCommandOutcome::Enqueued { shard_id })
    }

    fn dispatch_shutdown<E, F>(
        &mut self,
        mut dispatch: F,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<E>>
    where
        F: FnMut(ShardId, EgressCommand) -> Result<(), E>,
    {
        if self.shutting_down {
            return Ok(ManagerCommandOutcome::AlreadyShuttingDown);
        }
        for shard_index in 0..self.config.shard_count.get() {
            self.check_command_slot(ShardId::new(shard_index))
                .map_err(EgressManagerDispatchError::Command)?;
        }
        for shard_index in 0..self.config.shard_count.get() {
            let shard_id = ShardId::new(shard_index);
            dispatch(shard_id, EgressCommand::Shutdown)
                .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
        }
        for shard_index in 0..self.config.shard_count.get() {
            self.reserve_command_slot(ShardId::new(shard_index))
                .map_err(EgressManagerDispatchError::Command)?;
        }
        self.shutting_down = true;
        Ok(ManagerCommandOutcome::Broadcast {
            shard_count: self.config.shard_count,
        })
    }

    fn reserve_command_slot(&mut self, shard_id: ShardId) -> Result<(), EgressManagerCommandError> {
        self.check_command_slot(shard_id)?;
        let depth = &mut self.command_depths[shard_id.index() as usize];
        *depth += 1;
        Ok(())
    }

    fn check_command_slot(&self, shard_id: ShardId) -> Result<(), EgressManagerCommandError> {
        let Some(depth) = self.command_depths.get(shard_id.index() as usize) else {
            return Err(EgressManagerCommandError::UnknownShard { shard_id });
        };
        if *depth >= self.config.command_channel_capacity.get() {
            return Err(EgressManagerCommandError::CommandChannelFull { shard_id });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredOutput {
    pub id: OutputId,
    pub generation: u64,
    pub shard_id: ShardId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerCommandOutcome {
    Enqueued { shard_id: ShardId },
    Broadcast { shard_count: NonZeroU32 },
    IgnoredStale { shard_id: ShardId },
    AlreadyCurrent { shard_id: ShardId },
    AlreadyRemoved,
    AlreadyShuttingDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressManagerCommandError {
    UnknownShard { shard_id: ShardId },
    CommandChannelFull { shard_id: ShardId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressManagerDispatchError<E> {
    Command(EgressManagerCommandError),
    Dispatch { shard_id: ShardId, source: E },
}

pub fn assign_output_to_shard(output_id: &OutputId, shard_count: NonZeroU32) -> ShardId {
    let hash = stable_output_hash(output_id.as_str().as_bytes());
    ShardId::new((hash % u64::from(shard_count.get())) as u32)
}

fn stable_output_hash(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
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

        let result =
            manager.dispatch_command(EgressCommand::Add(output_spec), |shard_id, command| {
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
}
