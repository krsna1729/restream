//! `ProtocolEngine` trait and associated result/readiness types.
//!
//! A protocol engine owns only connection-local state. It advances when given
//! readiness, feed access, and a finite work budget. All lifecycle policy,
//! retry scheduling, and backpressure decisions belong to the fabric.

use crate::media::egress::feed::{EgressFeed, FeedCursor};
use crate::media::egress::policy::WorkBudget;

// ---------------------------------------------------------------------------
// Readiness
// ---------------------------------------------------------------------------

/// Which I/O interests are currently satisfied for a transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Readiness {
    pub readable: bool,
    pub writable: bool,
}

impl Readiness {
    pub const READABLE: Self = Self {
        readable: true,
        writable: false,
    };
    pub const WRITABLE: Self = Self {
        readable: false,
        writable: true,
    };
    pub const BOTH: Self = Self {
        readable: true,
        writable: true,
    };

    pub fn satisfies(self, interest: Interest) -> bool {
        (!interest.readable || self.readable) && (!interest.writable || self.writable)
    }
}

// ---------------------------------------------------------------------------
// Interest
// ---------------------------------------------------------------------------

/// I/O interests a protocol engine registers with the shard poller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Interest {
    pub readable: bool,
    pub writable: bool,
}

impl Interest {
    pub const READ: Self = Self {
        readable: true,
        writable: false,
    };
    pub const WRITE: Self = Self {
        readable: false,
        writable: true,
    };
    pub const READ_WRITE: Self = Self {
        readable: true,
        writable: true,
    };
    pub const NONE: Self = Self {
        readable: false,
        writable: false,
    };

    pub fn is_empty(self) -> bool {
        !self.readable && !self.writable
    }
}

// ---------------------------------------------------------------------------
// Close reason
// ---------------------------------------------------------------------------

/// Why the fabric is requesting that a protocol engine close its transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// Control plane removed or updated the output.
    Removed,
    /// Leaf is being resynchronized after a feed overrun.
    Resynchronizing,
    /// No-progress timeout exceeded.
    NoProgress,
    /// Backpressure limit exceeded.
    BackpressureLimit,
    /// Shard is shutting down.
    ShardShutdown,
    /// Peer initiated the close.
    PeerClosed,
}

// ---------------------------------------------------------------------------
// Recovery capability
// ---------------------------------------------------------------------------

/// Whether a protocol engine supports in-place recovery without reconnect.
///
/// The common policy (reconnect at a sync point) remains the safe default.
/// In-place recovery is only used when the engine advertises it and the
/// fabric decides it is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryCapability {
    /// Only reconnect-based recovery is supported.
    ReconnectOnly,
    /// The engine can resynchronize without closing the connection if the
    /// fabric provides a new valid cursor. (Advertise only with evidence.)
    InPlaceResync,
}

// ---------------------------------------------------------------------------
// Protocol failure
// ---------------------------------------------------------------------------

/// Reason an engine call returned a fatal error.
#[derive(Debug, Clone)]
pub struct ProtocolFailure {
    /// Short machine-readable reason code for metrics and alerts.
    pub reason: &'static str,
    /// Human-readable detail for diagnostics.
    pub detail: String,
    /// Whether this failure is likely transient (suitable for retry).
    pub retryable: bool,
}

// ---------------------------------------------------------------------------
// EngineProgress
// ---------------------------------------------------------------------------

/// The outcome of one `ProtocolEngine::advance` call.
///
/// The shard maps this to a lifecycle event and updates the leaf accordingly.
#[derive(Debug)]
pub enum EngineProgress {
    /// Forward progress was made.
    Progress {
        /// Bytes sent or accepted.
        bytes: usize,
        /// Media units consumed from the feed.
        units: usize,
        /// I/O interests the engine needs before the *next* advance.
        interest: Interest,
    },
    /// No progress was possible; engine needs the given readiness before
    /// it can advance again.
    Needs(Interest),
    /// Application-level handshake completed successfully.
    HandshakeComplete,
    /// The leaf's feed cursor fell behind the oldest retained entry.
    FeedOverrun,
    /// The remote peer closed the connection.
    PeerClosed,
    /// A fatal protocol or transport error occurred.
    Failed(ProtocolFailure),
    /// The engine has consumed its budget for this visit and asks to be
    /// rescheduled without blocking.
    Yield,
}

// ---------------------------------------------------------------------------
// ProtocolEngine
// ---------------------------------------------------------------------------

/// Contract for a connection-local protocol implementation.
///
/// An engine owns handshake bytes, wire serialization, protocol state, and
/// readiness mechanics. It **must not**:
///
/// - block the calling thread;
/// - spawn its own threads or async tasks;
/// - implement its own retry delay or lifecycle policy;
/// - hold a shared feed or registry lock while performing I/O;
/// - exceed the byte, unit, or time limits in `budget`.
pub trait ProtocolEngine {
    /// The feed type this engine consumes.
    type Feed: EgressFeed;
    /// The transport handle (e.g. a raw TCP socket, SRT socket).
    type Transport;

    /// Advance the engine by one scheduler visit.
    ///
    /// `readiness` describes which I/O directions are currently ready.
    /// `cursor` is advanced in-place as units are consumed.
    /// Returns why the engine stopped.
    fn advance(
        &mut self,
        transport: &mut Self::Transport,
        readiness: Readiness,
        feed: &Self::Feed,
        cursor: &mut FeedCursor,
        budget: WorkBudget,
    ) -> EngineProgress;

    /// Close the transport cleanly.
    ///
    /// The engine should release connection state and flush a graceful close
    /// byte sequence where the protocol supports it. Must not block.
    fn close(&mut self, transport: &mut Self::Transport, reason: CloseReason);

    /// Advertise whether this engine can perform in-place resynchronization.
    fn recovery_capability(&self) -> RecoveryCapability;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_satisfies_interest() {
        assert!(Readiness::WRITABLE.satisfies(Interest::WRITE));
        assert!(!Readiness::WRITABLE.satisfies(Interest::READ));
        assert!(Readiness::BOTH.satisfies(Interest::READ_WRITE));
        assert!(Readiness::BOTH.satisfies(Interest::WRITE));
    }

    #[test]
    fn interest_empty() {
        assert!(Interest::NONE.is_empty());
        assert!(!Interest::READ.is_empty());
        assert!(!Interest::WRITE.is_empty());
    }

    #[test]
    fn readiness_default_satisfies_none() {
        assert!(Readiness::default().satisfies(Interest::NONE));
        assert!(!Readiness::default().satisfies(Interest::READ));
    }
}
