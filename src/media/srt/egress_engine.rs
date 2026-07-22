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

#[derive(Debug, Default)]
pub(super) struct SrtEgressEngine<T> {
    pending: Option<Bytes>,
    _transport: PhantomData<fn() -> T>,
}

impl<T> SrtEgressEngine<T> {
    pub(super) fn pending_message_bytes(&self) -> usize {
        self.pending.as_ref().map_or(0, Bytes::len)
    }

    fn send_pending(&mut self, transport: &mut T) -> EngineProgress
    where
        T: SrtMessageSender,
    {
        let Some(message) = self.pending.as_ref() else {
            return EngineProgress::Needs(Interest::WRITE);
        };

        match transport.send_message(message) {
            SrtSendResult::Accepted { bytes } => {
                self.pending = None;
                EngineProgress::Progress {
                    bytes,
                    units: 0,
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
        self.pending = Some(message);

        if readiness.writable {
            match self.send_pending(transport) {
                EngineProgress::Progress {
                    bytes, interest, ..
                } => EngineProgress::Progress {
                    bytes,
                    units: 1,
                    interest,
                },
                other => other,
            }
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
