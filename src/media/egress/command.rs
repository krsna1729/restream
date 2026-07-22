//! Egress command types and stable output identity.
//!
//! Commands are idempotent by output identity and `generation`. A stale update
//! (lower `generation`) must not resurrect or overwrite a newer output.

use std::fmt;

// ---------------------------------------------------------------------------
// IDs
// ---------------------------------------------------------------------------

/// Stable identity for a live output destination.
///
/// Carried as a thin `String` newtype so pipeline IDs, output IDs, etc.
/// cannot be accidentally mixed (mirrors `crate::domain::ids` conventions).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutputId(String);

impl OutputId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OutputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Identity of a prepared media feed (shared ring / TsChunkRing view).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedId(String);

impl FeedId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FeedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Identity of an egress shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShardId(u32);

impl ShardId {
    pub fn new(index: u32) -> Self {
        Self(index)
    }

    pub fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ShardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "shard-{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Protocol specification
// ---------------------------------------------------------------------------

/// Protocol-specific connection parameters carried with an `OutputSpec`.
///
/// Extended as more protocols migrate onto the fabric.
#[derive(Debug, Clone)]
pub enum ProtocolSpec {
    /// Plain or TLS RTMP egress.
    Rtmp { url: String, tls: bool },
    /// SRT egress.
    Srt { url: String },
    /// Discard prepared media while exercising the common fabric path.
    Sink,
}

// ---------------------------------------------------------------------------
// Output specification
// ---------------------------------------------------------------------------

/// Everything required to create or update a leaf on an egress shard.
///
/// `generation` is monotonically increasing per `id`. Stale events with an
/// older generation are rejected by the shard without state mutation.
#[derive(Debug, Clone)]
pub struct OutputSpec {
    pub id: OutputId,
    /// Monotonically increasing per `id`. Incremented on every update.
    pub generation: u64,
    /// Which prepared feed this output should consume.
    pub feed: FeedId,
    /// Protocol-specific connection parameters.
    pub protocol: ProtocolSpec,
    /// Operational policy: timeouts, limits, retry bounds.
    pub policy: crate::media::egress::policy::LeafPolicy,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Commands issued by the application control plane to an egress shard group.
///
/// All commands are idempotent by output identity and generation. The control
/// plane must not block indefinitely on the command channel; an overload
/// condition is surfaced as an operator-visible error and reconciliation
/// retries desired state.
#[derive(Debug, Clone)]
pub enum EgressCommand {
    /// Start or reuse a leaf for this output specification.
    Add(OutputSpec),
    /// Update a running leaf to a new specification generation.
    Update(OutputSpec),
    /// Close and remove the leaf with this output ID.
    Remove(OutputId),
    /// Stop accepting new assignments and begin draining this shard.
    DrainShard(ShardId),
    /// Shut down the entire fabric manager.
    Shutdown,
}

impl EgressCommand {
    /// Returns the output ID affected by this command, if any.
    pub fn output_id(&self) -> Option<&OutputId> {
        match self {
            EgressCommand::Add(s) | EgressCommand::Update(s) => Some(&s.id),
            EgressCommand::Remove(id) => Some(id),
            EgressCommand::DrainShard(_) | EgressCommand::Shutdown => None,
        }
    }

    /// Returns the generation carried by this command, or `None` for
    /// commands that do not target a specific output.
    pub fn generation(&self) -> Option<u64> {
        match self {
            EgressCommand::Add(s) | EgressCommand::Update(s) => Some(s.generation),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::egress::policy::LeafPolicy;

    fn dummy_spec(id: &str, generation_val: u64) -> OutputSpec {
        OutputSpec {
            id: OutputId::new(id),
            generation: generation_val,
            feed: FeedId::new("feed-1"),
            protocol: ProtocolSpec::Rtmp {
                url: "rtmp://localhost/live".into(),
                tls: false,
            },
            policy: LeafPolicy::default(),
        }
    }

    #[test]
    fn output_id_eq() {
        assert_eq!(OutputId::new("a"), OutputId::new("a"));
        assert_ne!(OutputId::new("a"), OutputId::new("b"));
    }

    #[test]
    fn shard_id_display() {
        assert_eq!(ShardId::new(3).to_string(), "shard-3");
    }

    #[test]
    fn command_output_id_add() {
        let cmd = EgressCommand::Add(dummy_spec("out-1", 1));
        assert_eq!(cmd.output_id().map(|id| id.as_str()), Some("out-1"));
        assert_eq!(cmd.generation(), Some(1));
    }

    #[test]
    fn command_output_id_remove() {
        let cmd = EgressCommand::Remove(OutputId::new("out-2"));
        assert_eq!(cmd.output_id().map(|id| id.as_str()), Some("out-2"));
        assert_eq!(cmd.generation(), None);
    }

    #[test]
    fn command_drain_has_no_output() {
        let cmd = EgressCommand::DrainShard(ShardId::new(0));
        assert!(cmd.output_id().is_none());
        assert!(cmd.generation().is_none());
    }
}
