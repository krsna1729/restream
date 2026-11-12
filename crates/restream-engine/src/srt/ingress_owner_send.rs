//! The ingress Owner's send path to SRT read/play peers: driving direct-play
//! readers, per-peer window deferral and the overload disconnect. Split from
//! `ingress_owner` for size; it is part of the same owner-thread state.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::atomic::Ordering;

use bytes::Bytes;
use srt_proto::Timestamp;
use srt_transport::advanced::admission::LogicalPeerId;

use super::super::ingress_bridge::SessionId;
use tracing::warn;

use super::{DEFERRED_PER_PEER, DEFERRED_TOTAL, OwnerLoop, READER_SENDS_PER_VISIT};

/// Deferred read/play fragments, per peer, in arrival order.
pub(super) struct DeferredSends<K = LogicalPeerId> {
    pub(super) per_peer: HashMap<K, VecDeque<Bytes>>,
    pub(super) total: usize,
}

impl<K> Default for DeferredSends<K> {
    fn default() -> Self {
        Self {
            per_peer: HashMap::new(),
            total: 0,
        }
    }
}

impl<K: Hash + Eq + Copy> DeferredSends<K> {
    pub(super) fn len_for(&self, peer: &K) -> usize {
        self.per_peer.get(peer).map_or(0, VecDeque::len)
    }

    pub(super) fn push(&mut self, peer: K, payload: Bytes) {
        self.per_peer.entry(peer).or_default().push_back(payload);
        self.total += 1;
    }

    pub(super) fn forget(&mut self, peer: &K) {
        if let Some(queue) = self.per_peer.remove(peer) {
            self.total -= queue.len();
        }
    }

    /// The peer to disconnect as overloaded before `peer` may defer one more
    /// fragment, if any: `peer` itself past its own limit; otherwise, once
    /// the shared budget is spent, the peer with the largest backlog, so
    /// readers that stopped draining cannot get a healthy newcomer shed.
    pub(super) fn shed_for(&self, peer: &K) -> Option<K> {
        if self.len_for(peer) >= DEFERRED_PER_PEER {
            return Some(*peer);
        }
        if self.total < DEFERRED_TOTAL {
            return None;
        }
        self.per_peer
            .iter()
            .max_by_key(|(_, queue)| queue.len())
            .map(|(victim, _)| *victim)
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
        // Readers are listener peers: a caller session only publishes to us.
        let peers: Vec<LogicalPeerId> = self
            .media
            .readers
            .keys()
            .filter_map(|session| match session {
                SessionId::Peer(peer) => Some(*peer),
                SessionId::Caller(_) => None,
            })
            .collect();
        for peer in peers {
            if self.deferred.len_for(&peer) > 0 {
                continue;
            }
            let mut fragments = std::mem::take(&mut self.reader_scratch);
            if let Some(reader) = self.media.readers.get_mut(&SessionId::Peer(peer)) {
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
    /// an overloaded peer explicitly. Never a silent drop.
    fn defer(&mut self, peer: LogicalPeerId, payload: Bytes, now: Timestamp) {
        if let Some(victim) = self.deferred.shed_for(&peer) {
            warn!(peer = ?victim, "SRT reader is not draining; disconnecting the overloaded peer");
            self.stats
                .ingress_owner
                .overload_disconnects
                .fetch_add(1, Ordering::Relaxed);
            self.overloaded.insert(victim);
            self.deferred.forget(&victim);
            self.disconnect(SessionId::Peer(victim), now);
            if victim == peer {
                return;
            }
        }
        self.deferred.push(peer, payload);
        let stats = &self.stats.ingress_owner;
        super::publish(
            &stats.deferred_sends,
            &mut self.published.deferred_sends,
            self.deferred.total as u64,
        );
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
        super::publish(
            &self.stats.ingress_owner.deferred_sends,
            &mut self.published.deferred_sends,
            self.deferred.total as u64,
        );
    }
}

pub(super) enum SendOutcome {
    Sent,
    /// The peer's send window is closed right now.
    NoWindow,
    /// Nothing more can or should be delivered for this fragment.
    Dropped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spent_shared_budget_sheds_the_largest_backlog_not_the_newcomer() {
        let mut deferred = DeferredSends::<u64>::default();
        // Readers that stopped draining, each just under its own limit.
        let stalled = DEFERRED_TOTAL / (DEFERRED_PER_PEER - 1);
        for peer in 0..stalled as u64 {
            for _ in 0..DEFERRED_PER_PEER - 1 {
                deferred.push(peer, Bytes::new());
            }
        }
        let small = 1_000;
        while deferred.total < DEFERRED_TOTAL {
            deferred.push(small, Bytes::new());
        }
        assert!(deferred.len_for(&small) < DEFERRED_PER_PEER - 1);

        let newcomer = 9_999;
        let victim = deferred.shed_for(&newcomer).expect("the budget is spent");
        assert_ne!(victim, newcomer);
        assert_eq!(deferred.len_for(&victim), DEFERRED_PER_PEER - 1);
        deferred.forget(&victim);
        assert_eq!(deferred.shed_for(&newcomer), None);

        // A peer past its own limit is always the one shed.
        while deferred.len_for(&small) < DEFERRED_PER_PEER {
            deferred.push(small, Bytes::new());
        }
        assert_eq!(deferred.shed_for(&small), Some(small));
    }
}
