use std::time::Instant;

use crate::media::egress::backend::{CloseReason, Readiness};
use crate::media::egress::journal::TsFeed;
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::{LeafStallClass, WorkBudget, classify_stall};
use crate::media::egress::visit::{EngineVisitResult, visit_leaf};
use crate::media::snapshots::PublisherQuality;
use crate::media::srt::egress_stats::SenderQualitySampler;
use crate::media::srt::{SrtEgressEngine, SrtSendBacklog};
use srt_transport::advanced::caller::LogicalCallerStats;

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
            draining_since: None,
            draining_reason: None,
            blocked_queued: false,
            tx_failures: 0,
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
        if backlog_bytes < self.last_backlog_bytes && drops <= self.last_packets_sent_drop {
            self.common.progress.last_protocol_progress = Some(now);
        }
        self.last_backlog_bytes = backlog_bytes;
        if let Some(drops) = packets_sent_drop {
            self.last_packets_sent_drop = drops;
        }

        let last_progress = self
            .common
            .progress
            .last_byte_progress
            .into_iter()
            .chain(self.common.progress.last_protocol_progress)
            .max()
            .unwrap_or(self.observed_since);
        let age = now.saturating_duration_since(last_progress);
        classify_stall(
            pressure.pending_bytes(),
            age,
            lag_units,
            &self.common.limits,
        )
    }

    /// Quality snapshot from one public statistics read at `now`, with the
    /// sender backlog folded in. Rates are measured against this leaf's
    /// previous sample, so this is a `&mut self` operation: it is called once
    /// per stall sweep (about 1 Hz), never per media visit.
    pub(crate) fn sample_quality(
        &mut self,
        stats: &LogicalCallerStats,
        now: Instant,
    ) -> Option<PublisherQuality> {
        let mut quality = self.quality_sampler.sample(stats, now)?;
        if let Some(backlog) = crate::media::srt::egress_stats::send_backlog(stats) {
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
