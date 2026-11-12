//! Per-attempt ingest leases.
//!
//! Registering an input attempt
//! ([`MediaEngine::try_register_pipeline_input_attempt`]) returns an
//! [`IngestLease`], the only handle that ends the attempt. An orderly teardown
//! calls [`IngestLease::finish`]. A lease dropped without it (a panic unwinding
//! the task or entity state that held it, a cancelled task, an early return)
//! cancels the attempt at once, which frees its input for the next publisher
//! and stops its media, and queues it on the engine's release queue; the
//! engine's release task records an abnormal end and unregisters it. No path
//! can then leave a stream key locked as a duplicate publisher.
//!
//! The queue is engine-owned storage, not a channel a task owns: an attempt
//! stays queued until a release has ended it, so a release task cancelled with
//! its runtime loses nothing, and the next task started (or the shutdown's
//! final pass) ends the backlog first.
//!
//! Every release is attempt-checked: a lease ends only its own attempt (its
//! own cancel token, its own session), so a stale lease dropped after a newer
//! attempt registered on the same input leaves the newer one alone.
//!
//! Media state (an Owner's publisher, a ring pusher) holds only the attempt's
//! identity ([`IngestRegistration`]), never the lease.

use std::collections::VecDeque;
use std::sync::{Arc, Weak};

use tokio::sync::Notify;

use crate::engine::{IngestRegistration, MediaEngine};

/// The failure phase recorded for an attempt whose lease was dropped without
/// [`IngestLease::finish`].
pub const ABANDONED_PHASE: &str = "abandoned";
const ABANDONED_REASON: &str =
    "the input attempt ended without an orderly finish (its task panicked or was cancelled)";

/// How an attempt ends.
pub enum IngestEnd<'a> {
    /// Record why it ended as the pipeline's recent ingest outcome (when it
    /// is the selected input), then release it.
    Disconnected {
        phase: &'a str,
        reason: String,
        had_error: bool,
    },
    /// Release it without an outcome: it was refused or withdrawn before it
    /// carried media, or its owner already recorded how it ended.
    Released,
}

/// One dropped lease's attempt, for the engine's release task.
#[derive(Clone)]
pub(crate) struct AbandonedAttempt {
    pipeline_id: String,
    registration: IngestRegistration,
    /// Whether to record the abandoned outcome: false once a cancelled
    /// [`IngestLease::finish`] already recorded its own.
    record: bool,
}

/// The engine's queue of dropped leases' attempts. Unbounded so a drop never
/// blocks or fails: it holds at most one entry per registration made, and
/// each entry leaves it only once its release completed.
#[derive(Default)]
pub(crate) struct ReleaseQueue {
    pending: std::sync::Mutex<VecDeque<AbandonedAttempt>>,
    wake: Notify,
}

impl ReleaseQueue {
    fn push(&self, attempt: AbandonedAttempt) {
        restream_sync::lock::lock(&self.pending).push_back(attempt);
        // Stores a permit when no task waits, so a wake is never lost.
        self.wake.notify_one();
    }

    /// The oldest attempt not yet released; it stays queued until
    /// [`Self::complete`].
    fn front(&self) -> Option<AbandonedAttempt> {
        restream_sync::lock::lock(&self.pending).front().cloned()
    }

    /// Removes `attempt` by its id, wherever it is now: a second pass that
    /// released the same entry then removes nothing, never another entry.
    fn complete(&self, attempt: &AbandonedAttempt) {
        let mut pending = restream_sync::lock::lock(&self.pending);
        if let Some(index) = pending
            .iter()
            .position(|queued| queued.registration.attempt_id == attempt.registration.attempt_id)
        {
            pending.remove(index);
        }
    }

    async fn woken(&self) {
        self.wake.notified().await;
    }
}

/// One registered input attempt. Not `Clone`: whoever holds it owns the
/// attempt's end.
#[must_use = "dropping the lease ends the input attempt"]
pub struct IngestLease {
    pipeline_id: String,
    registration: IngestRegistration,
    engine: Weak<MediaEngine>,
    releases: Arc<ReleaseQueue>,
    /// `finish` recorded its outcome; a drop then only releases.
    recorded: bool,
    finished: bool,
}

impl IngestLease {
    pub(crate) fn new(
        engine: &Arc<MediaEngine>,
        releases: Arc<ReleaseQueue>,
        pipeline_id: &str,
        registration: IngestRegistration,
    ) -> Self {
        Self {
            pipeline_id: pipeline_id.to_string(),
            registration,
            engine: Arc::downgrade(engine),
            releases,
            recorded: false,
            finished: false,
        }
    }

    /// The attempt's identity, for metadata, probe and selection calls and
    /// for the media state that runs it.
    pub fn registration(&self) -> &IngestRegistration {
        &self.registration
    }

    pub fn pipeline_id(&self) -> &str {
        &self.pipeline_id
    }

    /// End the attempt in order: record `end` and unregister it, if it is
    /// still the input's current attempt.
    pub async fn finish(mut self, end: IngestEnd<'_>) {
        let Some(engine) = self.engine.upgrade() else {
            self.finished = true;
            return;
        };
        if let IngestEnd::Disconnected {
            phase,
            reason,
            had_error,
        } = end
        {
            engine
                .record_ingest_disconnect_if_current(
                    &self.pipeline_id,
                    &self.registration,
                    Some(phase),
                    Some(reason),
                    had_error,
                )
                .await;
        }
        // Each step is marked only once it completed: a finish dropped
        // midway leaves the rest to `Drop`, which neither records twice nor
        // skips the release (and a repeated, attempt-checked release is
        // harmless).
        self.recorded = true;
        engine
            .unregister_ingest_if_current(&self.pipeline_id, &self.registration)
            .await;
        self.finished = true;
    }
}

impl Drop for IngestLease {
    /// Safe on any thread and during unwinding: a token cancel, one short
    /// push under a poison-tolerant lock and a wake; no await, no panic.
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // At once, before the release task runs: the duplicate check admits
        // the next publisher on this input, and the attempt's media stops.
        // The token is this attempt's own; a newer attempt has its own.
        self.registration.cancel_token.cancel();
        self.releases.push(AbandonedAttempt {
            pipeline_id: std::mem::take(&mut self.pipeline_id),
            registration: self.registration.clone(),
            record: !self.recorded,
        });
    }
}

/// Ends every queued attempt, oldest first. Each leaves the queue only once
/// ended, so a pass cancelled midway leaves it for the next one, and two
/// passes over one queue release every entry.
pub(crate) async fn release_backlog(engine: &MediaEngine, queue: &ReleaseQueue) {
    while let Some(attempt) = queue.front() {
        if attempt.record {
            engine
                .record_ingest_disconnect_if_current(
                    &attempt.pipeline_id,
                    &attempt.registration,
                    Some(ABANDONED_PHASE),
                    Some(ABANDONED_REASON.to_string()),
                    true,
                )
                .await;
        }
        engine
            .unregister_ingest_if_current(&attempt.pipeline_id, &attempt.registration)
            .await;
        queue.complete(&attempt);
    }
}

/// The engine's release task: empties the backlog first, then each wake's
/// arrivals, until the engine is dropped.
pub(crate) async fn drain_releases(engine: Weak<MediaEngine>, queue: Arc<ReleaseQueue>) {
    loop {
        let Some(strong) = engine.upgrade() else {
            return;
        };
        release_backlog(&strong, &queue).await;
        drop(strong);
        queue.woken().await;
    }
}

#[cfg(test)]
mod tests;
