use std::time::{Duration, Instant};

use crate::egress::backend::{CloseReason, EngineProgress, Interest, Readiness, WaitCondition};
use crate::egress::ids::OutputId;
use crate::egress::journal::TsFeed;
use crate::egress::leaf::LeafCommon;
use crate::egress::policy::{LeafStallClass, WorkBudget};
use crate::egress::visit::{EngineVisitResult, visit_leaf};
use crate::snapshots::PublisherQuality;
use restream_srt::egress_stats::{SrtSendBacklog, send_backlog};
use srt_transport::advanced::caller::{LogicalCallerState, LogicalCallerStats};

use super::engine::SrtEgressEngine;
use super::stats::SenderQualitySampler;
use super::{SrtCaller, SrtLeafPressure, SrtOwners, apply_send_backlog};

/// One SRT output as Restream product state. It holds the transport only as
/// an identity ([`SrtCaller`]: family + `LogicalCallerId`); the socket,
/// caller table, TX pool and runtime belong to the shard's Owner.
pub(crate) struct SrtFabricLeaf {
    pub(super) common: LeafCommon,
    pub(super) engine: SrtEgressEngine,
    pub(super) caller: SrtCaller,
    /// Sender backlog observed at the previous stall check; a decline means
    /// the peer acknowledged data (protocol progress) even without new sends.
    last_backlog_bytes: u64,
    /// `pktSndDropTotal` observed at the previous stall check. A backlog
    /// decline only counts as progress when this counter did not advance:
    /// TLPKTDROP/TSBPD-deadline discards also shrink the buffer head, and a
    /// drop-riddled leaf must not extend its no-progress deadline forever.
    last_packets_sent_drop: u64,
    /// Anchor for stall aging before any progress has been recorded.
    observed_since: Instant,
    /// Sent-rate sampling state for this leaf's caller: the previous wire
    /// byte counter reading, so a cumulative counter becomes an interval rate.
    quality_sampler: SenderQualitySampler,
    /// Peer-acknowledged vs feed-published payload, rated over one window.
    delivery: crate::egress::delivery::DeliveryTracker,
    /// Set when this leaf has been asked to close but still had queued
    /// send-path bytes: it stays registered and visited so it can flush, and
    /// is force-closed once flushed or `drain_timeout` has passed.
    pub(super) draining_since: Option<Instant>,
    /// The reason to report once a draining leaf actually closes.
    pub(super) draining_reason: Option<CloseReason>,
    /// Parked in the backend's blocked queue (send window closed).
    pub(super) blocked_queued: bool,
    /// Attributed peer-local/transient TX failures observed for this caller.
    pub(super) tx_failures: u32,
    /// The caller has reached `Connected` and the cursor was anchored at the
    /// live start. Until then the leaf reads nothing from the feed and its
    /// feed lag is not measured.
    pub(super) connected: bool,
}

impl SrtFabricLeaf {
    pub(crate) fn new(common: LeafCommon, caller: SrtCaller) -> Self {
        Self {
            common,
            engine: SrtEgressEngine::default(),
            caller,
            last_backlog_bytes: 0,
            last_packets_sent_drop: 0,
            observed_since: Instant::now(),
            quality_sampler: SenderQualitySampler::default(),
            delivery: Default::default(),
            draining_since: None,
            draining_reason: None,
            blocked_queued: false,
            tx_failures: 0,
            connected: false,
        }
    }

    pub(crate) fn common(&self) -> &LeafCommon {
        &self.common
    }

    #[cfg(test)]
    pub(crate) fn caller(&self) -> SrtCaller {
        self.caller
    }

    pub(crate) fn pressure(&self, backlog: Option<SrtSendBacklog>) -> SrtLeafPressure {
        SrtLeafPressure {
            app_pending_bytes: self.engine.pending_message_bytes(),
            backlog,
        }
    }

    /// This leaf's caller statistics from its Owner.
    pub(super) fn stats(&self, owners: &SrtOwners) -> Option<LogicalCallerStats> {
        owners.stats(&self.caller)
    }

    /// This leaf's sender backlog from its Owner's caller statistics.
    pub(super) fn send_backlog(&self, owners: &SrtOwners) -> Option<SrtSendBacklog> {
        self.stats(owners).and_then(|stats| send_backlog(&stats))
    }

    /// Feed units this leaf is behind `head_sequence`; zero until its caller
    /// connected and the cursor was anchored.
    pub(super) fn feed_lag(&self, head_sequence: u64) -> u64 {
        if self.connected {
            self.common.feed_lag(head_sequence)
        } else {
            0
        }
    }

    pub(crate) fn observe_stall(
        &mut self,
        now: Instant,
        packets_sent_drop: Option<u64>,
        lag_units: u64,
        backlog: Option<SrtSendBacklog>,
    ) -> LeafStallClass {
        let pressure = self.pressure(backlog);
        let backlog_bytes = backlog.map_or(0, |backlog| backlog.bytes);
        let drops = packets_sent_drop.unwrap_or(self.last_packets_sent_drop);
        // A backlog decline without new drops: the peer acknowledged data.
        let acknowledged =
            backlog_bytes < self.last_backlog_bytes && drops <= self.last_packets_sent_drop;
        self.last_backlog_bytes = backlog_bytes;
        if let Some(drops) = packets_sent_drop {
            self.last_packets_sent_drop = drops;
        }
        self.common.stall_class(
            now,
            acknowledged,
            self.observed_since,
            pressure.pending_bytes(),
            lag_units,
        )
    }

    /// See [`LeafCommon::publish_health`].
    pub(super) fn publish_health(
        &self,
        lag_units: u64,
        reason: Option<&'static str>,
        quality: Option<PublisherQuality>,
    ) {
        self.common.publish_health(lag_units, reason, quality);
    }

    /// See [`LeafCommon::mark_terminated_unexpectedly`].
    pub(super) fn mark_terminated_unexpectedly(&self) {
        self.common.mark_terminated_unexpectedly();
    }

    /// See [`LeafCommon::join_feed_queue`].
    pub(super) fn join_feed_queue(&mut self) -> bool {
        self.common.join_feed_queue()
    }

    /// See [`LeafCommon::leave_feed_queue`].
    pub(super) fn leave_feed_queue(&mut self) -> bool {
        self.common.leave_feed_queue()
    }

    /// Claim this leaf's one entry in the backend's blocked queue: `true`
    /// (the caller must push it) when it is not parked there yet.
    pub(super) fn enter_blocked_queue(&mut self) -> bool {
        let enter = !self.blocked_queued;
        self.blocked_queued = true;
        enter
    }

    /// Give up the blocked-queue entry (re-examined, or refused by the
    /// queue).
    pub(super) fn leave_blocked_queue(&mut self) {
        debug_assert!(
            self.blocked_queued,
            "leaf left a blocked queue it was not in"
        );
        self.blocked_queued = false;
    }

    /// An attributed peer-local or transient TX failure for this caller.
    pub(super) fn record_tx_failure(&mut self) {
        self.tx_failures = self.tx_failures.saturating_add(1);
    }

    /// Asked to close with send-path bytes still queued: keep visiting the
    /// leaf so it flushes, then close it with `reason`.
    pub(super) fn begin_drain(&mut self, reason: CloseReason) {
        self.draining_since = Some(Instant::now());
        self.draining_reason = Some(reason);
    }

    pub(super) fn is_draining(&self) -> bool {
        self.draining_since.is_some()
    }

    /// A draining leaf that flushed (no backpressure left) or has drained
    /// for `timeout` closes now: the reason it was asked to close.
    pub(super) fn drain_finished(
        &self,
        backlog: Option<SrtSendBacklog>,
        now: Instant,
        timeout: Duration,
    ) -> Option<CloseReason> {
        let since = self.draining_since?;
        let flushed = !self.pressure(backlog).is_backpressured();
        (flushed || now.saturating_duration_since(since) >= timeout)
            .then(|| self.draining_reason.unwrap_or(CloseReason::Removed))
    }

    /// The stall sweep closes this leaf. One draining for a requested close
    /// (Remove, drain, shutdown) is expiring its drain window, not failing:
    /// only an undrained leaf that stalled terminated unexpectedly.
    pub(super) fn report_stall_close(&self, output_id: &OutputId, head_sequence: u64) {
        if self.is_draining() {
            return;
        }
        tracing::warn!(
            output_id = %output_id,
            tx_failures = self.tx_failures,
            blocked = self.blocked_queued,
            lag = head_sequence.saturating_sub(self.common.cursor.next_sequence),
            "srt egress leaf closed unexpectedly: no progress (stalled)"
        );
        self.common.mark_terminated_unexpectedly();
    }

    /// Close this leaf: drop its queued engine state and ask the Owner to
    /// retire its caller. Returns the caller for the backend to unindex.
    pub(super) fn retire(mut self, owners: &mut SrtOwners) -> SrtCaller {
        self.engine.clear();
        owners.begin_close(&self.caller);
        self.caller
    }

    /// Quality snapshot from one public statistics read at `now`, with the
    /// sender backlog folded in. Rates are measured against this leaf's
    /// previous sample, so this is a `&mut self` operation: it is called once
    /// per stall sweep (about 1 Hz), never per media visit.
    pub(crate) fn sample_quality(
        &mut self,
        stats: &LogicalCallerStats,
        now: Instant,
        feed_published_bytes: u64,
    ) -> Option<PublisherQuality> {
        let mut quality = self.quality_sampler.sample(stats, now)?;
        // Payload the peer's cumulative ACK confirmed (srt-rs
        // `total_bytes_acked`). An upper bound on what it played: a receiver
        // that drops late packets itself still ACKs past them. A bond has no
        // aggregate sender view, so its delivery stays unknown rather than
        // estimated.
        if let LogicalCallerStats::Direct(direct) = stats
            && let Some(sender) = direct.sender
        {
            let acknowledged = sender.total_bytes_acked;
            let delivery = self
                .delivery
                .sample(acknowledged, feed_published_bytes, now);
            quality.delivered_bps = delivery.delivered_bps;
            quality.offered_bps = delivery.offered_bps;
            quality.delivery_ratio = delivery.ratio;
        }
        if let Some(backlog) = restream_srt::egress_stats::send_backlog(stats) {
            apply_send_backlog(&mut quality, backlog);
        }
        Some(quality)
    }

    /// One scheduler visit. Payload fragments go to the shard's Owner through
    /// `send`; nothing here services the Owner.
    pub(crate) fn visit_ready(
        &mut self,
        generation: u64,
        readiness: Readiness,
        feed: &TsFeed,
        budget: WorkBudget,
        owners: &mut SrtOwners,
        now: srt_proto::Timestamp,
    ) -> EngineVisitResult {
        let caller = self.caller;
        if !self.connected {
            // The leaf exists from pool admission, before the handshake. A
            // unit pulled now would sit pending for the whole handshake,
            // reported as backpressure and aging behind live. A caller that
            // is gone takes the send path below, which reports the closure.
            let handshake = match owners.caller_state(&caller) {
                Some(LogicalCallerState::Connecting) => {
                    Some(EngineProgress::Needs(WaitCondition::Io(Interest::WRITE)))
                }
                Some(LogicalCallerState::Connected) => Some(EngineProgress::HandshakeComplete),
                None | Some(LogicalCallerState::Disconnected) => None,
            };
            if let Some(progress) = handshake {
                let result = visit_leaf(
                    generation,
                    &mut self.common,
                    feed,
                    readiness,
                    budget,
                    |_, _, _| progress,
                );
                self.connected = matches!(
                    &result,
                    EngineVisitResult::Visited(outcome)
                        if matches!(outcome.progress, EngineProgress::HandshakeComplete)
                );
                return result;
            }
        }
        let engine = &mut self.engine;
        visit_leaf(
            generation,
            &mut self.common,
            feed,
            readiness,
            budget,
            |cursor, readiness, budget| {
                engine.advance(readiness, feed, cursor, budget, &mut |fragment| {
                    owners.send(&caller, fragment, now)
                })
            },
        )
    }
}
