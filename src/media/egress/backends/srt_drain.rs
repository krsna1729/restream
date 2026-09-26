//! Graceful-close, drain and stall handling for [`SrtShardBackend`], split out
//! of `srt.rs` to stay under the source-audit line cap. Mirrors
//! `RtmpShardBackend`'s identical mechanism
//! (`src/media/egress/backends/rtmp_shard.rs`).

use super::*;
use crate::media::egress::policy::LeafStallClass;
use crate::media::srt::egress_stats::send_backlog;

impl SrtShardBackend {
    /// Ask a leaf to close, gracefully if it still has send-path bytes
    /// queued: rather than tearing the caller down immediately (losing
    /// whatever the application message or the protocol sender buffer had not
    /// yet delivered), mark it draining so it keeps being visited -- and
    /// keeps sending -- until it flushes or `drain_timeout` elapses. A leaf
    /// with nothing queued closes immediately.
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
        let backlog = self
            .owners
            .stats(&leaf.caller)
            .and_then(|stats| send_backlog(&stats));
        if !leaf.pressure(backlog).is_backpressured() {
            self.output_sockets.remove(output_id);
            self.remove_leaf(key, reason);
            return;
        }
        leaf.draining_since = Some(Instant::now());
        leaf.draining_reason = Some(reason);
        // A draining leaf must be visited to flush.
        if !leaf.common.schedule.enqueued {
            self.enqueue_ready_candidate(key);
        }
    }

    /// Minimum interval between stall sweeps: each probe reads a leaf's public
    /// logical-caller statistics, so the sweep runs at human-observable
    /// cadence, not per media tick.
    const STALL_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

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
        let feed_published_bytes = self.feed.published_bytes();
        let drain_timeout = self.drain_timeout;
        // Visit each queued leaf at most once per sweep: live keys return to
        // the tail, so a fixed count would revisit them at the same `now` and
        // collapse every two-sample rate (send rate, delivery) to a zero window.
        let visits = self.stall_candidates.len().min(256);
        for _ in 0..visits {
            let Some(key) = self.stall_candidates.pop_front() else {
                break;
            };
            let owners = &self.owners;
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
                        let stats = owners.stats(&leaf.caller);
                        let backlog = stats.as_ref().and_then(send_backlog);
                        let quality = stats.as_ref().and_then(|stats| {
                            leaf.sample_quality(stats, now, feed_published_bytes)
                        });
                        let drops = quality.as_ref().and_then(|q| q.packets_sent_drop);
                        let reason = match leaf.observe_stall(now, drops, lag_units, backlog) {
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
                            !leaf.pressure(backlog).is_backpressured()
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
                self.enqueue_stall_candidate(key);
                continue;
            }
            let Some(key) = self.output_sockets.remove(&output_id) else {
                continue;
            };
            // A leaf already draining for a requested close (Remove, drain,
            // shutdown) is expiring its drain window, not failing: only an
            // undrained leaf that stalled is an unexpected termination.
            if let Some(leaf) = self.leaves.get(key.0).and_then(Option::as_ref)
                && leaf.draining_since.is_none()
            {
                tracing::warn!(
                    output_id = %output_id,
                    tx_failures = leaf.tx_failures,
                    blocked = leaf.blocked_queued,
                    lag = head_sequence.saturating_sub(leaf.common().cursor.next_sequence),
                    "srt egress leaf closed unexpectedly: no progress (stalled)"
                );
                leaf.common.progress_sink.mark_terminated_unexpectedly();
            }
            self.remove_leaf(key, crate::media::egress::backend::CloseReason::NoProgress);
        }
    }

    /// Move leaves parked on `Feed` into the ready queue when the feed
    /// publishes more media. The queue is populated at visit time, so a feed
    /// wake does not scan every output on the shard.
    pub(super) fn enqueue_feed_waiting_leaves(&mut self) {
        while let Some(key) = self.feed_waiting.pop_front() {
            let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut) else {
                continue;
            };
            leaf.common.schedule.feed_wake_queued = false;
            if !leaf.common.schedule.wants_feed_wake || leaf.common.schedule.enqueued {
                continue;
            }
            self.enqueue_ready_candidate(key);
        }
    }
}
