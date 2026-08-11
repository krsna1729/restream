//! Graceful-close/drain methods for [`SrtShardBackend`], split out of
//! `srt.rs` to stay under the source-audit line cap. Mirrors
//! `RtmpShardBackend`'s identical mechanism
//! (`src/media/egress/backends/rtmp_shard.rs`) exactly — see the doc
//! comments on `begin_graceful_close`/`sweep_draining_leaves` there for the
//! full rationale.

use super::*;

impl<P, C, K, R> SrtShardBackend<P, C, K, R>
where
    P: SrtReadinessPoller,
    C: SrtSocketConfigurator,
    K: SrtSocketConnector,
    R: SrtResolveCompletionSource,
{
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
        let Some(socket_ref) = self.output_sockets.get(output_id).copied() else {
            return;
        };
        let Some(leaf) = self
            .leaves
            .get_mut(socket_ref.key.0)
            .and_then(Option::as_mut)
        else {
            return;
        };
        if !leaf.pressure().is_backpressured() {
            self.output_sockets.remove(output_id);
            self.remove_leaf_socket(socket_ref, reason);
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
    pub(super) fn sweep_draining_leaves(&mut self, now: Instant) {
        let expired: Vec<OutputId> = self
            .output_sockets
            .iter()
            .filter_map(|(output_id, socket_ref)| {
                let leaf = self.leaves.get_mut(socket_ref.key.0)?.as_mut()?;
                let draining_since = leaf.draining_since?;
                let flushed = !leaf.pressure().is_backpressured();
                let expired = now.saturating_duration_since(draining_since) >= self.drain_timeout;
                (flushed || expired).then(|| output_id.clone())
            })
            .collect();
        for output_id in expired {
            let Some(socket_ref) = self.output_sockets.remove(&output_id) else {
                continue;
            };
            let reason = self
                .leaves
                .get(socket_ref.key.0)
                .and_then(Option::as_ref)
                .and_then(|leaf| leaf.draining_reason)
                .unwrap_or(crate::media::egress::backend::CloseReason::Removed);
            self.remove_leaf_socket(socket_ref, reason);
        }
    }

    /// Minimum interval between stall sweeps: the native bstats probe is one
    /// FFI call per leaf, so the sweep runs at human-observable cadence, not
    /// per media tick.
    const STALL_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

    /// Close every leaf whose combined application and native pending state
    /// has made no progress within the no-progress deadline.  Closed leaves
    /// surface as terminated outputs; the application retry policy owns
    /// reconnection (SRT recovery capability is reconnect-only).
    pub(super) fn sweep_stalled_leaves(&mut self, now: Instant) {
        if self
            .last_stall_sweep
            .is_some_and(|last| now.saturating_duration_since(last) < Self::STALL_SWEEP_INTERVAL)
        {
            return;
        }
        self.last_stall_sweep = Some(now);
        self.sweep_draining_leaves(now);

        let head_sequence = crate::media::egress::feed::EgressFeed::head_sequence(&self.feed);
        for leaf in self.leaves.iter_mut().flatten() {
            // A leaf that has not been visited yet still holds the
            // placeholder cursor, so `head - cursor` would report the whole
            // feed as lag rather than a real measurement. It is not behind:
            // it has not started.
            let lag_units = if leaf.common().cursor_primed {
                head_sequence.saturating_sub(leaf.common().cursor.next_sequence)
            } else {
                0
            };
            // One native stats probe per leaf per sweep; `observe_stall`
            // reuses the sampled drop total on its second call via `None`.
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
        }

        let stalled: Vec<OutputId> = self
            .output_sockets
            .iter()
            .filter_map(|(output_id, socket_ref)| {
                let leaf = self.leaves.get_mut(socket_ref.key.0)?.as_mut()?;
                let lag_units = if leaf.common().cursor_primed {
                    head_sequence.saturating_sub(leaf.common().cursor.next_sequence)
                } else {
                    0
                };
                (leaf.observe_stall(now, None, lag_units) == LeafStallClass::Stalled)
                    .then(|| output_id.clone())
            })
            .collect();

        for output_id in stalled {
            let Some(socket_ref) = self.output_sockets.remove(&output_id) else {
                continue;
            };
            let _ = self.poller.remove(socket_ref.socket);
            if let Some(leaf) = self.leaves.get_mut(socket_ref.key.0).and_then(Option::take) {
                let mut leaf = leaf;
                leaf.common.progress_sink.mark_terminated_unexpectedly();
                leaf.engine.close(
                    &mut leaf.transport,
                    crate::media::egress::backend::CloseReason::NoProgress,
                );
            }
        }
    }

    /// Directly enqueue every connected leaf whose last `WaitCondition`
    /// wants a feed wake (`Feed`/`FeedOrIo`) — set in
    /// `apply_progress_to_common` (`visit.rs`) — without any poller call.
    /// Mirrors `poll_ready()`'s push-with-dedup shape exactly (same
    /// `enqueued` check and set), using `self.ready` directly instead of a
    /// real `srt_epoll_wait()`.
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
        let sockets: Vec<SrtLeafSocket> = self.output_sockets.values().copied().collect();
        for socket_ref in sockets {
            let Some(leaf) = self
                .leaves
                .get_mut(socket_ref.key.0)
                .and_then(Option::as_mut)
            else {
                continue;
            };
            if !leaf.common.schedule.wants_feed_wake || leaf.common.schedule.enqueued {
                continue;
            }
            leaf.common.schedule.enqueued = true;
            self.ready.push_back(SrtReadyLeaf {
                socket: socket_ref.socket,
                key: socket_ref.key,
                generation: leaf.common.generation,
                writable: false,
            });
        }
    }
}
