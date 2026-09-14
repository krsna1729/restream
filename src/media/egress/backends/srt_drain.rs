//! Graceful-close/drain methods for [`SrtShardBackend`], split out of
//! `srt.rs` to stay under the source-audit line cap. Mirrors
//! `RtmpShardBackend`'s identical mechanism
//! (`src/media/egress/backends/rtmp_shard.rs`) exactly — see the doc
//! comments on `begin_graceful_close`/`sweep_draining_leaves` there for the
//! full rationale.

use super::*;

impl SrtShardBackend {
    /// Ask a leaf to close, gracefully if it still has send-path bytes
    /// queued: rather than tearing down the transport immediately (losing
    /// whatever the application message queue or native libsrt sender
    /// buffer hadn't yet been acknowledged), mark it draining so it keeps
    /// getting visited — and therefore keeps sending — until either it
    /// flushes to zero or `drain_timeout` elapses (checked in
    /// `visit_one_ready_leaf` and `sweep_draining_leaves`). Mirrors
    /// `RtmpShardBackend::begin_graceful_close` exactly. A leaf with
    /// nothing queued closes immediately; there is nothing to wait for.
    pub(super) fn begin_graceful_close(
        &mut self,
        output_id: &OutputId,
        reason: crate::media::egress::backend::CloseReason,
    ) {
        self.pending_connects.remove(output_id);
        let Some(key) = self.output_sockets.get(output_id).copied() else {
            return;
        };
        let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut) else {
            return;
        };
        if !leaf.pressure().is_backpressured() {
            self.output_sockets.remove(output_id);
            self.remove_leaf(key, reason);
            return;
        }
        leaf.draining_since = Some(Instant::now());
        leaf.draining_reason = Some(reason);
    }

    /// Close every draining leaf (see `begin_graceful_close`) that has
    /// either fully flushed or been draining longer than `drain_timeout`.
    /// Mirrors `RtmpShardBackend::sweep_draining_leaves` exactly — the
    /// flush case here is a backstop, not the primary path; a leaf getting
    /// real write readiness closes opportunistically the moment it
    /// flushes, inside `visit_one_ready_leaf`, without waiting for this
    /// once-a-second sweep.
    #[cfg(test)]
    pub(super) fn sweep_draining_leaves(&mut self, now: Instant) {
        let expired: Vec<OutputId> = self
            .output_sockets
            .iter()
            .filter_map(|(output_id, key)| {
                let leaf = self.leaves.get_mut(key.0)?.as_mut()?;
                let draining_since = leaf.draining_since?;
                let flushed = !leaf.pressure().is_backpressured();
                let expired = now.saturating_duration_since(draining_since) >= self.drain_timeout;
                (flushed || expired).then(|| output_id.clone())
            })
            .collect();
        for output_id in expired {
            let Some(key) = self.output_sockets.remove(&output_id) else {
                continue;
            };
            let reason = self
                .leaves
                .get(key.0)
                .and_then(Option::as_ref)
                .and_then(|leaf| leaf.draining_reason)
                .unwrap_or(crate::media::egress::backend::CloseReason::Removed);
            self.remove_leaf(key, reason);
        }
    }

    /// Minimum interval between stall sweeps: the native bstats probe is one
    /// FFI call per leaf, so the sweep runs at human-observable cadence, not
    /// per media tick.
    const STALL_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

    /// Probe a bounded rotating set of leaves for no-progress and drain
    /// deadlines. New leaves enter `stall_candidates` once; each live key is
    /// returned to the tail, so this never walks the whole population in one
    /// media tick.
    pub(super) fn sweep_stalled_leaves(&mut self, now: Instant) {
        if self
            .last_stall_sweep
            .is_some_and(|last| now.saturating_duration_since(last) < Self::STALL_SWEEP_INTERVAL)
        {
            return;
        }
        self.last_stall_sweep = Some(now);
        let head_sequence = crate::media::egress::feed::EgressFeed::head_sequence(&self.feed);
        let drain_timeout = self.drain_timeout;
        for _ in 0..256 {
            let Some(key) = self.stall_candidates.pop_front() else {
                break;
            };
            let Some((output_id, close)) =
                self.leaves
                    .get_mut(key.0)
                    .and_then(Option::as_mut)
                    .map(|leaf| {
                        let lag_units = if leaf.common().cursor_primed {
                            head_sequence.saturating_sub(leaf.common().cursor.next_sequence)
                        } else {
                            0
                        };
                        let quality = leaf.sample_quality(now);
                        let drops = quality.as_ref().and_then(|q| q.packets_sent_drop);
                        let reason = match leaf.observe_stall(now, drops, lag_units) {
                            LeafStallClass::Idle => None,
                            LeafStallClass::Backpressured => Some("backpressured"),
                            LeafStallClass::Stalled => Some("stalled"),
                        };
                        leaf.common()
                            .progress_sink
                            .record_backpressure_state(lag_units, reason);
                        if let Some(quality) = quality {
                            leaf.common().progress_sink.record_quality(quality);
                        }
                        let draining = leaf.draining_since.is_some_and(|since| {
                            !leaf.pressure().is_backpressured()
                                || now.saturating_duration_since(since) >= drain_timeout
                        });
                        (
                            leaf.common().output_id.clone(),
                            draining || matches!(reason, Some("stalled")),
                        )
                    })
            else {
                continue;
            };
            if !close {
                self.stall_candidates.push_back(key);
                continue;
            }
            let Some(key) = self.output_sockets.remove(&output_id) else {
                continue;
            };
            if let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::take) {
                let mut leaf = leaf;
                leaf.common.progress_sink.mark_terminated_unexpectedly();
                leaf.engine.close(
                    &mut leaf.transport,
                    crate::media::egress::backend::CloseReason::NoProgress,
                );
            }
        }
    }

    /// Move leaves parked on `Feed`/`FeedOrIo` into the ready queue when the
    /// feed publishes more media. The queue is populated at visit time, so a
    /// feed wake does not scan every output on the shard.
    ///
    /// `SrtEgressEngine::advance` only ever reports `WaitCondition::Feed`
    /// on an empty feed and `Io(Interest::WRITE)` everywhere else (a
    /// pending message blocked on socket writability), so — unlike RTMP,
    /// which has real handshake/negotiation `Io`-only states to keep
    /// excluded — every SRT leaf this loop could possibly touch is exactly
    /// the case a feed wake should re-drive. Before this method existed,
    /// `FeedWake` was a no-op here (SRT relied entirely on being always
    /// `WRITE`-registered plus the shard's forced `ScheduleReady{1}` making
    /// the next real `poll_leaves()` rediscover it); this gives SRT the
    /// same direct-enqueue latency improvement RTMP gets, at the cost of
    /// doing real work per feed wake instead of none.
    pub(super) fn enqueue_feed_waiting_leaves(&mut self) {
        while let Some(key) = self.feed_waiting.pop_front() {
            let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut) else {
                continue;
            };
            leaf.common.schedule.feed_wake_queued = false;
            if !leaf.common.schedule.wants_feed_wake || leaf.common.schedule.enqueued {
                continue;
            }
            leaf.common.schedule.enqueued = true;
            self.ready.push_back(SrtReadyLeaf {
                key,
                generation: leaf.common.generation,
                writable: false,
            });
        }
    }
}
