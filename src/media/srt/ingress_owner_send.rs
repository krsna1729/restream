//! The ingress Owner's send path to SRT read/play peers: driving direct-play
//! readers, per-peer window deferral and the overload disconnect. Split from
//! `ingress_owner` for size; it is part of the same owner-thread state.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;

use bytes::Bytes;
use srt_proto::Timestamp;
use srt_transport::advanced::admission::LogicalPeerId;
use tracing::warn;

use super::{DEFERRED_PER_PEER, DEFERRED_TOTAL, OwnerLoop, READER_SENDS_PER_VISIT};

/// Deferred read/play fragments, per peer, in arrival order.
#[derive(Default)]
pub(super) struct DeferredSends {
    pub(super) per_peer: HashMap<LogicalPeerId, VecDeque<Bytes>>,
    pub(super) total: usize,
}

impl DeferredSends {
    pub(super) fn len_for(&self, peer: &LogicalPeerId) -> usize {
        self.per_peer.get(peer).map_or(0, VecDeque::len)
    }

    pub(super) fn push(&mut self, peer: LogicalPeerId, payload: Bytes) {
        self.per_peer.entry(peer).or_default().push_back(payload);
        self.total += 1;
    }

    pub(super) fn forget(&mut self, peer: &LogicalPeerId) {
        if let Some(queue) = self.per_peer.remove(peer) {
            self.total -= queue.len();
        }
    }
}

impl OwnerLoop {
    /// Pull each direct-play reader's next burst and send it through the
    /// existing per-peer send path (window deferral and overload disconnect
    /// included). A reader with fragments still deferred is skipped.
    pub(super) fn drive_readers(&mut self, now: Timestamp) {
        if !self.media.has_readers() {
            return;
        }
        let peers: Vec<LogicalPeerId> = self.media.readers.keys().copied().collect();
        for peer in peers {
            if self.deferred.len_for(&peer) > 0 {
                continue;
            }
            let mut fragments = std::mem::take(&mut self.reader_scratch);
            if let Some(reader) = self.media.readers.get_mut(&peer) {
                reader.refill();
                let count = reader.pending.len().min(READER_SENDS_PER_VISIT);
                fragments.extend(reader.pending.drain(..count));
            }
            for payload in fragments.drain(..) {
                self.send(peer, payload, now);
            }
            self.reader_scratch = fragments;
        }
    }

    pub(super) fn send(&mut self, peer: LogicalPeerId, payload: Bytes, now: Timestamp) {
        if self.overloaded.contains(&peer) {
            // Already being disconnected as overloaded.
            self.stats
                .ingress_owner
                .stale_commands
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        // Per-peer order: anything already waiting goes first.
        if self.deferred.len_for(&peer) > 0 {
            self.defer(peer, payload, now);
            return;
        }
        match self.try_send_now(&peer, &payload, now) {
            SendOutcome::Sent | SendOutcome::Dropped => {}
            SendOutcome::NoWindow => self.defer(peer, payload, now),
        }
    }

    fn try_send_now(
        &mut self,
        peer: &LogicalPeerId,
        payload: &Bytes,
        now: Timestamp,
    ) -> SendOutcome {
        let Some(mut entry) = self.owner.listener_peer_mut(*peer) else {
            // Retired (terminal event, overload, closing): harmless and counted.
            self.stats
                .ingress_owner
                .stale_commands
                .fetch_add(1, Ordering::Relaxed);
            return SendOutcome::Dropped;
        };
        if !entry.can_send() {
            return SendOutcome::NoWindow;
        }
        match entry.send_shared(payload.clone(), now) {
            Ok(_) => SendOutcome::Sent,
            Err(error) if error.kind == srt_proto::ErrorKind::InvalidState => {
                self.stats
                    .ingress_owner
                    .stale_commands
                    .fetch_add(1, Ordering::Relaxed);
                SendOutcome::Dropped
            }
            Err(error) => {
                warn!(peer = ?peer, %error, payload_len = payload.len(), "SRT reader send failed");
                self.stats
                    .ingress_owner
                    .send_failures
                    .fetch_add(1, Ordering::Relaxed);
                SendOutcome::Dropped
            }
        }
    }

    /// The peer's send window is closed: keep the fragment, bounded, or fail
    /// the overloaded peer explicitly. Never a silent drop.
    fn defer(&mut self, peer: LogicalPeerId, payload: Bytes, now: Timestamp) {
        if self.deferred.len_for(&peer) >= DEFERRED_PER_PEER
            || self.deferred.total >= DEFERRED_TOTAL
        {
            warn!(peer = ?peer, "SRT reader is not draining; disconnecting the overloaded peer");
            self.stats
                .ingress_owner
                .overload_disconnects
                .fetch_add(1, Ordering::Relaxed);
            self.overloaded.insert(peer);
            self.deferred.forget(&peer);
            self.disconnect(peer, now);
            return;
        }
        self.deferred.push(peer, payload);
        let stats = &self.stats.ingress_owner;
        stats
            .deferred_sends
            .store(self.deferred.total as u64, Ordering::Relaxed);
        stats
            .deferred_sends_hwm
            .fetch_max(self.deferred.total as u64, Ordering::Relaxed);
    }

    /// Flush deferred fragments for peers whose window has reopened. One pass
    /// over the (bounded) deferred set per visit.
    pub(super) fn retry_deferred(&mut self, now: Timestamp) {
        if self.deferred.total == 0 {
            return;
        }
        let peers: Vec<LogicalPeerId> = self.deferred.per_peer.keys().copied().collect();
        for peer in peers {
            while let Some(payload) = self
                .deferred
                .per_peer
                .get(&peer)
                .and_then(|queue| queue.front())
                .cloned()
            {
                match self.try_send_now(&peer, &payload, now) {
                    SendOutcome::Sent => {
                        if let Some(queue) = self.deferred.per_peer.get_mut(&peer) {
                            queue.pop_front();
                            self.deferred.total -= 1;
                        }
                    }
                    SendOutcome::Dropped => {
                        // The peer is gone or the fragment failed: nothing more
                        // can be delivered to it.
                        self.deferred.forget(&peer);
                        break;
                    }
                    SendOutcome::NoWindow => break,
                }
            }
            if self
                .deferred
                .per_peer
                .get(&peer)
                .is_some_and(VecDeque::is_empty)
            {
                self.deferred.per_peer.remove(&peer);
            }
        }
        self.stats
            .ingress_owner
            .deferred_sends
            .store(self.deferred.total as u64, Ordering::Relaxed);
    }
}

pub(super) enum SendOutcome {
    Sent,
    /// The peer's send window is closed right now.
    NoWindow,
    /// Nothing more can or should be delivered for this fragment.
    Dropped,
}
