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
        self.shard_saturated = shard_saturated(self.sweep_visited, self.sweep_pressured);
        self.sweep_visited = 0;
        self.sweep_pressured = 0;
        let shard_saturated = self.shard_saturated;
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
            let Some((output_id, close, pressured)) = self
                .leaves
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
                    let quality = stats
                        .as_ref()
                        .and_then(|stats| leaf.sample_quality(stats, now, feed_published_bytes));
                    let drops = quality.as_ref().and_then(|q| q.packets_sent_drop);
                    let reason = match leaf.observe_stall(now, drops, lag_units, backlog) {
                        LeafStallClass::Idle => None,
                        LeafStallClass::Backpressured => Some("backpressured"),
                        // A saturated shard stalls many leaves at once;
                        // closing them only reconnects them into the same
                        // saturated Owner (a reconnect storm). Keep them
                        // and let SRT's too-late drop shed the backlog.
                        LeafStallClass::Stalled if shard_saturated => Some("shard_saturated"),
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
                        reason.is_some(),
                    )
                })
            else {
                continue;
            };
            self.sweep_visited += 1;
            if pressured {
                self.sweep_pressured += 1;
            }
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

/// A shard is saturated when at least `SATURATED_MIN_PRESSURED_LEAVES` of the
/// leaves one full sweep visited, and at least a quarter of them, were
/// backpressured or stalled: the Owner, not one destination, is behind. The
/// minimum is on pressured leaves, not on population, so one stuck
/// destination on a small shard (1 of 4) is still recycled.
const SATURATED_SHARE_DENOMINATOR: usize = 4;
const SATURATED_MIN_PRESSURED_LEAVES: usize = 4;

pub(super) fn shard_saturated(visited: usize, pressured: usize) -> bool {
    pressured >= SATURATED_MIN_PRESSURED_LEAVES
        && pressured * SATURATED_SHARE_DENOMINATOR >= visited
}

#[cfg(test)]
mod saturation_tests {
    use super::shard_saturated;

    #[test]
    fn a_lone_stuck_destination_does_not_saturate_the_shard() {
        assert!(!shard_saturated(4, 1), "1 of 4 is one bad destination");
        assert!(!shard_saturated(8, 2));
        assert!(!shard_saturated(100, 1));
        assert!(!shard_saturated(100, 24));
    }

    #[test]
    fn a_quarter_of_the_shard_and_at_least_four_outputs_behind_is_saturation() {
        assert!(shard_saturated(16, 4));
        assert!(shard_saturated(100, 25));
        assert!(shard_saturated(200, 200));
    }

    #[test]
    fn fewer_than_four_pressured_outputs_are_never_saturation() {
        assert!(
            !shard_saturated(3, 3),
            "small shards still recycle stalled outputs"
        );
        assert!(!shard_saturated(4, 3));
        assert!(!shard_saturated(0, 0));
    }
}
