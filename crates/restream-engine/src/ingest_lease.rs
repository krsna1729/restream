//! Per-attempt ingest leases.
//!
//! Registering an input attempt
//! (`MediaEngine::try_register_pipeline_input_attempt`) returns an
//! [`IngestLease`], the only handle that ends the attempt. An orderly teardown
//! calls [`IngestLease::finish`]. A lease dropped without it (a panic unwinding
//! the task or entity state that held it, a cancelled task, an early return)
//! cancels the attempt at once, which frees its input for the next publisher
//! and stops its media, and queues it on the ingest registry's release queue;
//! the registry's release task records an abnormal end and unregisters it. No
//! path can then leave a stream key locked as a duplicate publisher.
//!
//! The queue is registry-owned storage, not a channel a task owns: an attempt
//! stays queued until a release has ended it, so a release task cancelled with
//! its runtime loses nothing, and the next task started (or the shutdown's
//! final pass) ends the backlog first. The release task ends each attempt in
//! a task of its own, so an attempt whose pipeline selection is stuck delays
//! no other pipeline's release; a claimed attempt is not handed to a second
//! task, and a claim dropped unfinished (its task cancelled) is handed out
//! again.
//!
//! Every release is attempt-checked: a lease ends only its own attempt (its
//! own cancel token, its own session), so a stale lease dropped after a newer
//! attempt registered on the same input leaves the newer one alone.
//!
//! Media state (an Owner's publisher, a ring pusher) holds only the attempt's
//! identity ([`IngestRegistration`]), never the lease.

use std::sync::{Arc, Weak};

use crate::ingest_registry::{AbandonedAttempt, IngestRegistration, IngestRegistry, ReleaseQueue};

/// Admission hands off the release owner and the media registration separately.
/// Holding this value keeps the attempt alive through its lease.
#[must_use = "dropping the admission ends the input attempt"]
pub struct RegisteredIngest {
    pub lease: IngestLease,
    pub registration: IngestRegistration,
}

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

/// One registered input attempt. Not `Clone`: whoever holds it owns the
/// attempt's end.
#[must_use = "dropping the lease ends the input attempt"]
pub struct IngestLease {
    pipeline_id: String,
    input_id: String,
    attempt_id: u64,
    cancel: tokio_util::sync::CancellationToken,
    ingests: Weak<IngestRegistry>,
    releases: Arc<ReleaseQueue>,
    /// `finish` recorded its outcome; a drop then only releases.
    recorded: bool,
    finished: bool,
}

impl IngestLease {
    pub(crate) fn new(
        ingests: &Arc<IngestRegistry>,
        pipeline_id: &str,
        registration: &IngestRegistration,
    ) -> Self {
        Self {
            pipeline_id: pipeline_id.to_string(),
            input_id: registration.input_id.clone(),
            attempt_id: registration.attempt_id,
            cancel: registration.cancel_token.clone(),
            ingests: Arc::downgrade(ingests),
            releases: ingests.releases.clone(),
            recorded: false,
            finished: false,
        }
    }

    pub fn pipeline_id(&self) -> &str {
        &self.pipeline_id
    }

    /// End the attempt in order: record `end` and unregister it, if it is
    /// still the input's current attempt.
    pub async fn finish(mut self, end: IngestEnd<'_>) {
        let Some(ingests) = self.ingests.upgrade() else {
            self.finished = true;
            return;
        };
        if let IngestEnd::Disconnected {
            phase,
            reason,
            had_error,
        } = end
        {
            ingests
                .record_ingest_disconnect_if_current(
                    &self.pipeline_id,
                    &self.input_id,
                    self.attempt_id,
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
        ingests
            .unregister_ingest_if_current(&self.pipeline_id, &self.input_id, self.attempt_id)
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
        self.cancel.cancel();
        self.releases.push(AbandonedAttempt {
            pipeline_id: std::mem::take(&mut self.pipeline_id),
            input_id: std::mem::take(&mut self.input_id),
            attempt_id: self.attempt_id,
            record: !self.recorded,
        });
    }
}

#[cfg(test)]
mod tests;
