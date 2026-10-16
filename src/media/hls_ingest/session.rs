//! One stream key's HLS push session on the MEDIA executor: applies objects
//! to the ordering state, replies at once, then publishes what it releases.

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::ordering::{
    Counters, Payload, PlaylistOutcome, Release, SegmentOutcome, Session, SessionLimits,
};
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
    let mut session = Session::new(context.limits);
    let end = match TsPublishTarget::open(
        context.engine,
        context.pipeline_id,
        context.registration,
        context.ring,
    )
    .await
    {
        Ok(target) => drive(&mut session, &target, &cancel, &mut objects).await,
        Err(error) => SessionEnd::Failed(error),
    };
    // End the attempt before the queue closes: a handler whose send then
    // fails must find the attempt cancelled, so its one restart registers
    // a new attempt instead of being refused as a second publisher.
    cancel.cancel();
    drop(objects);
    (end, session.counters())
}

async fn drive(
    session: &mut Session,
    target: &TsPublishTarget,
    cancel: &CancellationToken,
    objects: &mut mpsc::Receiver<HlsObject>,
) -> SessionEnd {
    let mut publisher = TsPublisher::new(TsTimestamps::default());
    let mut releases: Vec<Release> = Vec::new();
    let mut last_object = Instant::now();

    loop {
        let idle_at = last_object + session.idle_timeout();
        let wake_at = session
            .next_deadline()
            .map_or(idle_at, |deadline| deadline.min(idle_at));
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return SessionEnd::Cancelled,
            object = objects.recv() => {
                // The registry keeps a sender while the session is
                // registered, so a closed queue means it was forgotten.
                let Some(object) = object else { return SessionEnd::Cancelled };
                last_object = Instant::now();
                let outcome = apply(session, object.kind, object.path, object.payload, last_object);
                let _ = object.reply.send(outcome);
            }
            _ = tokio::time::sleep_until(wake_at.into()) => {
                if Instant::now() >= idle_at {
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
            if release.discontinuity {
                publisher.discontinuity(target);
            }
            for chunk in release.payload.bytes.chunks(PUBLISH_CHUNK_BYTES) {
                if cancel.is_cancelled() {
                    return SessionEnd::Cancelled;
                }
                publisher.publish(target, chunk).await;
            }
        }
        if session.finished() {
            publisher.flush(target);
            return SessionEnd::Ended;
        }
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
