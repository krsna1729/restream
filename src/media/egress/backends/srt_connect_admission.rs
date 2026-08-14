//! Connect-concurrency admission control for `SrtShardBackend`, split out
//! of `srt.rs` to stay under the source-audit line cap.
//!
//! `egress-architecture.md`'s "Dead destinations and retries" section has
//! long described the intended design: "Reconnection work is protected by:
//! a process-wide token bucket; a per-shard concurrent connect and
//! handshake limit." That admission control was never implemented for
//! either reconnects or the initial mass-`Add` path -- this module is that
//! missing piece, engine-wide (see `SrtShardBackend::connect_admission`),
//! for both.
//!
//! **Why the permit spans the whole pending-handshake window, not just the
//! `connect()` call**: SRT's `connect()` is non-blocking-initiate --
//! confirmed against `connect_single_srt_egress_socket`
//! (`src/media/srt/egress_connect/single.rs`), which calls
//! `set_nonblocking_connect` before `ops.connect(...)` and returns
//! `Ok(socket)` immediately after. The real handshake completes
//! asynchronously in the background, observed later via epoll `WRITE`
//! readiness. A permit released right after the `connect()` call returns
//! would therefore throttle almost nothing -- it would be held for
//! microseconds regardless of real handshake duration. The permit has to
//! live on the leaf itself (`SrtFabricLeaf::handshake_permit`) until that
//! leaf's first poller visit resolves the handshake one way or another
//! (`SrtShardBackend::visit_one_ready_leaf` clears it unconditionally, a
//! no-op after the first visit).
//!
//! **Why a field on the leaf, not a side-map keyed by `OutputId`**: this
//! backend has at least four distinct leaf-removal paths
//! (`remove_leaf_by_output`, `begin_graceful_close`'s immediate-close
//! branch, `sweep_draining_leaves`, and `sweep_stalled_leaves`'s
//! stalled-leaf removal). A side-map would need a manual release at every
//! one of them, with a real leak risk if one were missed or a new removal
//! path were added later. Storing the permit as a leaf field means every
//! removal path releases it for free via ordinary struct-field drop when
//! the leaf itself is dropped -- proven directly by
//! `removing_an_unvisited_leaf_releases_its_handshake_permit` in
//! `tests/media_tick.rs`.

use super::*;

impl<P, C, K, R> SrtShardBackend<P, C, K, R>
where
    P: SrtReadinessPoller,
    C: SrtSocketConfigurator,
    K: SrtSocketConnector,
    R: SrtResolveCompletionSource,
{
    /// Opts this backend's resolved-connect draining into the shared
    /// `admission` semaphore. Kept as a separate builder step, mirroring
    /// `with_srt_egress_muxer_port_reuse`, so every existing constructor
    /// and test call site is unaffected; only production wiring
    /// (`resolving_srt_shard_backend_with_configurator`) calls this.
    /// `None` leaves connects fully unthrottled -- the pre-existing
    /// behavior for every caller that does not opt in.
    pub(crate) fn with_connect_admission(
        mut self,
        admission: Option<Arc<tokio::sync::Semaphore>>,
    ) -> Self {
        self.connect_admission = admission;
        self
    }

    /// Drains `resolved` into live leaves at the pace `connect_admission`
    /// allows, backlogging whatever does not fit this pass. Returns
    /// whether any completion actually connected (mirrors the
    /// `connected_any` this replaced inside `on_media_tick`).
    ///
    /// Resolved completions this pass could not admit -- and anything
    /// already sitting in `connect_backlog` from a previous tick -- are
    /// retried on the *next* `on_media_tick`, which runs on every shard
    /// loop iteration (sub-millisecond cadence under load; see
    /// `EgressShardCommandEffect::ScheduleReady`'s effect on the loop's
    /// idle-wait path in `shard.rs`), so throttling here bounds
    /// concurrency, not overall connect throughput.
    pub(super) fn drain_connect_backlog(&mut self, resolved: Vec<SrtResolvedConnect>) -> bool {
        self.connect_backlog.extend(resolved);

        let mut connected_any = false;
        while let Some(completion) = self.connect_backlog.pop_front() {
            let permit = match &self.connect_admission {
                Some(admission) => match Arc::clone(admission).try_acquire_owned() {
                    Ok(permit) => Some(permit),
                    Err(_) => {
                        // No admission slot right now. Put this completion
                        // back and stop draining for this tick.
                        self.connect_backlog.push_front(completion);
                        break;
                    }
                },
                None => None,
            };
            let result = self.complete_pending_connect(
                &completion.output_id,
                completion.generation,
                &completion.peer_addrs,
            );
            if let (Ok(key), Some(permit)) = (&result, permit)
                && let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut)
            {
                leaf.handshake_permit = Some(permit);
            }
            // else: either connect setup itself failed synchronously (no
            // leaf was created -- the permit drops here, releasing it
            // immediately) or admission is disabled (`permit` was already
            // `None`).
            connected_any |= result.is_ok();
        }
        connected_any
    }

    #[cfg(test)]
    pub(crate) fn connect_backlog_len(&self) -> usize {
        self.connect_backlog.len()
    }

    /// Whether a live leaf for `output_id` is still holding a
    /// connect-admission permit (i.e. has not been visited yet). Test-only
    /// window into `handshake_permit` for proving the admission mechanism
    /// directly, without driving the full poller/visit pipeline (already
    /// covered elsewhere).
    #[cfg(test)]
    pub(crate) fn leaf_holds_handshake_permit(&self, output_id: &OutputId) -> bool {
        self.output_sockets
            .get(output_id)
            .and_then(|socket_ref| self.leaves.get(socket_ref.key.0))
            .and_then(Option::as_ref)
            .is_some_and(|leaf| leaf.handshake_permit.is_some())
    }

    /// Simulates this leaf's first poller visit resolving its pending
    /// handshake, without driving the full readiness/visit pipeline
    /// (already covered by the other `media_tick`/`visit` tests). Mirrors
    /// exactly the one line `visit_one_ready_leaf` runs before visiting.
    #[cfg(test)]
    pub(crate) fn simulate_first_visit_for_test(&mut self, output_id: &OutputId) {
        if let Some(leaf) = self
            .output_sockets
            .get(output_id)
            .and_then(|socket_ref| self.leaves.get_mut(socket_ref.key.0))
            .and_then(Option::as_mut)
        {
            leaf.handshake_permit = None;
        }
    }
}
