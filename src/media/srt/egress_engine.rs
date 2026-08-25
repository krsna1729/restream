use bytes::Bytes;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::time::Instant;

use crate::media::egress::backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, ProtocolFailure, Readiness,
    RecoveryCapability, WaitCondition,
};
use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::journal::TsFeed;
use crate::media::egress::policy::WorkBudget;

use super::{SrtMessageSender, SrtSendResult};

/// Maximum bytes per `srt_send()` call in message mode: 7 × 188-byte MPEG-TS
/// packets, matching legacy SRT egress's fixed send buffer
/// (the historical SRT egress path) and the live-mode payload ceiling.
///
/// A muxed TS feed unit is one chunk boundary from the shared muxer
/// (`src/media/srt/shared_muxer.rs`), which can span many packets — a
/// keyframe burst is commonly tens of KB. Sending a unit larger than this in
/// one call fails with SRT error 5009 ("Incorrect use of Message API");
/// legacy never hits this because it re-chunks the byte stream to 1316 bytes
/// on the way out regardless of original chunk boundaries. The engine
/// fragments a retained unit into ≤1316-byte pieces here instead, sending as
/// many fragments as the visit's budget allows (see `send_pending`) so a
/// single visit's work stays bounded without costing one scheduler cycle per
/// fragment.
pub(super) const MAX_SRT_MESSAGE_PAYLOAD: usize = 1316;

/// Feed units pulled per `feed.read_from` refill once `pending_units` is
/// empty and no unit is currently being fragmented. Matches the RTMP fabric
/// engine's `FEED_READ_BURST` (`src/media/egress/backends/rtmp.rs`): reduces
/// how often `feed.read_from` runs (each call allocates a `Vec` and touches
/// ring atomics) without changing the existing one-unit-fragmented-per-visit
/// behavior below.
const FEED_READ_BURST: usize = 32;

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

#[derive(Debug)]
pub(crate) struct SrtEgressEngine<T> {
    pending: Option<PendingSrtMessage>,
    /// Units already pulled from the feed but not yet handed to `pending`
    /// for fragmentation. See `FEED_READ_BURST`.
    pending_units: VecDeque<Bytes>,
    _transport: PhantomData<fn() -> T>,
}

impl<T> Default for SrtEgressEngine<T> {
    fn default() -> Self {
        Self {
            pending: None,
            pending_units: VecDeque::new(),
            _transport: PhantomData,
        }
    }
}

impl<T> SrtEgressEngine<T> {
    pub(crate) fn pending_message_bytes(&self) -> usize {
        self.pending
            .as_ref()
            .map_or(0, PendingSrtMessage::remaining_len)
    }

    /// Send as many `MAX_SRT_MESSAGE_PAYLOAD` fragments of the pending unit
    /// as the budget allows in one visit, instead of exactly one. A single
    /// fragment per visit is correct but costs a full wake/poll/visit cycle
    /// per fragment — for a keyframe-sized unit (tens of KB) that is dozens
    /// of cycles instead of one, which live measurement showed as a real
    /// CPU regression (see `docs/egress-implementation.md` Phase 4 status).
    /// Looping here amortizes that cost across one scheduler visit while
    /// still respecting the visit's byte/deadline budget, so one slow or
    /// always-writable leaf still cannot monopolize the shard.
    fn send_pending(&mut self, transport: &mut T, budget: WorkBudget) -> EngineProgress
    where
        T: SrtMessageSender,
    {
        if self.pending.is_none() {
            return EngineProgress::Needs(WaitCondition::Io(Interest::WRITE));
        }

        let mut total_bytes = 0usize;
        loop {
            let Some(pending) = self.pending.as_mut() else {
                // Unit fully sent and cleared below; report what this visit
                // transferred.
                return EngineProgress::Progress {
                    bytes: total_bytes,
                    units: 1,
                    wait: WaitCondition::Io(Interest::WRITE),
                };
            };

            let fragment = pending.next_fragment();
            match transport.send_message(&fragment) {
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
}

impl<T> ProtocolEngine for SrtEgressEngine<T>
where
    T: SrtMessageSender,
{
    type Feed = TsFeed;
    type Transport = T;

    fn advance(
        &mut self,
        transport: &mut Self::Transport,
        readiness: Readiness,
        feed: &Self::Feed,
        cursor: &mut FeedCursor,
        budget: WorkBudget,
    ) -> EngineProgress {
        if self.pending.is_some() {
            return if readiness.writable {
                self.send_pending(transport, budget)
            } else {
                EngineProgress::Needs(WaitCondition::Io(Interest::WRITE))
            };
        }

        if self.pending_units.is_empty() {
            let read_budget = ReadBudget::new(FEED_READ_BURST, budget.max_bytes);
            match feed.read_from(*cursor, read_budget) {
                FeedRead::Units { units, next_cursor } => {
                    *cursor = next_cursor;
                    self.pending_units.extend(units);
                }
                FeedRead::Empty => return EngineProgress::Needs(WaitCondition::Feed),
                FeedRead::Overrun { .. } | FeedRead::EpochMismatch { .. } => {
                    return EngineProgress::FeedOverrun;
                }
            }
        }

        let Some(message) = self.pending_units.pop_front() else {
            return EngineProgress::Needs(WaitCondition::Feed);
        };
        self.pending = Some(PendingSrtMessage::new(message));

        if readiness.writable {
            self.send_pending(transport, budget)
        } else {
            EngineProgress::Needs(WaitCondition::Io(Interest::WRITE))
        }
    }

    fn close(&mut self, transport: &mut Self::Transport, reason: CloseReason) {
        self.pending = None;
        transport.close(reason);
    }

    fn recovery_capability(&self) -> RecoveryCapability {
        RecoveryCapability::ReconnectOnly
    }
}
