//! One stream key's HLS push session on the MEDIA executor: applies objects
//! to the ordering state and replies at once; publishes what it releases,
//! paced to media time, between objects.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::ordering::{
    Counters, Payload, PlaylistOutcome, Release, SegmentOutcome, Session, SessionLimits,
};
use super::pacing::Pacer;
use super::playlist;
use super::{ObjectKind, ObjectOutcome};
use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::ring_buffer::RingBuffer;
use crate::media::ts_publisher::{PUBLISH_CHUNK_BYTES, TsPublishTarget, TsPublisher, TsTimestamps};

/// One uploaded object handed from the HTTP handler to its session.
pub(super) struct HlsObject {
    pub(super) path: String,
    pub(super) kind: ObjectKind,
    pub(super) payload: Payload,
    pub(super) reply: oneshot::Sender<ObjectOutcome>,
}

pub(super) struct SessionContext {
    pub(super) engine: Arc<MediaEngine>,
    pub(super) pipeline_id: String,
    pub(super) registration: IngestRegistration,
    pub(super) ring: Arc<RingBuffer>,
    pub(super) limits: SessionLimits,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SessionEnd {
    /// `EXT-X-ENDLIST` and every listed segment published.
    Ended,
    Idle,
    Cancelled,
    Failed(String),
}

pub(super) async fn run(
    context: SessionContext,
    mut objects: mpsc::Receiver<HlsObject>,
) -> (SessionEnd, Counters) {
    let cancel = context.registration.cancel_token.clone();
    let outbox_limit = context.limits.hold_bytes;
    let mut session = Session::new(context.limits);
    let end = match TsPublishTarget::open(
        context.engine,
        context.pipeline_id,
        context.registration,
        context.ring,
    )
    .await
    {
        Ok(target) => {
            let mut outbox = Outbox::default();
            drive(
                &mut session,
                &target,
                &cancel,
                &mut objects,
                &mut outbox,
                outbox_limit,
            )
            .await
        }
        Err(error) => SessionEnd::Failed(error),
    };
    // End the attempt before the queue closes: a handler whose send then
    // fails must find the attempt cancelled, so its one restart registers
    // a new attempt instead of being refused as a second publisher. Objects
    // already queued are answered, never lost: the encoder retries them.
    cancel.cancel();
    objects.close();
    while let Ok(object) = objects.try_recv() {
        let _ = object.reply.send(ObjectOutcome::Retry);
    }
    (end, session.counters())
}

/// Released segments not yet fully published, and where the first one is.
#[derive(Default)]
struct Outbox {
    releases: VecDeque<Release>,
    offset: usize,
    bytes: usize,
}

impl Outbox {
    fn push(&mut self, release: Release) {
        self.bytes += release.payload.bytes.len();
        self.releases.push_back(release);
    }

    fn is_empty(&self) -> bool {
        self.releases.is_empty()
    }
}

async fn drive(
    session: &mut Session,
    target: &TsPublishTarget,
    cancel: &CancellationToken,
    objects: &mut mpsc::Receiver<HlsObject>,
    outbox: &mut Outbox,
    outbox_limit: usize,
) -> SessionEnd {
    let mut publisher = TsPublisher::new(TsTimestamps::default());
    let mut pacer = Pacer::default();
    let mut releases: Vec<Release> = Vec::new();
    let mut last_activity = Instant::now();

    loop {
        let now = Instant::now();
        let publish_at = (!outbox.is_empty())
            .then(|| now + pacer.delay(now, publisher.media_time_ms(), session.pacing_lead()));
        // Idle counts only once everything released has been published.
        let idle_at = outbox
            .is_empty()
            .then(|| last_activity + session.idle_timeout());
        let wake_at = match (session.next_deadline(), idle_at) {
            (Some(deadline), Some(idle)) => Some(deadline.min(idle)),
            (deadline, idle) => deadline.or(idle),
        };
        // An encoder faster than real time waits for its replies once a
        // hold's worth of media is queued for publication.
        let accepting = outbox.bytes <= outbox_limit;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return SessionEnd::Cancelled,
            object = objects.recv(), if accepting => {
                // The registry keeps a sender while the session is
                // registered, so a closed queue means it was forgotten.
                let Some(object) = object else { return SessionEnd::Cancelled };
                last_activity = Instant::now();
                let outcome = apply(session, object.kind, object.path, object.payload, last_activity);
                let _ = object.reply.send(outcome);
            }
            _ = sleep_until(publish_at) => {
                publish_next_chunk(&mut publisher, &mut pacer, target, outbox).await;
                last_activity = Instant::now();
            }
            _ = sleep_until(wake_at) => {
                if idle_at.is_some_and(|idle| Instant::now() >= idle) {
                    publisher.flush(target);
                    return SessionEnd::Idle;
                }
            }
        }

        session.poll(Instant::now(), &mut releases);
        for release in releases.drain(..) {
            tracing::trace!(
                sequence = release.sequence,
                epoch = release.epoch,
                discontinuity = release.discontinuity,
                "hls ingest segment released"
            );
            outbox.push(release);
        }
        if session.finished() && outbox.is_empty() {
            publisher.flush(target);
            return SessionEnd::Ended;
        }
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// Publish at most one chunk of the outbox's first release.
async fn publish_next_chunk(
    publisher: &mut TsPublisher,
    pacer: &mut Pacer,
    target: &TsPublishTarget,
    outbox: &mut Outbox,
) {
    let Some(release) = outbox.releases.front() else {
        return;
    };
    if outbox.offset == 0 && release.discontinuity {
        publisher.discontinuity(target);
        pacer.reset();
    }
    let bytes = &release.payload.bytes;
    let end = bytes.len().min(outbox.offset + PUBLISH_CHUNK_BYTES);
    publisher.publish(target, &bytes[outbox.offset..end]).await;
    if end < bytes.len() {
        outbox.offset = end;
    } else {
        outbox.offset = 0;
        outbox.bytes -= bytes.len();
        // Dropping the release returns its bytes to the global budget.
        outbox.releases.pop_front();
    }
}

fn apply(
    session: &mut Session,
    kind: ObjectKind,
    path: String,
    payload: Payload,
    now: Instant,
) -> ObjectOutcome {
    match kind {
        ObjectKind::Segment => match session.on_segment(path, payload, now) {
            SegmentOutcome::Held => ObjectOutcome::Held,
            SegmentOutcome::Attached => ObjectOutcome::Stored,
            SegmentOutcome::Duplicate | SegmentOutcome::OtherVariant => ObjectOutcome::Dropped,
        },
        ObjectKind::Playlist => match playlist::parse(&payload.bytes, &path) {
            Ok(parsed) => match session.on_playlist(&path, parsed, now) {
                PlaylistOutcome::Accepted => ObjectOutcome::Stored,
                PlaylistOutcome::OtherVariant => ObjectOutcome::Dropped,
            },
            Err(error) => ObjectOutcome::Malformed(error.to_string()),
        },
    }
}
