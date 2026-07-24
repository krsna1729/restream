#![allow(dead_code)]

use bytes::Bytes;
use std::marker::PhantomData;

use crate::media::egress::backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, ProtocolFailure, Readiness,
    RecoveryCapability,
};
use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::journal::TsFeed;
use crate::media::egress::policy::WorkBudget;

use super::srt_egress_sender::{SrtMessageSender, SrtSendResult};

/// Maximum bytes per `srt_send()` call in message mode: 7 × 188-byte MPEG-TS
/// packets, matching legacy SRT egress's fixed send buffer
/// (`src/media/srt_egress.rs`) and libsrt's live-mode payload ceiling.
///
/// A muxed TS feed unit is one chunk boundary from the shared muxer
/// (`src/media/srt/shared_muxer.rs`), which can span many packets — a
/// keyframe burst is commonly tens of KB. Sending a unit larger than this in
/// one call fails with SRT error 5009 ("Incorrect use of Message API");
/// legacy never hits this because it re-chunks the byte stream to 1316 bytes
/// on the way out regardless of original chunk boundaries. The engine
/// fragments a retained unit into ≤1316-byte pieces here instead, sending
/// one fragment per visit so a single visit's work stays bounded.
pub(super) const MAX_SRT_MESSAGE_PAYLOAD: usize = 1316;

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
    _transport: PhantomData<fn() -> T>,
}

impl<T> Default for SrtEgressEngine<T> {
    fn default() -> Self {
        Self {
            pending: None,
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

    fn send_pending(&mut self, transport: &mut T) -> EngineProgress
    where
        T: SrtMessageSender,
    {
        let Some(pending) = self.pending.as_mut() else {
            return EngineProgress::Needs(Interest::WRITE);
        };

        let fragment = pending.next_fragment();
        match transport.send_message(&fragment) {
            SrtSendResult::Accepted { bytes } => {
                pending.advance(bytes);
                let done = pending.is_complete();
                if done {
                    self.pending = None;
                }
                EngineProgress::Progress {
                    bytes,
                    units: usize::from(done),
                    interest: Interest::WRITE,
                }
            }
            SrtSendResult::WouldBlock => EngineProgress::Needs(Interest::WRITE),
            SrtSendResult::PeerClosed => EngineProgress::PeerClosed,
            SrtSendResult::Failed(failure) => EngineProgress::Failed(ProtocolFailure {
                reason: failure.reason,
                detail: failure.detail,
                retryable: failure.retryable,
            }),
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
                self.send_pending(transport)
            } else {
                EngineProgress::Needs(Interest::WRITE)
            };
        }

        let read_budget = ReadBudget::new(budget.max_units.min(1), budget.max_bytes);
        let (units, next_cursor) = match feed.read_from(*cursor, read_budget) {
            FeedRead::Units { units, next_cursor } => (units, next_cursor),
            FeedRead::Empty => return EngineProgress::Needs(Interest::NONE),
            FeedRead::Overrun { .. } | FeedRead::EpochMismatch { .. } => {
                return EngineProgress::FeedOverrun;
            }
        };

        let Some(message) = units.into_iter().next() else {
            return EngineProgress::Needs(Interest::NONE);
        };
        *cursor = next_cursor;
        self.pending = Some(PendingSrtMessage::new(message));

        if readiness.writable {
            self.send_pending(transport)
        } else {
            EngineProgress::Needs(Interest::WRITE)
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
