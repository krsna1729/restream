//! Owner event handling for [`SrtShardBackend`]: exact, generation-safe
//! attribution of pool, caller and TX-failure events to leaves.
//!
//! Every lookup is by `(family, LogicalCallerId)` or `(family, PoolRequestId)`
//! -- never a walk of the leaf population -- except family-wide fault
//! retirement, which is a cold path taken once per Owner fault.

use super::owner_set::SrtOwnerEvent;
use super::*;
use srt_transport::compio::TxFailureClass;

impl SrtShardBackend {
    pub(super) fn handle_owner_event(&mut self, event: SrtOwnerEvent) {
        match event {
            SrtOwnerEvent::Admitted {
                family,
                request_id,
                caller,
            } => self.on_admitted(family, request_id, caller),
            SrtOwnerEvent::RequestFailed {
                family,
                request_id,
                reason,
            } => {
                let Some(request) = self.queued_requests.remove(&(family, request_id)) else {
                    return;
                };
                self.fail_pending(&request.output_id, request.generation, &reason);
            }
            SrtOwnerEvent::Expired { family, caller } => {
                // The pool already retired the caller; close the leaf whose
                // handshake ran out of time and nothing else.
                self.close_unexpected(
                    SrtCaller { family, id: caller },
                    false,
                    "handshake attempt deadline",
                );
            }
            SrtOwnerEvent::Connected { family, caller } => {
                if let Some(key) = self.callers.get(&SrtCaller { family, id: caller }).copied() {
                    self.enqueue_ready_candidate(key);
                } else {
                    self.stale_events = self.stale_events.saturating_add(1);
                }
            }
            SrtOwnerEvent::PeerGroupCollision { family, fault } => {
                // The bond spans remote receiving groups, so it is not an SRT
                // bond at all: fail the WHOLE output (never leave the healthy
                // leg running or degrade to one leg). Attributed by exact
                // logical caller; session-local, never an Owner fault.
                let caller = SrtCaller {
                    family,
                    id: fault.id,
                };
                if let Some(key) = self.callers.get(&caller).copied()
                    && let Some(leaf) = self.leaves.get(key.0).and_then(Option::as_ref)
                {
                    tracing::warn!(
                        output_id = %leaf.common.output_id,
                        family = ?family,
                        caller = ?fault.id,
                        member_id = fault.collision.member_id,
                        peer = %fault.peer,
                        expected_peer_group_id = fault.collision.expected_peer_group_id,
                        actual_peer_group_id = fault.collision.actual_peer_group_id,
                        "srt bonded output legs reach different receiving groups; failing the output"
                    );
                }
                self.close_unexpected(caller, true, "bonded legs reach different receiving groups");
            }
            SrtOwnerEvent::Disconnected { family, caller } => {
                self.close_unexpected(SrtCaller { family, id: caller }, true, "peer disconnected");
            }
            SrtOwnerEvent::TxFailure {
                family,
                attribution,
                class,
            } => {
                // Peer-local and transient failures are attributed to one
                // caller (and leg) and never fault the Owner or a sibling.
                if class == TxFailureClass::OwnerStructural {
                    return; // fault handling owns this via `newly_faulted`
                }
                let Some(id) = attribution.caller_id() else {
                    return;
                };
                if let Some(key) = self.callers.get(&SrtCaller { family, id }).copied()
                    && let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut)
                {
                    leaf.tx_failures = leaf.tx_failures.saturating_add(1);
                }
            }
            SrtOwnerEvent::OutputFailure {
                family,
                attribution,
            } => {
                let Some(id) = attribution.caller_id() else {
                    return;
                };
                // Leg 0 is a direct caller: its only session is quarantined
                // and will never send again. A bonded leg (member id >= 1)
                // leaves the group's other legs healthy, so the logical
                // output stays.
                if attribution.leg() == 0 {
                    self.close_unexpected(
                        SrtCaller { family, id },
                        true,
                        "protocol output failure",
                    );
                }
            }
        }
    }

    /// A queued connect was admitted. Attach it only if its output and
    /// generation are still the ones that asked; otherwise it is stale (the
    /// output was removed or replaced) and the new caller is retired at once
    /// so it cannot hold a pool permit or attach to a replacement.
    fn on_admitted(
        &mut self,
        family: AddressFamily,
        request_id: PoolRequestId,
        caller: srt_transport::advanced::caller::LogicalCallerId,
    ) {
        let identity = SrtCaller { family, id: caller };
        let Some(request) = self.queued_requests.remove(&(family, request_id)) else {
            // An immediate admission's event; its leaf was made synchronously.
            return;
        };
        let current = self
            .pending_connects
            .get(&request.output_id)
            .is_some_and(|pending| {
                pending.common.generation == request.generation
                    && pending.stage == PendingStage::Queued { family, request_id }
            });
        if !current {
            self.owners.remove_now(&identity);
            self.stale_events = self.stale_events.saturating_add(1);
            return;
        }
        if let Some(pending) = self.pending_connects.remove(&request.output_id) {
            self.install_leaf(pending.common, identity);
        }
    }

    /// Close the leaf owning `caller` as an unexpected termination. `disconnect`
    /// asks the Owner to retire the caller too (false when the pool already
    /// did).
    fn close_unexpected(&mut self, caller: SrtCaller, owner_still_holds: bool, why: &str) {
        let Some(key) = self.callers.get(&caller).copied() else {
            self.stale_events = self.stale_events.saturating_add(1);
            return;
        };
        let Some(leaf) = self.leaves.get(key.0).and_then(Option::as_ref) else {
            return;
        };
        leaf.common.progress_sink.mark_terminated_unexpectedly();
        let output_id = leaf.common.output_id.clone();
        tracing::warn!(
            output_id = %output_id,
            family = ?caller.family,
            caller = ?caller.id,
            reason = why,
            "srt egress leaf closed unexpectedly"
        );
        self.output_sockets.remove(&output_id);
        self.remove_leaf(key, crate::media::egress::backend::CloseReason::PeerClosed);
        if !owner_still_holds {
            // The pool removed the caller; make sure no closing entry lingers.
            self.owners.remove_now(&caller);
        }
    }

    /// An Owner latched a structural fault: nothing on that family can be
    /// trusted. Retire every leaf and pending request on it; the sibling
    /// family Owner keeps running.
    pub(super) fn fail_family(&mut self, family: AddressFamily) {
        tracing::error!(?family, fault = ?self.owners.fault(family), "srt egress Owner faulted");
        let doomed: Vec<LeafKey> = self
            .callers
            .iter()
            .filter(|(caller, _)| caller.family == family)
            .map(|(_, key)| *key)
            .collect();
        for key in doomed {
            if let Some(leaf) = self.leaves.get(key.0).and_then(Option::as_ref) {
                leaf.common.progress_sink.mark_terminated_unexpectedly();
                let output_id = leaf.common.output_id.clone();
                self.output_sockets.remove(&output_id);
            }
            self.remove_leaf(key, crate::media::egress::backend::CloseReason::PeerClosed);
        }
        let queued: Vec<((AddressFamily, PoolRequestId), OutputId, u64)> = self
            .queued_requests
            .iter()
            .filter(|((queued_family, _), _)| *queued_family == family)
            .map(|(key, request)| (*key, request.output_id.clone(), request.generation))
            .collect();
        for (key, output_id, generation) in queued {
            self.queued_requests.remove(&key);
            self.fail_pending(&output_id, generation, "SRT Owner faulted");
        }
    }
}
