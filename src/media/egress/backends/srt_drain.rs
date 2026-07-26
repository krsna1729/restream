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
}
