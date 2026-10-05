use std::collections::HashMap;
use std::num::{NonZeroU32, NonZeroUsize};

use crate::media::egress::command::{EgressCommand, OutputId, OutputSpec, ShardId};
use crate::media::egress::shard::{EgressShardGroup, EgressShardGroupError};

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
    /// Every live output: its spec and the shard it is placed on, in one
    /// entry, so the spec and the placement cannot disagree.
    desired: HashMap<OutputId, DesiredOutput>,
    draining_shards: Vec<bool>,
    /// Shards that accept NEW outputs (`<= config.shard_count`). Shards above
    /// it only drain: a live output is never moved, so resizing cannot
    /// reconnect a destination.
    placement: NonZeroU32,
    shutting_down: bool,
}

impl EgressManager {
    pub fn new(config: EgressManagerConfig) -> Self {
        Self {
            config,
            desired: HashMap::new(),
            draining_shards: vec![false; config.shard_count.get() as usize],
            placement: config.shard_count,
            shutting_down: false,
        }
    }

    pub fn config(&self) -> EgressManagerConfig {
        self.config
    }

    pub fn assign_output(&self, output_id: &OutputId) -> ShardId {
        assign_output_to_shard(output_id, self.placement)
    }

    pub fn assign_spec(&self, spec: &OutputSpec) -> ShardId {
        self.assign_output(&spec.id)
    }

    pub fn desired_output(&self, output_id: &OutputId) -> Option<&DesiredOutput> {
        self.desired.get(output_id)
    }

    pub fn dispatch_command<S: CommandSink>(
        &mut self,
        command: EgressCommand,
        sink: &S,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<S::Error>> {
        match command {
            EgressCommand::Add(spec) => self.dispatch_spec(spec, false, sink),
            EgressCommand::Update(spec) => self.dispatch_spec(spec, true, sink),
            EgressCommand::Remove(output_id) => self.dispatch_remove(output_id, sink),
            // Feed wakes are delivered per shard by the feed watcher, not
            // routed through manager assignment.
            EgressCommand::FeedWake => Ok(ManagerCommandOutcome::Ignored),
            EgressCommand::DrainShard(shard_id) => {
                self.check_command_slots(shard_id, 1, sink)
                    .map_err(EgressManagerDispatchError::Command)?;
                sink.send(shard_id, EgressCommand::DrainShard(shard_id))
                    .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
                self.mark_draining(shard_id)
                    .map_err(EgressManagerDispatchError::Command)?;
                Ok(ManagerCommandOutcome::Enqueued { shard_id })
            }
            EgressCommand::Shutdown => self.dispatch_shutdown(sink),
        }
    }

    pub fn dispatch_recreate_shard<S: CommandSink>(
        &mut self,
        shard_id: ShardId,
        sink: &S,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<S::Error>> {
        if self.shutting_down {
            return Ok(ManagerCommandOutcome::AlreadyShuttingDown);
        }
        let mut specs = self
            .specs_for_shard(shard_id)
            .map_err(EgressManagerDispatchError::Command)?;
        specs.sort_by(|left, right| left.id.cmp(&right.id));
        self.check_command_slots(shard_id, specs.len(), sink)
            .map_err(EgressManagerDispatchError::Command)?;
        for spec in specs {
            sink.send(shard_id, EgressCommand::Add(spec))
                .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
        }
        Ok(ManagerCommandOutcome::Replayed {
            shard_id,
            output_count: self.desired_count_for_shard(shard_id),
        })
    }

    fn dispatch_spec<S: CommandSink>(
        &mut self,
        spec: OutputSpec,
        is_update: bool,
        sink: &S,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<S::Error>> {
        // A live output stays where its leaf and socket live.
        let shard_id = self
            .desired
            .get(&spec.id)
            .map_or_else(|| self.assign_spec(&spec), DesiredOutput::shard_id);
        if let Some(current) = self.desired.get(&spec.id) {
            if spec.generation < current.generation() {
                return Ok(ManagerCommandOutcome::IgnoredStale {
                    shard_id: current.shard_id,
                });
            }
            if spec.generation == current.generation() {
                return Ok(ManagerCommandOutcome::AlreadyCurrent {
                    shard_id: current.shard_id,
                });
            }
        }

        if self
            .is_draining(shard_id)
            .map_err(EgressManagerDispatchError::Command)?
        {
            return Err(EgressManagerDispatchError::Command(
                EgressManagerCommandError::ShardDraining { shard_id },
            ));
        }
        // Check slot availability BEFORE cloning the OutputSpec for the
        // dispatch. When the channel is full, this avoids the spec clone
        // (heap-allocated Strings, Arc bump, LeafPolicy clone) entirely.
        self.check_command_slots(shard_id, 1, sink)
            .map_err(EgressManagerDispatchError::Command)?;
        let command = if is_update {
            EgressCommand::Update(spec.clone())
        } else {
            EgressCommand::Add(spec.clone())
        };
        sink.send(shard_id, command)
            .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
        self.desired
            .insert(spec.id.clone(), DesiredOutput { spec, shard_id });
        Ok(ManagerCommandOutcome::Enqueued { shard_id })
    }

    fn dispatch_remove<S: CommandSink>(
        &mut self,
        output_id: OutputId,
        sink: &S,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<S::Error>> {
        let Some(current) = self.desired.get(&output_id) else {
            return Ok(ManagerCommandOutcome::AlreadyRemoved);
        };
        let shard_id = current.shard_id;
        self.check_command_slots(shard_id, 1, sink)
            .map_err(EgressManagerDispatchError::Command)?;
        sink.send(shard_id, EgressCommand::Remove(output_id.clone()))
            .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
        self.desired.remove(&output_id);
        Ok(ManagerCommandOutcome::Enqueued { shard_id })
    }

    fn dispatch_shutdown<S: CommandSink>(
        &mut self,
        sink: &S,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<S::Error>> {
        if self.shutting_down {
            return Ok(ManagerCommandOutcome::AlreadyShuttingDown);
        }
        for shard_index in 0..self.config.shard_count.get() {
            self.check_command_slots(ShardId::new(shard_index), 1, sink)
                .map_err(EgressManagerDispatchError::Command)?;
        }
        for shard_index in 0..self.config.shard_count.get() {
            let shard_id = ShardId::new(shard_index);
            sink.send(shard_id, EgressCommand::Shutdown)
                .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
        }
        self.shutting_down = true;
        Ok(ManagerCommandOutcome::Broadcast {
            shard_count: self.config.shard_count,
        })
    }

    /// Refuse before sending (and before cloning a spec) when `shard_id`'s
    /// channel cannot take `additional` more commands, so a multi-command
    /// send is all-or-nothing. The channel is the only record of its depth:
    /// a concurrent sender (feed wakes) can still fill it between this check
    /// and the send, which then fails as a `Dispatch` error.
    fn check_command_slots<S: CommandSink>(
        &self,
        shard_id: ShardId,
        additional: usize,
        sink: &S,
    ) -> Result<(), EgressManagerCommandError> {
        if shard_id.index() >= self.config.shard_count.get() {
            return Err(EgressManagerCommandError::UnknownShard { shard_id });
        }
        let Some(free) = sink.free_slots(shard_id) else {
            return Err(EgressManagerCommandError::UnknownShard { shard_id });
        };
        if additional > free {
            return Err(EgressManagerCommandError::CommandChannelFull { shard_id });
        }
        Ok(())
    }

    fn is_draining(&self, shard_id: ShardId) -> Result<bool, EgressManagerCommandError> {
        self.draining_shards
            .get(shard_id.index() as usize)
            .copied()
            .ok_or(EgressManagerCommandError::UnknownShard { shard_id })
    }

    fn mark_draining(&mut self, shard_id: ShardId) -> Result<(), EgressManagerCommandError> {
        let Some(draining) = self.draining_shards.get_mut(shard_id.index() as usize) else {
            return Err(EgressManagerCommandError::UnknownShard { shard_id });
        };
        *draining = true;
        Ok(())
    }

    fn specs_for_shard(
        &self,
        shard_id: ShardId,
    ) -> Result<Vec<OutputSpec>, EgressManagerCommandError> {
        if shard_id.index() >= self.config.shard_count.get() {
            return Err(EgressManagerCommandError::UnknownShard { shard_id });
        }
        Ok(self
            .desired
            .values()
            .filter(|desired| desired.shard_id == shard_id)
            .map(|desired| desired.spec.clone())
            .collect())
    }

    fn desired_count_for_shard(&self, shard_id: ShardId) -> usize {
        self.desired
            .values()
            .filter(|desired| desired.shard_id == shard_id)
            .count()
    }

    /// Live output count this manager currently owns, the demand input for
    /// shard sizing.
    pub fn output_count(&self) -> usize {
        self.desired.len()
    }

    /// Shards accepting new outputs.
    pub fn placement_count(&self) -> NonZeroU32 {
        self.placement
    }

    /// Make `shards` shards, already spawned by the caller, available for new
    /// outputs. Existing outputs stay where they are.
    pub fn grow_to(&mut self, shards: NonZeroU32) {
        let shards = shards.max(self.config.shard_count);
        self.config.shard_count = shards;
        self.draining_shards.resize(shards.get() as usize, false);
        self.placement = shards;
    }

    /// Send new outputs only to the first `shards` shards (never more than
    /// exist). Higher shards drain through ordinary output removal.
    pub fn set_placement(&mut self, shards: NonZeroU32) {
        self.placement = shards.min(self.config.shard_count);
    }

    /// Forget the highest shard when it no longer accepts new outputs and
    /// owns none. The caller then shuts its thread down. A shard with a live
    /// output is never retired.
    pub fn retire_empty_tail(&mut self) -> bool {
        let tail = self.config.shard_count.get();
        if tail <= 1
            || self.placement.get() >= tail
            || self.desired_count_for_shard(ShardId::new(tail - 1)) > 0
        {
            return false;
        }
        self.config.shard_count = NonZeroU32::new(tail - 1).expect("tail > 1");
        self.draining_shards.truncate((tail - 1) as usize);
        true
    }
}

/// Where the manager sends commands: the shard command channels in
/// production. The channel is the only authority on how full it is.
pub trait CommandSink {
    type Error;
    /// Commands `shard_id`'s channel can take now; `None` for no such shard.
    fn free_slots(&self, shard_id: ShardId) -> Option<usize>;
    fn send(&self, shard_id: ShardId, command: EgressCommand) -> Result<(), Self::Error>;
}

impl CommandSink for EgressShardGroup {
    type Error = EgressShardGroupError;

    fn free_slots(&self, shard_id: ShardId) -> Option<usize> {
        self.free_command_slots(shard_id)
    }

    fn send(&self, shard_id: ShardId, command: EgressCommand) -> Result<(), Self::Error> {
        self.try_send_to(shard_id, command)
    }
}

/// A live output as the manager last dispatched it.
#[derive(Debug, Clone)]
pub struct DesiredOutput {
    spec: OutputSpec,
    shard_id: ShardId,
}

impl DesiredOutput {
    pub fn id(&self) -> &OutputId {
        &self.spec.id
    }

    pub fn generation(&self) -> u64 {
        self.spec.generation
    }

    pub fn shard_id(&self) -> ShardId {
        self.shard_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerCommandOutcome {
    Enqueued {
        shard_id: ShardId,
    },
    Broadcast {
        shard_count: NonZeroU32,
    },
    IgnoredStale {
        shard_id: ShardId,
    },
    AlreadyCurrent {
        shard_id: ShardId,
    },
    AlreadyRemoved,
    AlreadyShuttingDown,
    /// The command is not routed through manager assignment (feed wakes are
    /// delivered per shard by the feed watcher).
    Ignored,
    Replayed {
        shard_id: ShardId,
        output_count: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressManagerCommandError {
    UnknownShard { shard_id: ShardId },
    CommandChannelFull { shard_id: ShardId },
    ShardDraining { shard_id: ShardId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressManagerDispatchError<E> {
    Command(EgressManagerCommandError),
    Dispatch { shard_id: ShardId, source: E },
}

/// Rendezvous (highest-random-weight) hashing: score every shard by
/// hashing `(output_id, shard_index)` together and pick the max. Changing the
/// count changes the winner for about `1/shard_count` of NEW outputs, while
/// existing outputs keep their recorded shard. `shard_count` is always small
/// (see `default_egress_fabric_shards`, capped at 8), so this stays a cheap
/// `O(shard_count)` scan.
pub fn assign_output_to_shard(output_id: &OutputId, shard_count: NonZeroU32) -> ShardId {
    let bytes = output_id.as_str().as_bytes();
    (0..shard_count.get())
        .max_by_key(|&shard_index| stable_output_hash_pair(bytes, shard_index))
        .map(ShardId::new)
        .unwrap_or_else(|| ShardId::new(0))
}

fn stable_output_hash(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// `stable_output_hash` extended with a shard index folded into the same
/// FNV chain, so each shard gets an independent score for the same output
/// id.
fn stable_output_hash_pair(bytes: &[u8], shard_index: u32) -> u64 {
    let mut hash = stable_output_hash(bytes);
    for byte in shard_index.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests;
