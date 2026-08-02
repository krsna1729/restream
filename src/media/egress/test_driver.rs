//! Test driver: `FakeFeed`, `FakeEngine`, and `FakePoller`.
//!
//! These are deterministic replacements for the real network and media
//! machinery. They allow the scheduler, lifecycle, and policy tests to run
//! without any real sockets, wall-clock sleeps, or media files.
//!
//! The test driver is compiled only when `cfg(test)` or the
//! `egress-test-driver` crate feature is active.

use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::media::egress::backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, Readiness, RecoveryCapability,
    WaitCondition,
};
use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::policy::WorkBudget;

// ---------------------------------------------------------------------------
// FakeFeed
// ---------------------------------------------------------------------------

/// A scripted feed that produces pre-loaded `Bytes` units in sequence.
///
/// Supports epoch changes, overrun injection, and sync-point placement.
#[derive(Debug, Clone)]
pub struct FakeFeed {
    inner: Arc<Mutex<FakeFeedInner>>,
}

#[derive(Debug)]
struct FakeFeedInner {
    epoch: u64,
    units: VecDeque<Bytes>,
    base_sequence: u64,
    /// Total units ever published (head = base + units.len()).
    total_published: u64,
    /// Sequences that are sync points (keyframe equivalents).
    sync_points: Vec<u64>,
    /// Oldest sequence still retained.
    oldest_sequence: u64,
    /// If set, `read_from` returns Overrun for any cursor before this sequence.
    overrun_at: Option<u64>,
}

impl FakeFeed {
    /// Create an empty feed starting at epoch 0, sequence 0.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeFeedInner {
                epoch: 0,
                units: VecDeque::new(),
                base_sequence: 0,
                total_published: 0,
                sync_points: Vec::new(),
                oldest_sequence: 0,
                overrun_at: None,
            })),
        }
    }

    /// Push a new unit into the feed.
    pub fn push(&self, data: Bytes, is_sync_point: bool) {
        let mut inner = self.inner.lock().unwrap();
        let seq = inner.base_sequence + inner.total_published;
        if is_sync_point {
            inner.sync_points.push(seq);
        }
        inner.units.push_back(data);
        inner.total_published += 1;
    }

    /// Convenience: push a plain-text unit.
    pub fn push_str(&self, s: &str, is_sync_point: bool) {
        self.push(Bytes::copy_from_slice(s.as_bytes()), is_sync_point);
    }

    /// Advance the epoch (simulates a source replacement).
    pub fn advance_epoch(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.epoch += 1;
    }

    /// Force overrun: any cursor with `next_sequence < seq` reports Overrun.
    pub fn set_overrun_at(&self, seq: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.oldest_sequence = seq;
        inner.overrun_at = Some(seq);
    }
}

impl Default for FakeFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl EgressFeed for FakeFeed {
    type Unit = Bytes;

    fn head_sequence(&self) -> u64 {
        let inner = self.inner.lock().unwrap();
        inner.base_sequence + inner.total_published
    }

    fn oldest_sequence(&self) -> u64 {
        self.inner.lock().unwrap().oldest_sequence
    }

    fn epoch(&self) -> u64 {
        self.inner.lock().unwrap().epoch
    }

    fn read_from(&self, cursor: FeedCursor, budget: ReadBudget) -> FeedRead<Bytes> {
        let inner = self.inner.lock().unwrap();

        // Epoch mismatch check.
        if cursor.epoch != inner.epoch {
            return FeedRead::EpochMismatch {
                current_epoch: inner.epoch,
            };
        }

        // Overrun check.
        if inner
            .overrun_at
            .is_some_and(|overrun| cursor.next_sequence < overrun)
        {
            return FeedRead::Overrun {
                oldest_sequence: inner.oldest_sequence,
            };
        }

        let head = inner.base_sequence + inner.total_published;
        if cursor.next_sequence >= head {
            return FeedRead::Empty;
        }

        let start_offset = (cursor.next_sequence - inner.base_sequence) as usize;
        let available = inner.units.len().saturating_sub(start_offset);
        let take = available.min(budget.max_units);

        if take == 0 {
            return FeedRead::Empty;
        }

        let mut total_bytes = 0usize;
        let mut units = Vec::with_capacity(take);
        for i in 0..take {
            let unit = inner.units[start_offset + i].clone();
            total_bytes += unit.len();
            units.push(unit);
            if total_bytes >= budget.max_bytes {
                break;
            }
        }

        let consumed = units.len() as u64;
        FeedRead::Units {
            units,
            next_cursor: FeedCursor::new(cursor.epoch, cursor.next_sequence + consumed),
        }
    }

    fn latest_sync_point(&self) -> Option<FeedCursor> {
        let inner = self.inner.lock().unwrap();
        inner
            .sync_points
            .last()
            .map(|&seq| FeedCursor::new(inner.epoch, seq))
    }

    fn sync_point_at_or_after(&self, sequence: u64) -> Option<FeedCursor> {
        let inner = self.inner.lock().unwrap();
        inner
            .sync_points
            .iter()
            .find(|&&s| s >= sequence)
            .map(|&seq| FeedCursor::new(inner.epoch, seq))
    }
}

// ---------------------------------------------------------------------------
// FakeTransport
// ---------------------------------------------------------------------------

/// A minimal fake transport handle. Carries the scripted readiness and
/// records calls made by the engine.
#[derive(Debug, Default)]
pub struct FakeTransport {
    /// Readiness the transport will report on the next advance call.
    pub readiness: Readiness,
    pub bytes_written: usize,
    pub close_count: u32,
}

// ---------------------------------------------------------------------------
// EngineScript — scripted engine behavior
// ---------------------------------------------------------------------------

/// A scripted behavior for one `advance()` call.
#[derive(Debug, Clone)]
pub enum EngineScript {
    /// Consume `units` units and `bytes` bytes; report the residual wait
    /// condition.
    Progress {
        bytes: usize,
        units: usize,
        wait: WaitCondition,
    },
    /// Report that the transport needs the given wait condition satisfied.
    Needs(WaitCondition),
    /// Signal handshake completion.
    HandshakeComplete,
    /// Report feed overrun.
    FeedOverrun,
    /// Report peer closed.
    PeerClosed,
    /// Report a failure.
    Fail {
        reason: &'static str,
        retryable: bool,
    },
    /// Yield the visit.
    Yield,
}

// ---------------------------------------------------------------------------
// FakeEngine
// ---------------------------------------------------------------------------

/// A deterministic protocol engine driven by a pre-loaded script.
///
/// Supports all scripted behaviors listed in the implementation doc:
/// - always makes progress
/// - always needs write readiness
/// - partial progress and blocks
/// - consumes CPU budget without bytes
/// - fails after a configured number of writes
/// - never completes handshake
/// - closes after becoming active
/// - reports feed overrun
/// - returns contradictory or zero progress
pub struct FakeEngine<F: EgressFeed = FakeFeed> {
    script: VecDeque<EngineScript>,
    pub advance_calls: usize,
    pub close_calls: u32,
    _phantom: std::marker::PhantomData<F>,
}

impl<F: EgressFeed> FakeEngine<F> {
    pub fn new(script: Vec<EngineScript>) -> Self {
        Self {
            script: script.into(),
            advance_calls: 0,
            close_calls: 0,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Create an engine that always reports progress.
    pub fn always_progress(bytes_per_call: usize, units_per_call: usize) -> Self {
        // Infinite script: use a repeating sentinel.
        let step = EngineScript::Progress {
            bytes: bytes_per_call,
            units: units_per_call,
            wait: WaitCondition::Io(Interest::WRITE),
        };
        Self::new(vec![step; 1024]) // large but bounded for tests
    }

    /// Create an engine that immediately reports WouldBlock.
    pub fn always_blocks() -> Self {
        Self::new(vec![
            EngineScript::Needs(WaitCondition::Io(Interest::WRITE));
            1024
        ])
    }
}

impl<F: EgressFeed<Unit = Bytes>> ProtocolEngine for FakeEngine<F> {
    type Feed = F;
    type Transport = FakeTransport;

    fn advance(
        &mut self,
        transport: &mut FakeTransport,
        _readiness: Readiness,
        _feed: &F,
        cursor: &mut FeedCursor,
        _budget: WorkBudget,
    ) -> EngineProgress {
        self.advance_calls += 1;
        let step = self.script.pop_front().unwrap_or(EngineScript::Yield);
        match step {
            EngineScript::Progress { bytes, units, wait } => {
                transport.bytes_written += bytes;
                cursor.next_sequence += units as u64;
                EngineProgress::Progress { bytes, units, wait }
            }
            EngineScript::Needs(i) => EngineProgress::Needs(i),
            EngineScript::HandshakeComplete => EngineProgress::HandshakeComplete,
            EngineScript::FeedOverrun => EngineProgress::FeedOverrun,
            EngineScript::PeerClosed => EngineProgress::PeerClosed,
            EngineScript::Fail { reason, retryable } => {
                EngineProgress::Failed(crate::media::egress::backend::ProtocolFailure {
                    reason,
                    detail: "scripted failure".into(),
                    retryable,
                })
            }
            EngineScript::Yield => EngineProgress::Yield,
        }
    }

    fn close(&mut self, transport: &mut FakeTransport, _reason: CloseReason) {
        transport.close_count += 1;
        self.close_calls += 1;
    }

    fn recovery_capability(&self) -> RecoveryCapability {
        RecoveryCapability::ReconnectOnly
    }
}

// ---------------------------------------------------------------------------
// FakePoller
// ---------------------------------------------------------------------------

/// A test-controlled readiness source.
///
/// The test sets the next readiness event; `poll()` returns it immediately.
#[derive(Debug, Default)]
pub struct FakePoller {
    pub events: VecDeque<(usize, Readiness)>, // (leaf_key, readiness)
}

impl FakePoller {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a readiness event for `leaf_key`.
    pub fn push_event(&mut self, leaf_key: usize, readiness: Readiness) {
        self.events.push_back((leaf_key, readiness));
    }

    /// Return the next pending event without blocking.
    pub fn poll_nonblocking(&mut self) -> Option<(usize, Readiness)> {
        self.events.pop_front()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::egress::feed::ReadBudget;
    use crate::media::egress::policy::WorkBudget;
    use std::time::Duration;

    fn cursor_at(seq: u64) -> FeedCursor {
        FeedCursor::new(0, seq)
    }

    fn budget() -> WorkBudget {
        WorkBudget::new(64, 512 * 1024, Duration::from_secs(10))
    }

    // -----------------------------------------------------------------------
    // FakeFeed tests
    // -----------------------------------------------------------------------

    #[test]
    fn fake_feed_read_empty() {
        let feed = FakeFeed::new();
        let r = feed.read_from(cursor_at(0), ReadBudget::default());
        assert!(matches!(r, FeedRead::Empty));
    }

    #[test]
    fn fake_feed_reads_pushed_units() {
        let feed = FakeFeed::new();
        feed.push_str("hello", true);
        feed.push_str("world", false);

        let r = feed.read_from(cursor_at(0), ReadBudget::default());
        if let FeedRead::Units { units, next_cursor } = r {
            assert_eq!(units.len(), 2);
            assert_eq!(next_cursor.next_sequence, 2);
        } else {
            panic!("expected Units, got {r:?}");
        }
    }

    #[test]
    fn fake_feed_respects_budget_units() {
        let feed = FakeFeed::new();
        for i in 0..10 {
            feed.push(Bytes::from(vec![i as u8; 4]), false);
        }
        let r = feed.read_from(cursor_at(0), ReadBudget::new(3, usize::MAX));
        if let FeedRead::Units { units, .. } = r {
            assert!(units.len() <= 3);
        } else {
            panic!("expected Units");
        }
    }

    #[test]
    fn fake_feed_overrun() {
        let feed = FakeFeed::new();
        feed.push_str("a", false);
        feed.push_str("b", false);
        feed.set_overrun_at(2); // oldest = 2, cursor at 0 → overrun.

        let r = feed.read_from(cursor_at(0), ReadBudget::default());
        assert!(matches!(r, FeedRead::Overrun { .. }));
    }

    #[test]
    fn fake_feed_epoch_mismatch() {
        let feed = FakeFeed::new();
        feed.advance_epoch(); // epoch now 1

        // Cursor has epoch 0.
        let r = feed.read_from(cursor_at(0), ReadBudget::default());
        assert!(matches!(r, FeedRead::EpochMismatch { current_epoch: 1 }));
    }

    #[test]
    fn fake_feed_sync_points() {
        let feed = FakeFeed::new();
        feed.push_str("kf", true); // seq 0 → sync point
        feed.push_str("p", false); // seq 1
        feed.push_str("kf2", true); // seq 2 → sync point

        assert_eq!(feed.latest_sync_point().unwrap().next_sequence, 2);
        assert_eq!(feed.sync_point_at_or_after(1).unwrap().next_sequence, 2);
        assert!(feed.sync_point_at_or_after(3).is_none());
    }

    // -----------------------------------------------------------------------
    // FakeEngine tests
    // -----------------------------------------------------------------------

    #[test]
    fn fake_engine_scripted_progress() {
        let mut engine: FakeEngine<FakeFeed> = FakeEngine::new(vec![
            EngineScript::Progress {
                bytes: 100,
                units: 2,
                wait: WaitCondition::Io(Interest::WRITE),
            },
            EngineScript::Needs(WaitCondition::Io(Interest::WRITE)),
        ]);
        let feed = FakeFeed::new();
        let mut transport = FakeTransport::default();
        let mut cursor = cursor_at(0);
        let b = budget();

        let p = engine.advance(&mut transport, Readiness::WRITABLE, &feed, &mut cursor, b);
        assert!(matches!(
            p,
            EngineProgress::Progress {
                bytes: 100,
                units: 2,
                ..
            }
        ));
        assert_eq!(transport.bytes_written, 100);
        assert_eq!(cursor.next_sequence, 2);

        let p2 = engine.advance(&mut transport, Readiness::WRITABLE, &feed, &mut cursor, b);
        assert!(matches!(
            p2,
            EngineProgress::Needs(WaitCondition::Io(Interest { writable: true, .. }))
        ));
    }

    #[test]
    fn fake_engine_handshake_complete() {
        let mut engine: FakeEngine<FakeFeed> =
            FakeEngine::new(vec![EngineScript::HandshakeComplete]);
        let feed = FakeFeed::new();
        let mut transport = FakeTransport::default();
        let mut cursor = cursor_at(0);
        let r = engine.advance(
            &mut transport,
            Readiness::BOTH,
            &feed,
            &mut cursor,
            budget(),
        );
        assert!(matches!(r, EngineProgress::HandshakeComplete));
    }

    #[test]
    fn fake_engine_close_called() {
        let mut engine: FakeEngine<FakeFeed> = FakeEngine::new(vec![]);
        let mut transport = FakeTransport::default();
        engine.close(&mut transport, CloseReason::Removed);
        assert_eq!(engine.close_calls, 1);
        assert_eq!(transport.close_count, 1);
    }

    // -----------------------------------------------------------------------
    // FakePoller tests
    // -----------------------------------------------------------------------

    #[test]
    fn fake_poller_queues_events() {
        let mut poller = FakePoller::new();
        poller.push_event(0, Readiness::WRITABLE);
        poller.push_event(1, Readiness::READABLE);

        assert_eq!(poller.poll_nonblocking(), Some((0, Readiness::WRITABLE)));
        assert_eq!(poller.poll_nonblocking(), Some((1, Readiness::READABLE)));
        assert_eq!(poller.poll_nonblocking(), None);
    }
}
