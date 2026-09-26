use bytes::Bytes;
use std::time::Instant;

use crate::media::egress::backend::{
    EngineProgress, Interest, ProtocolFailure, Readiness, WaitCondition,
};
use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::journal::TsFeed;
use crate::media::egress::policy::WorkBudget;

/// Maximum bytes per logical-caller message send: the SRT message payload
/// ceiling is seven 188-byte MPEG-TS packets. Muxed TS feed units can exceed
/// that limit, so the engine fragments each retained unit into bounded
/// messages and sends only as many fragments as the visit budget allows.
pub(super) const MAX_SRT_MESSAGE_PAYLOAD: usize = 1316;

/// Feed units pulled per `feed.read_from` refill once `pending_units` is
/// empty and no unit is currently being fragmented. Matches the RTMP fabric
/// engine's `FEED_READ_BURST` (`src/media/egress/backends/rtmp.rs`): reduces
/// how often `feed.read_from` runs (each call allocates a `Vec` and touches
/// ring atomics) without changing the existing one-unit-fragmented-per-visit
/// behavior below.
const FEED_READ_BURST: usize = 32;

/// What one logical-caller send did. Produced by the shard's Owner adapter
/// (`SrtOwners::send`) from the caller's logical state and `can_send`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SrtSendResult {
    Accepted {
        bytes: usize,
    },
    /// The caller cannot take payload right now (still connecting, or its
    /// send window is full). Never retried in a loop: the leaf parks.
    WouldBlock,
    PeerClosed,
    Failed {
        reason: &'static str,
        detail: String,
        retryable: bool,
    },
}

#[derive(Debug)]
struct PendingSrtMessage {
    bytes: Bytes,
    offset: usize,
}

impl PendingSrtMessage {
    fn new(bytes: Bytes) -> Self {
        Self { bytes, offset: 0 }
    }

    fn next_fragment(&self) -> Bytes {
        let end = (self.offset + MAX_SRT_MESSAGE_PAYLOAD).min(self.bytes.len());
        self.bytes.slice(self.offset..end)
    }

    fn advance(&mut self, sent: usize) {
        self.offset += sent;
    }

    fn is_complete(&self) -> bool {
        self.offset >= self.bytes.len()
    }

    fn remaining_len(&self) -> usize {
        self.bytes.len() - self.offset
    }
}

/// Feed consumption and TS-unit fragmentation for one SRT leaf. It owns no
/// transport: payload is handed to the caller-supplied `send` (the shard's
/// Owner, addressed by the leaf's `LogicalCallerId`).
#[derive(Debug)]
pub(crate) struct SrtEgressEngine {
    pending: Option<PendingSrtMessage>,
    /// Units already pulled from the feed but not yet handed to `pending`
    /// for fragmentation. See `FEED_READ_BURST`.
    pending_units: Vec<Bytes>,
    pending_units_index: usize,
}

impl Default for SrtEgressEngine {
    fn default() -> Self {
        Self {
            pending: None,
            pending_units: Vec::with_capacity(FEED_READ_BURST),
            pending_units_index: 0,
        }
    }
}

impl SrtEgressEngine {
    pub(crate) fn pending_message_bytes(&self) -> usize {
        self.pending
            .as_ref()
            .map_or(0, PendingSrtMessage::remaining_len)
    }

    /// Drop any retained application message (leaf close).
    pub(crate) fn clear(&mut self) {
        self.pending = None;
        self.pending_units.clear();
        self.pending_units_index = 0;
    }

    /// Send as many `MAX_SRT_MESSAGE_PAYLOAD` fragments of the pending unit
    /// as the budget allows in one visit, instead of exactly one. A single
    /// fragment per visit is correct but costs a full wake/poll/visit cycle
    /// per fragment — for a keyframe-sized unit (tens of KB) that is dozens
    /// of cycles instead of one. Fragments only enter protocol state here;
    /// transport service is the shard's separate, once-per-batch phase, so a
    /// fragment never causes an Owner service pass. The byte/deadline budget
    /// still stops one always-writable leaf from monopolizing the shard.
    fn send_pending(
        &mut self,
        budget: WorkBudget,
        send: &mut impl FnMut(&Bytes) -> SrtSendResult,
    ) -> EngineProgress {
        if self.pending.is_none() {
            return EngineProgress::Needs(WaitCondition::Io(Interest::WRITE));
        }

        let mut total_bytes = 0usize;
        loop {
            let Some(pending) = self.pending.as_mut() else {
                return EngineProgress::Progress {
                    bytes: total_bytes,
                    units: 1,
                    wait: WaitCondition::Io(Interest::WRITE),
                };
            };

            let fragment = pending.next_fragment();
            match send(&fragment) {
                SrtSendResult::Accepted { bytes } => {
                    pending.advance(bytes);
                    total_bytes += bytes;
                    if pending.is_complete() {
                        self.pending = None;
                        return EngineProgress::Progress {
                            bytes: total_bytes,
                            units: 1,
                            wait: WaitCondition::Io(Interest::WRITE),
                        };
                    }
                    if total_bytes >= budget.max_bytes || Instant::now() >= budget.deadline {
                        return EngineProgress::Progress {
                            bytes: total_bytes,
                            units: 0,
                            wait: WaitCondition::Io(Interest::WRITE),
                        };
                    }
                    // Budget allows another fragment: loop without
                    // returning to the shard scheduler.
                }
                SrtSendResult::WouldBlock => {
                    return if total_bytes > 0 {
                        EngineProgress::Progress {
                            bytes: total_bytes,
                            units: 0,
                            wait: WaitCondition::Io(Interest::WRITE),
                        }
                    } else {
                        EngineProgress::Needs(WaitCondition::Io(Interest::WRITE))
                    };
                }
                SrtSendResult::PeerClosed => return EngineProgress::PeerClosed,
                SrtSendResult::Failed {
                    reason,
                    detail,
                    retryable,
                } => {
                    return EngineProgress::Failed(ProtocolFailure {
                        reason,
                        detail,
                        retryable,
                    });
                }
            }
        }
    }

    pub(crate) fn advance(
        &mut self,
        readiness: Readiness,
        feed: &TsFeed,
        cursor: &mut FeedCursor,
        budget: WorkBudget,
        send: &mut impl FnMut(&Bytes) -> SrtSendResult,
    ) -> EngineProgress {
        if self.pending.is_some() {
            return if readiness.writable {
                self.send_pending(budget, send)
            } else {
                EngineProgress::Needs(WaitCondition::Io(Interest::WRITE))
            };
        }

        if self.pending_units_index >= self.pending_units.len() {
            self.pending_units.clear();
            self.pending_units_index = 0;
            match feed.read_from_into(
                *cursor,
                ReadBudget::new(FEED_READ_BURST, budget.max_bytes),
                &mut self.pending_units,
            ) {
                FeedRead::Units { next_cursor, .. } => *cursor = next_cursor,
                FeedRead::Empty => return EngineProgress::Needs(WaitCondition::Feed),
                FeedRead::Overrun { .. } | FeedRead::EpochMismatch { .. } => {
                    return EngineProgress::FeedOverrun;
                }
            }
        }

        let Some(message) = self.pending_units.get(self.pending_units_index).cloned() else {
            return EngineProgress::Needs(WaitCondition::Feed);
        };
        self.pending_units_index += 1;
        self.pending = Some(PendingSrtMessage::new(message));

        if readiness.writable {
            self.send_pending(budget, send)
        } else {
            EngineProgress::Needs(WaitCondition::Io(Interest::WRITE))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::egress::feed::FeedCursor;
    use crate::media::egress::journal::FeedEpoch;
    use crate::media::ts_chunk_ring::TsChunkRing;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    fn feed_with(unit: Bytes) -> (TsFeed, FeedCursor) {
        let ring = TsChunkRing::new(8, CancellationToken::new());
        let feed = TsFeed::new(&ring, Arc::new(FeedEpoch::new()));
        let cursor = FeedCursor::new(feed.epoch(), feed.head_sequence());
        ring.push(unit, true);
        (feed, cursor)
    }

    fn writable() -> Readiness {
        Readiness {
            readable: false,
            writable: true,
        }
    }

    fn generous() -> WorkBudget {
        WorkBudget::new(32, 1 << 20, Duration::from_secs(5))
    }

    /// O. Fragments handed to the transport are zero-copy slices of the feed's
    /// own allocation: no per-leaf payload copy or queue is restored.
    #[test]
    fn fragments_are_zero_copy_slices_of_the_feed_unit() {
        let unit = Bytes::from(vec![0x47u8; 3_000]);
        let (feed, mut cursor) = feed_with(unit);
        let mut engine = SrtEgressEngine::default();
        let mut fragments: Vec<Bytes> = Vec::new();
        let progress = engine.advance(
            writable(),
            &feed,
            &mut cursor,
            generous(),
            &mut |fragment| {
                fragments.push(fragment.clone());
                SrtSendResult::Accepted {
                    bytes: fragment.len(),
                }
            },
        );

        assert!(matches!(
            progress,
            EngineProgress::Progress { units: 1, .. }
        ));
        let sizes: Vec<usize> = fragments.iter().map(Bytes::len).collect();
        assert_eq!(
            sizes,
            vec![1316, 1316, 368],
            "TS unit fragmentation is preserved"
        );
        let base = fragments[0].as_ptr() as usize;
        for (index, fragment) in fragments.iter().enumerate() {
            assert_eq!(
                fragment.as_ptr() as usize,
                base + index * 1316,
                "fragment {index} points into the same allocation (no copy)"
            );
        }
    }

    /// P. A caller that cannot send makes the engine stop after ONE attempt:
    /// it reports that it needs the window to open, it never retries in a loop.
    #[test]
    fn would_block_stops_after_one_attempt_without_spinning() {
        let (feed, mut cursor) = feed_with(Bytes::from(vec![0x47u8; 1316]));
        let mut engine = SrtEgressEngine::default();
        let mut attempts = 0;
        let progress = engine.advance(writable(), &feed, &mut cursor, generous(), &mut |_| {
            attempts += 1;
            SrtSendResult::WouldBlock
        });
        assert_eq!(attempts, 1);
        assert!(matches!(
            progress,
            EngineProgress::Needs(WaitCondition::Io(_))
        ));
        assert_eq!(
            engine.pending_message_bytes(),
            1316,
            "the unit is retained, not dropped"
        );
    }

    /// Steady state: fragmenting and handing a warmed unit stream to the
    /// transport allocates nothing (scratch is reused, fragments are shared
    /// slices).
    #[test]
    fn fragmenting_allocates_nothing_after_warm_up() {
        let ring = TsChunkRing::new(64, CancellationToken::new());
        let feed = TsFeed::new(&ring, Arc::new(FeedEpoch::new()));
        let mut cursor = FeedCursor::new(feed.epoch(), feed.head_sequence());
        for _ in 0..40 {
            ring.push(Bytes::from(vec![0x47u8; 3_000]), true);
        }
        let mut engine = SrtEgressEngine::default();
        let mut sent = 0usize;
        let mut send = |fragment: &Bytes| {
            sent += fragment.len();
            SrtSendResult::Accepted {
                bytes: fragment.len(),
            }
        };
        // Warm: first read fills the reusable unit vector.
        engine.advance(writable(), &feed, &mut cursor, generous(), &mut send);
        crate::test_alloc::begin();
        for _ in 0..30 {
            engine.advance(writable(), &feed, &mut cursor, generous(), &mut send);
        }
        let allocations = crate::test_alloc::end();
        assert_eq!(
            allocations, 0,
            "{allocations} allocations in 30 steady-state visits"
        );
        assert!(sent >= 30 * 3_000);
    }

    /// R. The visit's byte budget bounds fragments sent in one visit.
    #[test]
    fn visit_byte_budget_bounds_fragments_per_visit() {
        let (feed, mut cursor) = feed_with(Bytes::from(vec![0x47u8; 5_000]));
        let mut engine = SrtEgressEngine::default();
        let mut sent = 0;
        let budget = WorkBudget::new(32, 1316, Duration::from_secs(5));
        let progress = engine.advance(writable(), &feed, &mut cursor, budget, &mut |fragment| {
            sent += 1;
            SrtSendResult::Accepted {
                bytes: fragment.len(),
            }
        });
        assert_eq!(sent, 1, "one fragment fills a 1316-byte visit budget");
        assert!(matches!(
            progress,
            EngineProgress::Progress { units: 0, .. }
        ));
        assert!(
            engine.pending_message_bytes() > 0,
            "the rest waits for the next visit"
        );
    }
}
