//! Graceful-close/drain and stall-sweep methods for [`RtmpShardBackend`],
//! split out of `rtmp_shard.rs` to stay under the source-audit line cap.
//! Mirrors `SrtShardBackend`'s identical mechanism
//! (`src/media/egress/backends/srt_drain.rs`) exactly.

use super::*;

impl<P, R, S> RtmpShardBackend<P, R, S>
where
    P: RtmpReadinessPoller,
    R: RtmpResolveCompletionSource,
    S: RtmpPublishStartupSource,
{
    /// Ask a leaf to close, gracefully if it still has application bytes
    /// queued: rather than tearing down the transport immediately (losing
    /// whatever `pending_application_bytes` hadn't reached the wire yet),
    /// mark it draining so it keeps getting visited — and therefore keeps
    /// writing — until either it flushes to zero or `drain_timeout` elapses
    /// (checked in `visit_one_ready_leaf` and `sweep_draining_leaves`).
    /// A leaf with nothing queued closes immediately; there is nothing to
    /// wait for.
    pub(super) fn begin_graceful_close(&mut self, output_id: &OutputId, reason: CloseReason) {
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
        if leaf.common.pending_application_bytes == 0 {
            self.output_sockets.remove(output_id);
            self.remove_leaf_socket(socket_ref, reason);
            return;
        }
        leaf.draining_since = Some(Instant::now());
        leaf.draining_reason = Some(reason);
    }

    /// Close every draining leaf (see `begin_graceful_close`) that has
    /// either fully flushed or been draining longer than `drain_timeout`.
    /// The flush case here is a backstop, not the primary path — a leaf
    /// getting real write readiness closes opportunistically the moment it
    /// flushes, inside `visit_one_ready_leaf`, without waiting for this
    /// once-a-second sweep. This is what actually bounds a leaf that stops
    /// getting write readiness at all (a peer that stops reading): nothing
    /// else will ever notice it again.
    pub(super) fn sweep_draining_leaves(&mut self, now: Instant) {
        let expired: Vec<OutputId> = self
            .output_sockets
            .iter()
            .filter_map(|(output_id, socket_ref)| {
                let leaf = self.leaves.get(socket_ref.key.0)?.as_ref()?;
                let draining_since = leaf.draining_since?;
                let flushed = leaf.common.pending_application_bytes == 0;
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
                .unwrap_or(CloseReason::Removed);
            self.remove_leaf_socket(socket_ref, reason);
        }
    }

    /// Close every leaf whose pending application bytes have made no
    /// byte/protocol progress within the no-progress deadline. Mirrors
    /// `SrtShardBackend::sweep_stalled_leaves` exactly (same
    /// `classify_stall` policy, same closed-leaves-retry-via-reconnect
    /// contract) — this is what makes `LeafCommon::pending_application_bytes`
    /// (wired up in `visit_one_ready_leaf`, `docs/egress-implementation.md`
    /// Phase 5 status) actually mean something: previously nothing read it,
    /// so a leaf that fell arbitrarily far behind a slow or wedged peer was
    /// never closed for that reason alone.
    pub(super) fn sweep_stalled_leaves(&mut self, now: Instant) {
        if self
            .last_stall_sweep
            .is_some_and(|last| now.saturating_duration_since(last) < Self::STALL_SWEEP_INTERVAL)
        {
            return;
        }
        self.last_stall_sweep = Some(now);
        self.sweep_draining_leaves(now);

        let head_sequence = self.feed.head_sequence();
        for leaf in self.leaves.iter_mut().flatten() {
            let lag_units = head_sequence.saturating_sub(leaf.common.cursor.next_sequence);
            let reason = match leaf.observe_stall(now) {
                LeafStallClass::Idle => None,
                LeafStallClass::Backpressured => Some("backpressured"),
                LeafStallClass::Stalled => Some("stalled"),
            };
            leaf.common
                .progress_sink
                .record_backpressure_state(lag_units, reason);
            if let Some(quality) = leaf.sample_quality(now) {
                leaf.common.progress_sink.record_quality(quality);
            }
        }

        let stalled: Vec<OutputId> = self
            .output_sockets
            .iter()
            .filter_map(|(output_id, socket_ref)| {
                let leaf = self.leaves.get(socket_ref.key.0)?.as_ref()?;
                (leaf.observe_stall(now) == LeafStallClass::Stalled).then(|| output_id.clone())
            })
            .collect();

        for output_id in stalled {
            let Some(socket_ref) = self.output_sockets.remove(&output_id) else {
                continue;
            };
            let _ = self.poller.remove(socket_ref.fd);
            if let Some(leaf) = self.leaves.get_mut(socket_ref.key.0).and_then(Option::take) {
                let mut leaf = leaf;
                leaf.common.progress_sink.mark_terminated_unexpectedly();
                leaf.engine
                    .close(&mut leaf.transport, CloseReason::NoProgress);
            }
        }
    }
}
