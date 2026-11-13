//! Per-publisher lifecycle work off the shared SRT session loop. Each
//! attached publisher has one task that owns its ingest lease and applies its
//! stream probes in event order. The peer's disconnect drops the task's queue
//! sender, so the task applies every probe queued before the disconnect, then
//! finishes the lease: a peer's probe answers and its finish happen in event
//! order, and nothing follows the finish. What a task awaits (its pipeline's
//! selection, the ingest maps, the ring map) delays neither the loop nor
//! another peer.
//!
//! Every probe gets exactly one answer, in order: a panicking application is
//! contained and answered with no ring (the Owner then releases the held
//! packets into its current ring). Answers reach the loop on a channel the
//! loop reads only while its command slot is free, so the one-pending-command
//! backpressure holds. The channel holds at most one answer per publisher:
//! the Owner keeps at most one probe per publisher in flight.
//!
//! A task is never aborted, so its lease always ends in order; a task that
//! panics outside a probe drops its lease, which releases the attempt as
//! abandoned (`crate::ingest_lease`).

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt;
use restream_mpegts::DemuxProbe;
use restream_sync::RingBuffer;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinSet;
use tracing::error;

use super::{Publisher, PublisherSession, SrtServer};
use crate::srt::ingress_media::ProbeIdentity;
use crate::srt::ingress_owner::IngressPeer;
use crate::srt::ingress_quality::QualityFold;

/// One `Probe` event for a publisher's task.
pub(super) struct ProbeJob {
    pub(super) identity: ProbeIdentity,
    pub(super) probe: Box<DemuxProbe>,
}

/// A probe's answer, for the loop to send as `ProbeApplied`.
pub(super) struct ProbeAnswer {
    pub(super) logical_peer: IngressPeer,
    pub(super) identity: ProbeIdentity,
    pub(super) ring: Option<Arc<RingBuffer>>,
}

/// The publishers' tasks, and the sender their answers go out on.
pub(super) struct PeerWork {
    tasks: JoinSet<()>,
    answers: UnboundedSender<ProbeAnswer>,
}

impl PeerWork {
    /// The tasks' home, and the loop's end of their answer channel (never
    /// closed while this lives).
    pub(super) fn new() -> (Self, UnboundedReceiver<ProbeAnswer>) {
        let (answers, answers_rx) = unbounded_channel();
        let work = Self {
            tasks: JoinSet::new(),
            answers,
        };
        (work, answers_rx)
    }

    /// Start an admitted publisher's task; the loop keeps the returned
    /// handle, and dropping it finishes the publisher after its queued
    /// probes.
    pub(super) fn attach(
        &mut self,
        server: &Arc<SrtServer>,
        logical_peer: IngressPeer,
        session: PublisherSession,
    ) -> Publisher {
        let (jobs_tx, jobs) = unbounded_channel();
        let ingest = session.ingest.clone();
        self.tasks.spawn(run(
            server.clone(),
            logical_peer,
            session,
            jobs,
            self.answers.clone(),
        ));
        Publisher {
            jobs: jobs_tx,
            ingest,
            closing: false,
            quality_fold: QualityFold::default(),
        }
    }

    /// Finish a publisher that never attached (its peer left during
    /// admission), off the loop.
    pub(super) fn finish(&mut self, server: &Arc<SrtServer>, session: PublisherSession) {
        let server = server.clone();
        self.tasks
            .spawn(async move { server.finish_publisher(session).await });
    }

    /// Join the next finished task, reporting a panic. Cancel safe.
    pub(super) async fn reap(&mut self) -> Option<()> {
        if let Err(error) = self.tasks.join_next().await? {
            super::report_failed("publisher", error);
        }
        Some(())
    }

    /// Listener exit, with every publisher handle dropped: wait for each
    /// task to apply its queued probes and finish its lease.
    pub(super) async fn drain(&mut self) {
        while self.reap().await.is_some() {}
    }
}

async fn run(
    server: Arc<SrtServer>,
    logical_peer: IngressPeer,
    session: PublisherSession,
    mut jobs: UnboundedReceiver<ProbeJob>,
    answers: UnboundedSender<ProbeAnswer>,
) {
    while let Some(ProbeJob { identity, probe }) = jobs.recv().await {
        let applied = AssertUnwindSafe(server.apply_probe(&session, identity, *probe))
            .catch_unwind()
            .await;
        let ring = applied.unwrap_or_else(|payload| {
            let message = crate::panic_boundary::record_contained(&*payload);
            error!(panic = message, "SRT probe application panicked");
            None
        });
        // Fails only once the loop has exited, when no answer is wanted.
        let _ = answers.send(ProbeAnswer {
            logical_peer,
            identity,
            ring,
        });
    }
    server.finish_publisher(session).await;
}
