//! Protocol-neutral egress fabric.
//!
//! This module contains the target architecture described in
//! `docs/egress-architecture.md`. It is introduced behind the
//! `RESTREAM_EGRESS_FABRIC` rollout selector and coexists with the legacy
//! per-output task model during migration.
//!
//! # Module layout
//!
//! ```text
//! egress/
//!   mod.rs          — this file, re-exports and feature gate
//!   command.rs      — EgressCommand, OutputSpec, IDs
//!   feed.rs         — EgressFeed trait, FeedCursor, FeedRead
//!   lifecycle.rs    — LeafLifecycle state machine
//!   scheduler.rs    — ReadyQueue, ScheduleState, round-robin
//!   policy.rs       — LeafPolicy, RetryState, WorkBudget, LeafLimits
//!   backend.rs      — ProtocolEngine trait, EngineProgress, Readiness
//!   backends/       — concrete fabric protocol engines
//!   leaf.rs         — Leaf<P>, LeafCommon, LeafDeadlines, ProgressState
//!   timer.rs        — TimerWheel<K>
//!   metrics.rs      — ShardMetrics, LeafMetrics, FeedMetrics
//!   manager.rs      — desired output assignment and command admission
//!   shard.rs        — fixed shard threads, supervision snapshots, wake budgets
//!   supervisor.rs   — panic recovery orchestration across manager and shards
//!   test_driver.rs  — FakeFeed, FakeEngine, FakePoller (cfg(test) / test-only)
//! ```

pub mod backend;
pub mod backends;
pub mod command;
pub mod feed;
pub mod journal;
pub mod leaf;
pub mod lifecycle;
pub mod manager;
pub mod metrics;
pub mod policy;
pub mod scheduler;
pub mod shard;
pub mod supervisor;
pub mod timer;
pub mod visit;

#[cfg(any(test, feature = "egress-test-driver"))]
pub mod test_driver;
#[cfg(test)]
mod visit_tests;

// Re-export the stable public surface for this phase.
pub use backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, Readiness, RecoveryCapability,
};
pub use backends::sink::{SinkDiscardStats, SinkEngine, SinkTransport};
pub use command::{EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec, ShardId};
pub use feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
pub use lifecycle::LeafLifecycle;
pub use manager::{
    DesiredOutput, EgressManager, EgressManagerCommandError, EgressManagerConfig,
    EgressManagerConfigError, EgressManagerDispatchError, ManagerCommandOutcome,
};
pub use policy::{LeafLimits, LeafPolicy, RetryState, WorkBudget};
pub use shard::{
    EgressShardBackend, EgressShardCommandEffect, EgressShardConfig, EgressShardConfigError,
    EgressShardGroup, EgressShardGroupError, EgressShardHandle, EgressShardHealth,
    EgressShardHeartbeat, EgressShardSendError, EgressShardSnapshot,
};
pub use supervisor::{
    EgressShardRecovery, EgressSupervisor, EgressSupervisorConfig, EgressSupervisorError,
    EgressSupervisorRecovery,
};
pub use visit::{EngineVisit, EngineVisitOutcome, EngineVisitResult};
