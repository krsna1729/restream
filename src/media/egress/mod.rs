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
//!   leaf.rs         — Leaf<P>, LeafCommon, LeafDeadlines, ProgressState
//!   timer.rs        — TimerWheel<K>
//!   metrics.rs      — ShardMetrics, LeafMetrics, FeedMetrics
//!   test_driver.rs  — FakeFeed, FakeEngine, FakePoller (cfg(test) / test-only)
//! ```

pub mod backend;
pub mod command;
pub mod feed;
pub mod journal;
pub mod leaf;
pub mod lifecycle;
pub mod manager;
pub mod metrics;
pub mod policy;
pub mod scheduler;
pub mod timer;

#[cfg(any(test, feature = "egress-test-driver"))]
pub mod test_driver;

// Re-export the stable public surface for this phase.
// EgressManager and shard types are added in Phase 3.
pub use backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, Readiness, RecoveryCapability,
};
pub use command::{EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec, ShardId};
pub use feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
pub use lifecycle::LeafLifecycle;
pub use manager::{EgressManager, EgressManagerConfig, EgressManagerConfigError};
pub use policy::{LeafLimits, LeafPolicy, RetryState, WorkBudget};
