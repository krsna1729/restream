//! Graceful-close/drain and stall-sweep methods for [`RtmpShardBackend`],
//! split out of `rtmp_shard.rs` to stay under the source-audit line cap.
//! Mirrors `SrtShardBackend`'s identical mechanism
//! (`src/media/egress/backends/srt_drain.rs`) exactly.

use super::*;

impl<P, S> RtmpShardBackend<P, S>
where
    P: RtmpReadinessPoller,
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
        self.remove_connecting_output(output_id);
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

    pub(super) fn sweep_connecting_leaves(&mut self, now: Instant) {
        let expired: Vec<LeafKey> = self
            .connecting
            .iter()
            .filter_map(|(key, connecting)| (connecting.deadline <= now).then_some(*key))
            .collect();
        for key in expired {
            let Some(connecting) = self.connecting.remove(&key) else {
                continue;
            };
            self.connecting_by_output
                .remove(&connecting.common.output_id);
            let _ = self.poller.remove(connecting.stream.as_raw_fd());
            connecting
                .common
                .progress_sink
                .mark_terminated_unexpectedly();
        }
    }

    /// Close every draining leaf (see `begin_graceful_close`) that has
    /// either fully flushed or been draining longer than `drain_timeout`.
    /// The flush case here is a backstop, not the primary path — a leaf
    /// getting real write readiness closes opportunistically the moment it
    /// flushes, inside `visit_one_ready_leaf`, without waiting for this
    /// once-a-second sweep. This is what actually bounds a leaf that stops
    /// getting write readiness at all (a peer that stops reading): nothing
    /// else will ever notice it again.
    #[cfg(test)]
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
        let head_sequence = self.feed.head_sequence();
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
                        let lag_units = if leaf.common.cursor_primed {
                            head_sequence.saturating_sub(leaf.common.cursor.next_sequence)
                        } else {
                            0
                        };
                        let quality = leaf.sample_quality(now);
                        let reason = match leaf.observe_stall(now) {
                            LeafStallClass::Idle => None,
                            LeafStallClass::Backpressured => Some("backpressured"),
                            LeafStallClass::Stalled => Some("stalled"),
                        };
                        leaf.common
                            .progress_sink
                            .record_backpressure_state(lag_units, reason);
                        if let Some(quality) = quality {
                            leaf.common.progress_sink.record_quality(quality);
                        }
                        let draining = leaf.draining_since.is_some_and(|since| {
                            leaf.common.pending_application_bytes == 0
                                || now.saturating_duration_since(since) >= drain_timeout
                        });
                        let startup_expired =
                            !leaf.engine.is_publish_accepted() && now >= leaf.startup_deadline;
                        if startup_expired {
                            tracing::warn!(
                                output_id = %leaf.common.output_id,
                                handshake_done = leaf.engine.is_handshake_done(),
                                "rtmp fabric leaf did not reach publish acceptance before its startup deadline"
                            );
                        }
                        (
                            leaf.common.output_id.clone(),
                            draining || startup_expired || matches!(reason, Some("stalled")),
                        )
                    })
            else {
                continue;
            };
            if !close {
                self.enqueue_stall_candidate(key);
                continue;
            }
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
