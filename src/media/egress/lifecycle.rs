//! `LeafLifecycle` state machine with exhaustive legal-transition table.
//!
//! The fabric owns all lifecycle transitions. A protocol engine reports
//! *events*; it does not choose an independent lifecycle.

use std::fmt;

// ---------------------------------------------------------------------------
// LeafLifecycle
// ---------------------------------------------------------------------------

/// All states a leaf may be in during its lifetime.
///
/// Transitions are validated by [`apply_event`]. Illegal transitions must not
/// occur — they indicate a fabric bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeafLifecycle {
    /// Leaf slot allocated; initial state after an `Add` command.
    Created,
    /// DNS resolution in progress.
    Resolving,
    /// TCP or SRT connect in progress.
    Connecting,
    /// Application-level handshake (RTMP or SRT negotiation) in progress.
    Handshaking,
    /// Leaf is delivering media.
    Active,
    /// Transport would block; leaf is waiting for writable readiness.
    Backpressured,
    /// Leaf fell behind the feed or received an epoch change; finding a new
    /// sync point before reconnecting.
    Resynchronizing,
    /// Connection failed or leaf is waiting for backoff delay.
    RetryWait,
    /// Leaf is draining and releasing resources.
    Closing,
    /// Leaf is fully stopped; slot may be reclaimed.
    Stopped,
}

impl LeafLifecycle {
    /// Returns `true` if this state represents an ongoing media delivery
    /// phase (not connecting, not waiting, not closing).
    pub fn is_active(self) -> bool {
        matches!(self, Self::Active | Self::Backpressured)
    }

    /// Returns `true` if the leaf has reached a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped)
    }

    /// Returns `true` if the leaf is consuming network resources.
    pub fn is_connected(self) -> bool {
        matches!(
            self,
            Self::Handshaking | Self::Active | Self::Backpressured | Self::Resynchronizing
        )
    }

    /// Returns `true` if the leaf is waiting for a timer before reconnecting.
    pub fn is_waiting(self) -> bool {
        matches!(self, Self::RetryWait)
    }
}

impl fmt::Display for LeafLifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Created => "created",
            Self::Resolving => "resolving",
            Self::Connecting => "connecting",
            Self::Handshaking => "handshaking",
            Self::Active => "active",
            Self::Backpressured => "backpressured",
            Self::Resynchronizing => "resynchronizing",
            Self::RetryWait => "retry_wait",
            Self::Closing => "closing",
            Self::Stopped => "stopped",
        };
        f.write_str(s)
    }
}

// ---------------------------------------------------------------------------
// LifecycleEvent
// ---------------------------------------------------------------------------

/// Events the fabric delivers to the lifecycle machine.
///
/// Protocol engines produce outcomes (`EngineProgress`); the fabric maps them
/// to these events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// DNS resolution completed successfully; proceed to connect.
    Resolved,
    /// DNS resolution failed or timed out.
    ResolveFailed,
    /// Transport connection established; begin handshake.
    Connected,
    /// Transport connection failed or timed out.
    ConnectFailed,
    /// Protocol handshake completed; begin media delivery.
    HandshakeComplete,
    /// Protocol handshake failed or timed out.
    HandshakeFailed,
    /// Transport would block; wait for writable readiness.
    WouldBlock,
    /// Writable readiness received; resume sending.
    Writable,
    /// Feed cursor fell behind the oldest retained entry.
    FeedOverrun,
    /// A sync point has been located; reconnect from it.
    SyncPointFound,
    /// Peer closed the connection gracefully.
    PeerClosed,
    /// A transport or protocol error occurred.
    Failure,
    /// Retry backoff timer expired; attempt to reconnect.
    RetryTimerExpired,
    /// Control plane issued a `Remove` or `DrainShard` command.
    RemoveRequested,
    /// The leaf has released all resources.
    CleanupComplete,
}

// ---------------------------------------------------------------------------
// Transition error
// ---------------------------------------------------------------------------

/// Returned when a lifecycle event is applied to an incompatible state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub from: LeafLifecycle,
    pub event: LifecycleEvent,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "illegal lifecycle transition: {:?} + {:?}",
            self.from, self.event
        )
    }
}

// ---------------------------------------------------------------------------
// Transition table
// ---------------------------------------------------------------------------

/// Apply a lifecycle event and return the next state, or an error if the
/// transition is illegal.
///
/// This is the canonical transition table. Every legal edge from the
/// architecture doc is listed; all others are rejected.
pub fn apply_event(
    state: LeafLifecycle,
    event: LifecycleEvent,
) -> Result<LeafLifecycle, IllegalTransition> {
    use LeafLifecycle::*;
    use LifecycleEvent::*;

    let next = match (state, event) {
        // Created
        (Created, Resolved) => Connecting,
        (Created, RemoveRequested) => Closing,

        // Resolving
        (Resolving, Resolved) => Connecting,
        (Resolving, ResolveFailed) => RetryWait,
        (Resolving, RemoveRequested) => Closing,

        // Connecting
        (Connecting, Connected) => Handshaking,
        (Connecting, ConnectFailed) => RetryWait,
        (Connecting, RemoveRequested) => Closing,

        // Handshaking
        (Handshaking, HandshakeComplete) => Active,
        (Handshaking, HandshakeFailed) => RetryWait,
        (Handshaking, PeerClosed) => RetryWait,
        (Handshaking, Failure) => RetryWait,
        (Handshaking, RemoveRequested) => Closing,

        // Active
        (Active, WouldBlock) => Backpressured,
        (Active, FeedOverrun) => Resynchronizing,
        (Active, PeerClosed) => RetryWait,
        (Active, Failure) => RetryWait,
        (Active, RemoveRequested) => Closing,

        // Backpressured
        (Backpressured, Writable) => Active,
        (Backpressured, FeedOverrun) => Resynchronizing,
        (Backpressured, PeerClosed) => RetryWait,
        (Backpressured, Failure) => RetryWait,
        (Backpressured, RemoveRequested) => Closing,

        // Resynchronizing
        (Resynchronizing, SyncPointFound) => Connecting,
        (Resynchronizing, Failure) => RetryWait,
        (Resynchronizing, RemoveRequested) => Closing,

        // RetryWait
        (RetryWait, RetryTimerExpired) => Resolving,
        (RetryWait, RemoveRequested) => Closing,

        // Closing
        (Closing, CleanupComplete) => Stopped,

        // No other transitions are legal.
        _ => {
            return Err(IllegalTransition { from: state, event });
        }
    };

    Ok(next)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use LeafLifecycle::*;
    use LifecycleEvent::*;

    // All documented legal transitions.
    const LEGAL: &[(LeafLifecycle, LifecycleEvent, LeafLifecycle)] = &[
        (Created, Resolved, Connecting),
        (Created, RemoveRequested, Closing),
        (Resolving, Resolved, Connecting),
        (Resolving, ResolveFailed, RetryWait),
        (Resolving, RemoveRequested, Closing),
        (Connecting, Connected, Handshaking),
        (Connecting, ConnectFailed, RetryWait),
        (Connecting, RemoveRequested, Closing),
        (Handshaking, HandshakeComplete, Active),
        (Handshaking, HandshakeFailed, RetryWait),
        (Handshaking, PeerClosed, RetryWait),
        (Handshaking, Failure, RetryWait),
        (Handshaking, RemoveRequested, Closing),
        (Active, WouldBlock, Backpressured),
        (Active, FeedOverrun, Resynchronizing),
        (Active, PeerClosed, RetryWait),
        (Active, Failure, RetryWait),
        (Active, RemoveRequested, Closing),
        (Backpressured, Writable, Active),
        (Backpressured, FeedOverrun, Resynchronizing),
        (Backpressured, PeerClosed, RetryWait),
        (Backpressured, Failure, RetryWait),
        (Backpressured, RemoveRequested, Closing),
        (Resynchronizing, SyncPointFound, Connecting),
        (Resynchronizing, Failure, RetryWait),
        (Resynchronizing, RemoveRequested, Closing),
        (RetryWait, RetryTimerExpired, Resolving),
        (RetryWait, RemoveRequested, Closing),
        (Closing, CleanupComplete, Stopped),
    ];

    #[test]
    fn all_legal_transitions_succeed() {
        for &(from, event, expected) in LEGAL {
            let result = apply_event(from, event);
            assert_eq!(
                result,
                Ok(expected),
                "legal transition {:?} + {:?} should yield {:?}, got {:?}",
                from,
                event,
                expected,
                result
            );
        }
    }

    #[test]
    fn stopped_is_terminal() {
        // No event from Stopped is legal (it is terminal).
        let all_events = [
            Resolved,
            ResolveFailed,
            Connected,
            ConnectFailed,
            HandshakeComplete,
            HandshakeFailed,
            WouldBlock,
            Writable,
            FeedOverrun,
            SyncPointFound,
            PeerClosed,
            Failure,
            RetryTimerExpired,
            RemoveRequested,
            CleanupComplete,
        ];
        for event in all_events {
            assert!(
                apply_event(Stopped, event).is_err(),
                "Stopped + {event:?} should be illegal"
            );
        }
    }

    #[test]
    fn remove_reachable_from_all_non_terminal() {
        let non_terminal = [
            Created,
            Resolving,
            Connecting,
            Handshaking,
            Active,
            Backpressured,
            Resynchronizing,
            RetryWait,
            Closing,
        ];
        for state in non_terminal {
            // Closing + RemoveRequested is not listed (use CleanupComplete there).
            if state == Closing {
                continue;
            }
            assert_eq!(
                apply_event(state, RemoveRequested),
                Ok(Closing),
                "{state:?} + RemoveRequested should reach Closing"
            );
        }
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        // A sample of clearly illegal edges.
        let illegal = [
            (Created, Writable),
            (Active, RetryTimerExpired),
            (Stopped, RemoveRequested),
            (RetryWait, WouldBlock),
            (Resynchronizing, Connected),
        ];
        for (from, event) in illegal {
            assert!(
                apply_event(from, event).is_err(),
                "{from:?} + {event:?} should be illegal"
            );
        }
    }

    #[test]
    fn helper_predicates() {
        assert!(Active.is_active());
        assert!(Backpressured.is_active());
        assert!(!Connecting.is_active());
        assert!(Stopped.is_terminal());
        assert!(!Active.is_terminal());
        assert!(Active.is_connected());
        assert!(!Created.is_connected());
        assert!(RetryWait.is_waiting());
        assert!(!Active.is_waiting());
    }
}
