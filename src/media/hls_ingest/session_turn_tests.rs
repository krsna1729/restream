//! The session turn's unit bound: one poll applies at most one object.
//!
//! The mode of failure this defends is a poll whose work is set by another
//! runtime: the object path has nothing to await, so without the yield at the
//! end of it a single poll applies every object the HTTP handlers manage to
//! queue while it runs. On a shared MEDIA executor that is exactly the shape
//! the placement rule forbids — one poll's work following the sender's pace
//! instead of one unit.

use std::pin::pin;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use super::*;
use crate::media::engine::MediaEngine;
use crate::media::hls_ingest::ordering::{Payload, SessionLimits};

/// A live media playlist with no `EXT-X-ENDLIST`, so the session stays up
/// waiting for its next object instead of finishing.
const PLAYLIST: &[u8] = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n\
#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2.000,\nsegment-0.ts\n";

async fn context() -> SessionContext {
    let engine = Arc::new(MediaEngine::new());
    let registration = engine
        .try_register_pipeline_input_attempt("pipe", "input", "key", "hls", true)
        .await
        .expect("the test's only attempt registers");
    let ring = engine.get_or_create_pipeline("pipe").await;
    SessionContext {
        engine,
        pipeline_id: "pipe".to_string(),
        registration,
        ring,
        lead_ms: Arc::new(AtomicU64::new(0)),
        limits: SessionLimits {
            hold_segments: 8,
            hold_bytes: 1 << 20,
            idle_floor: Duration::from_secs(10),
        },
        max_init_bytes: 1 << 20,
        init: Arc::new(Mutex::new(Default::default())),
    }
}

/// The replies the session has answered, in total: a oneshot receiver yields
/// its value once, so each answered object counts once.
#[derive(Default)]
struct Answers {
    replies: Vec<oneshot::Receiver<ObjectOutcome>>,
    answered: usize,
}

impl Answers {
    fn count(&mut self) -> usize {
        for reply in &mut self.replies {
            if reply.try_recv().is_ok() {
                self.answered += 1;
            }
        }
        self.answered
    }
}

/// Queue `count` playlists; the returned sender keeps the session's queue open.
async fn queue(count: usize) -> (mpsc::Sender<HlsObject>, mpsc::Receiver<HlsObject>, Answers) {
    let (sender, receiver) = mpsc::channel(count.max(1));
    let mut answers = Answers::default();
    for index in 0..count {
        let (reply, outcome) = oneshot::channel();
        answers.replies.push(outcome);
        sender
            .send(HlsObject {
                path: format!("index-{index}.m3u8"),
                content: ObjectContent::Playlist(crate::media::hls_ingest::playlist::parse(
                    PLAYLIST,
                    &format!("index-{index}.m3u8"),
                )),
                payload: Payload::new(PLAYLIST.into(), None, None, None),
                reply,
            })
            .await
            .expect("the queue has room for the test's objects");
    }
    (sender, receiver, answers)
}

/// One poll of the session future, and whether it is still running.
fn poll_once<F: Future>(future: &mut std::pin::Pin<&mut F>) -> Poll<F::Output> {
    future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
}

#[tokio::test]
async fn one_poll_applies_at_most_one_object() {
    let (_sender, receiver, mut answers) = queue(3).await;
    let mut session = pin!(run(context().await, receiver));

    assert!(
        poll_once(&mut session).is_pending(),
        "the session keeps waiting for its next object"
    );
    assert_eq!(
        answers.count(),
        1,
        "one poll applies one object, not every object the queue already holds"
    );

    // The bound is per poll, not per session: each next poll takes one more
    // object, so a full queue still drains at one object per turn.
    assert!(poll_once(&mut session).is_pending());
    assert_eq!(answers.count(), 2, "the next poll takes the next object");
    assert!(poll_once(&mut session).is_pending());
    assert_eq!(answers.count(), 3, "and so on");
    assert_eq!(
        poll_once(&mut session),
        Poll::Pending,
        "with the queue drained the session waits for more"
    );
    assert_eq!(answers.count(), 3);
}

#[tokio::test]
async fn a_poll_that_receives_nothing_applies_nothing() {
    let (_sender, receiver, mut answers) = queue(0).await;
    let mut session = pin!(run(context().await, receiver));

    assert!(poll_once(&mut session).is_pending());
    assert_eq!(answers.count(), 0);
}

/// The session turn applies the playlist that dispatch parsed and never parses
/// the bytes itself: parsing is linear in the playlist (about 29 ms for a
/// 1 MiB one) and the turn runs on the shared MEDIA worker. The bytes here are
/// not a playlist at all, so a turn that parsed them would answer `Malformed`.
#[tokio::test]
async fn a_turn_applies_the_parsed_playlist_without_parsing_the_bytes() {
    let (sender, receiver) = mpsc::channel(1);
    let (reply, mut outcome) = oneshot::channel();
    sender
        .send(HlsObject {
            path: "index.m3u8".to_string(),
            content: ObjectContent::Playlist(crate::media::hls_ingest::playlist::parse(
                PLAYLIST,
                "index.m3u8",
            )),
            payload: Payload::new(b"not a playlist".as_slice().into(), None, None, None),
            reply,
        })
        .await
        .expect("the queue has room");
    let mut session = pin!(run(context().await, receiver));

    assert!(poll_once(&mut session).is_pending());
    assert_eq!(
        outcome.try_recv(),
        Ok(ObjectOutcome::Stored),
        "the turn applied the parsed playlist"
    );
}
