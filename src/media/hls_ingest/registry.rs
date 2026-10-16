//! Stream-key authorization, per-key upload slots, the global buffer budget
//! and the session registry. Runs on Tokio CONTROL; per object it does one
//! lookup and one bounded channel send, never media work.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tracing::{error, info, warn};

use super::ordering::SessionLimits;
use super::session::{self, HlsObject, InitCache, SessionContext, SessionEnd};
use super::{
    BodyError, HlsIngestLimits, KeyCharge, ObjectBody, ObjectKind, ObjectOutcome, SegmentShare,
};
use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::executor::{MediaService, MediaServiceClass};
use crate::media::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessMode,
};
use crate::media::security::{IngestSecurityService, RateLimitScope};
use crate::secret_display::redact_secret;

/// Objects queued for one session; encoders upload sequentially, and the
/// per-key upload slots bound what can wait here anyway.
const OBJECT_QUEUE: usize = 2;
/// Initial body capacity; larger bodies grow with the bytes counted.
const INITIAL_BODY_CAPACITY: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizeError {
    /// Unknown key, disabled input, or a banned client.
    Forbidden,
    LookupFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// Another publisher holds the input (RTMP, SRT, file).
    PublisherActive,
    /// The session ended under the object twice, or the MEDIA executor is
    /// unavailable.
    Unavailable,
}

struct SessionHandle {
    attempt_id: u64,
    objects: mpsc::Sender<HlsObject>,
}

/// Per stream key (input id): its upload slots and the lock that keeps two
/// first objects from both registering an attempt.
struct KeyState {
    uploads: Arc<Semaphore>,
    /// The key's share of the global budget, claimed by segments.
    share: Arc<Semaphore>,
    /// Serializes share claims; a chunked body holds it until complete.
    share_lock: Arc<tokio::sync::Mutex<()>>,
    /// Bytes the key has outstanding, for the fair-share log.
    outstanding: Arc<AtomicUsize>,
    /// The session's pacing lead in ms (target duration, or the idle floor
    /// before a playlist); sizes the whole-body deadline.
    lead_ms: Arc<AtomicU64>,
    start: tokio::sync::Mutex<()>,
}

pub struct HlsIngest {
    engine: Arc<MediaEngine>,
    access: Arc<dyn PipelineAccessAuthenticator>,
    security: Arc<IngestSecurityService>,
    limits: HlsIngestLimits,
    budget: Arc<Semaphore>,
    /// Keyed by input id; held for lookups only, never across an await.
    sessions: std::sync::Mutex<HashMap<String, SessionHandle>>,
    keys: std::sync::Mutex<HashMap<String, Arc<KeyState>>>,
    /// The init each stream key applied last: an encoder uploads its
    /// `EXT-X-MAP` init once per run, so it outlives the session.
    inits: Arc<std::sync::Mutex<InitCache>>,
}

impl HlsIngest {
    pub fn new(
        engine: Arc<MediaEngine>,
        access: Arc<dyn PipelineAccessAuthenticator>,
        security: Arc<IngestSecurityService>,
        limits: HlsIngestLimits,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine,
            access,
            security,
            limits,
            budget: Arc::new(Semaphore::new(limits.total_buffer_bytes)),
            sessions: std::sync::Mutex::new(HashMap::new()),
            keys: std::sync::Mutex::new(HashMap::new()),
            inits: Arc::new(std::sync::Mutex::new(InitCache::new(
                limits.init_cache_bytes,
            ))),
        })
    }

    pub fn limits(&self) -> &HlsIngestLimits {
        &self.limits
    }

    /// Resolve the stream key exactly as RTMP and SRT publish do.
    pub async fn authorize(
        &self,
        stream_key: &str,
        client_ip: &str,
    ) -> Result<AuthenticatedPipeline, AuthorizeError> {
        if self
            .security
            .is_ip_banned_for(RateLimitScope::HlsPublish, client_ip)
            .is_some()
        {
            return Err(AuthorizeError::Forbidden);
        }
        match self
            .access
            .authenticate(PipelineAccessMode::HlsPublish, stream_key, client_ip)
            .await
        {
            Ok(pipeline) => Ok(pipeline),
            Err(PipelineAccessError::InvalidStreamKey) => {
                warn!(stream_key = %redact_secret(stream_key), "hls ingest stream key rejected");
                Err(AuthorizeError::Forbidden)
            }
            Err(PipelineAccessError::LookupFailed(error)) => {
                error!(%error, "hls ingest stream key lookup failed");
                Err(AuthorizeError::LookupFailed(error))
            }
        }
    }

    fn key_state(&self, input_id: &str) -> Arc<KeyState> {
        crate::sync::lock(&self.keys)
            .entry(input_id.to_string())
            .or_insert_with(|| {
                Arc::new(KeyState {
                    uploads: Arc::new(Semaphore::new(self.limits.uploads_per_key)),
                    share: Arc::new(Semaphore::new(self.limits.key_buffer_bytes)),
                    share_lock: Arc::new(tokio::sync::Mutex::new(())),
                    outstanding: Arc::new(AtomicUsize::new(0)),
                    lead_ms: Arc::new(AtomicU64::new(0)),
                    start: tokio::sync::Mutex::new(()),
                })
            })
            .clone()
    }

    /// One of the key's upload slots, held from body read to reply. `None`
    /// only if the slot semaphore was closed, which this module never does.
    pub async fn upload_slot(&self, input_id: &str) -> Option<OwnedSemaphorePermit> {
        let uploads = self.key_state(input_id).uploads.clone();
        uploads.acquire_owned().await.ok()
    }

    /// How long one request body may take in total: four target durations
    /// (four idle floors before the first playlist, and never less). A
    /// segment fits when the uplink carries a quarter of the stream bitrate,
    /// which a real-time encoder's uplink always does; a trickle does not.
    pub fn body_deadline(&self, input_id: &str) -> Duration {
        let lead = Duration::from_millis(self.key_state(input_id).lead_ms.load(Ordering::Relaxed));
        lead.max(self.limits.idle_floor).saturating_mul(4)
    }

    /// An empty body for `kind`, counted against `input_id`. A segment's
    /// declared length is claimed from the key's share and the global budget
    /// at once (waiting for the share); it preallocates at most 1 MiB, so
    /// memory tracks the bytes claimed.
    pub async fn object_body(
        &self,
        kind: ObjectKind,
        content_length: Option<u64>,
        input_id: &str,
    ) -> Result<ObjectBody, BodyError> {
        let max_bytes = kind.max_bytes(&self.limits);
        let declared = content_length.map_or(Ok(0), |length| {
            usize::try_from(length)
                .ok()
                .filter(|length| *length <= max_bytes)
                .ok_or(BodyError::TooLarge)
        })?;
        let key = self.key_state(input_id);
        let segment = (kind != ObjectKind::Playlist).then(|| SegmentShare {
            share: key.share.clone(),
            lock: key.share_lock.clone(),
            growing: None,
        });
        let mut body = ObjectBody {
            budget: self.budget.clone(),
            segment,
            max_bytes,
            bytes: bytes::BytesMut::with_capacity(declared.min(INITIAL_BODY_CAPACITY)),
            claimed: 0,
            permit: None,
            share_permit: None,
            input_id: input_id.to_string(),
            fair_share: self.limits.total_buffer_bytes / 4,
            charge: KeyCharge {
                outstanding: key.outstanding.clone(),
                bytes: 0,
            },
        };
        body.claim_declared(declared).await?;
        Ok(body)
    }

    /// Hand one fully read object to the key's session, starting it if
    /// needed. A session that ended under the object gets one restart with
    /// the same object; a refused start (another publisher) is final.
    pub async fn dispatch(
        self: &Arc<Self>,
        pipeline: &AuthenticatedPipeline,
        stream_key: &str,
        client_addr: &str,
        kind: ObjectKind,
        path: String,
        body: ObjectBody,
    ) -> Result<ObjectOutcome, DispatchError> {
        let (reply, outcome) = oneshot::channel();
        let mut object = HlsObject {
            path,
            kind,
            payload: body.into_payload(),
            reply,
        };
        for _attempt in 0..2 {
            let (attempt_id, objects) = self.session_for(pipeline, stream_key, client_addr).await?;
            match objects.send(object).await {
                Ok(()) => return outcome.await.map_err(|_| DispatchError::Unavailable),
                Err(mpsc::error::SendError(returned)) => {
                    object = returned;
                    self.forget(&pipeline.input_id, attempt_id);
                }
            }
        }
        Err(DispatchError::Unavailable)
    }

    fn current_session(&self, input_id: &str) -> Option<(u64, mpsc::Sender<HlsObject>)> {
        crate::sync::lock(&self.sessions)
            .get(input_id)
            .map(|handle| (handle.attempt_id, handle.objects.clone()))
    }

    async fn session_for(
        self: &Arc<Self>,
        pipeline: &AuthenticatedPipeline,
        stream_key: &str,
        client_addr: &str,
    ) -> Result<(u64, mpsc::Sender<HlsObject>), DispatchError> {
        if let Some(session) = self.current_session(&pipeline.input_id) {
            return Ok(session);
        }
        // Only objects of this key wait here; other keys start in parallel.
        let key = self.key_state(&pipeline.input_id);
        let _starting = key.start.lock().await;
        if let Some(session) = self.current_session(&pipeline.input_id) {
            return Ok(session);
        }
        let engine = &self.engine;
        let registration = engine
            .try_register_pipeline_input_attempt(
                &pipeline.id,
                &pipeline.input_id,
                stream_key,
                "hls",
                pipeline.selected,
            )
            .await
            .ok_or(DispatchError::PublisherActive)?;
        let ring = engine.get_or_create_pipeline(&pipeline.id).await;
        engine
            .update_ingest_session_meta(
                &pipeline.id,
                &registration,
                None,
                None,
                Some(client_addr.to_string()),
            )
            .await;
        let (objects, receiver) = mpsc::channel(OBJECT_QUEUE);
        let attempt_id = registration.attempt_id;
        crate::sync::lock(&self.sessions).insert(
            pipeline.input_id.clone(),
            SessionHandle {
                attempt_id,
                objects: objects.clone(),
            },
        );
        info!(pipeline = %pipeline.id, input = %pipeline.input_id, "[hls] Ingest registered");
        let context = SessionContext {
            engine: engine.clone(),
            pipeline_id: pipeline.id.clone(),
            registration,
            ring,
            lead_ms: key.lead_ms.clone(),
            limits: SessionLimits {
                hold_segments: self.limits.hold_segments,
                hold_bytes: self.limits.hold_bytes,
                idle_floor: self.limits.idle_floor,
            },
            max_init_bytes: self.limits.max_init_bytes,
            inits: self.inits.clone(),
        };
        tokio::spawn(Arc::clone(self).supervise(context, receiver));
        Ok((attempt_id, objects))
    }

    fn forget(&self, input_id: &str, attempt_id: u64) {
        let mut sessions = crate::sync::lock(&self.sessions);
        if sessions
            .get(input_id)
            .is_some_and(|handle| handle.attempt_id == attempt_id)
        {
            sessions.remove(input_id);
        }
        let idle = !sessions.contains_key(input_id);
        drop(sessions);
        let mut keys = crate::sync::lock(&self.keys);
        // Keep the key's state while anything still uses it: an upload slot
        // in flight, bytes counted against it, or a start in progress.
        if idle
            && keys.get(input_id).is_some_and(|key| {
                Arc::strong_count(key) == 1
                    && Arc::strong_count(&key.uploads) == 1
                    && Arc::strong_count(&key.share) == 1
                    && Arc::strong_count(&key.share_lock) == 1
                    && Arc::strong_count(&key.outstanding) == 1
                    && Arc::strong_count(&key.lead_ms) == 1
            })
        {
            keys.remove(input_id);
        }
    }

    /// Runs the session on MEDIA and, whatever ends it (ENDLIST, idle,
    /// cancellation, a failure or a panic, which the executor contains),
    /// reports the disconnect and releases the input attempt.
    async fn supervise(
        self: Arc<Self>,
        context: SessionContext,
        objects: mpsc::Receiver<HlsObject>,
    ) {
        let pipeline_id = context.pipeline_id.clone();
        let registration: IngestRegistration = context.registration.clone();
        let service = MediaService {
            class: MediaServiceClass::HlsIngest,
            pipeline_id: pipeline_id.clone(),
            stage: format!("hls-ingest:{}", registration.input_id),
        };
        let result = crate::media::executor::run_with_class(
            service,
            registration.cancel_token.clone(),
            session::run(context, objects),
        )
        .await;
        let (phase, reason, had_error) = match &result {
            Ok((SessionEnd::Ended, _)) => ("eof", "HLS playlist ended".to_string(), false),
            Ok((SessionEnd::Idle, _)) => ("idle", "HLS publisher went idle".to_string(), false),
            Ok((SessionEnd::Cancelled, _)) => ("cancel", "HLS ingest cancelled".to_string(), false),
            Ok((SessionEnd::Failed(error), _)) => ("media-executor", error.clone(), true),
            Err(error) => ("media-executor", error.clone(), true),
        };
        if let Ok((_, counters)) = &result {
            info!(
                pipeline = %pipeline_id,
                phase,
                skipped = counters.skipped,
                duplicates = counters.duplicates,
                evicted = counters.evicted,
                epochs = counters.epochs,
                "[hls] Ingest session ended"
            );
        }
        self.engine
            .record_ingest_disconnect_if_current(
                &pipeline_id,
                &registration,
                Some(phase),
                Some(reason),
                had_error,
            )
            .await;
        self.engine
            .unregister_ingest_if_current(&pipeline_id, &registration)
            .await;
        self.forget(&registration.input_id, registration.attempt_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::hls_ingest::test_support::{
        INPUT_ID, PIPELINE_ID, STREAM_KEY, authenticated, emptied_mdat, fmp4_hevc_init,
        fmp4_playlist, fmp4_video_objects, test_ingest,
    };

    /// A session that ended between lookup and send gets exactly one
    /// restart, with the object already read.
    #[tokio::test]
    async fn an_object_for_a_session_that_just_ended_restarts_it_once() {
        let (engine, ingest) = test_ingest(HlsIngestLimits::default());
        let (stale, receiver) = mpsc::channel(OBJECT_QUEUE);
        drop(receiver);
        crate::sync::lock(&ingest.sessions).insert(
            INPUT_ID.to_string(),
            SessionHandle {
                attempt_id: u64::MAX,
                objects: stale,
            },
        );
        let mut body = ingest
            .object_body(ObjectKind::TsSegment, None, INPUT_ID)
            .await
            .unwrap();
        body.push(b"segment").await.unwrap();

        let outcome = ingest
            .dispatch(
                &authenticated(),
                STREAM_KEY,
                "127.0.0.1:9",
                ObjectKind::TsSegment,
                "index0.ts".to_string(),
                body,
            )
            .await;

        assert_eq!(outcome, Ok(ObjectOutcome::Held));
        let attempt = crate::sync::lock(&ingest.sessions)[INPUT_ID].attempt_id;
        assert_ne!(attempt, u64::MAX, "the stale handle was replaced");
        assert!(engine.input_has_session(PIPELINE_ID, INPUT_ID).await);
        engine.cancel_pipeline_input(INPUT_ID).await;
    }

    async fn send(
        ingest: &Arc<HlsIngest>,
        kind: ObjectKind,
        path: &str,
        bytes: &[u8],
    ) -> ObjectOutcome {
        send_to(ingest, &authenticated(), kind, path, bytes).await
    }

    async fn send_to(
        ingest: &Arc<HlsIngest>,
        pipeline: &AuthenticatedPipeline,
        kind: ObjectKind,
        path: &str,
        bytes: &[u8],
    ) -> ObjectOutcome {
        let mut body = ingest
            .object_body(kind, None, &pipeline.input_id)
            .await
            .unwrap();
        body.push(bytes).await.unwrap();
        ingest
            .dispatch(
                pipeline,
                STREAM_KEY,
                "127.0.0.1:9",
                kind,
                path.to_string(),
                body,
            )
            .await
            .unwrap()
    }

    fn fixture() -> Vec<u8> {
        std::fs::read(crate::test_fixtures::canonical_h264_ts_fixture().unwrap()).unwrap()
    }

    /// Wait until the pipeline's current ring satisfies `predicate`, or fail
    /// the test after `secs`. The engine can swap the ring after a probe, so
    /// the buffer is re-read every pass.
    async fn wait_for_ring(
        engine: &MediaEngine,
        secs: u64,
        what: &str,
        mut predicate: impl FnMut(&crate::media::ring_buffer::RingBuffer) -> bool,
    ) {
        tokio::time::timeout(std::time::Duration::from_secs(secs), async {
            loop {
                let ring = engine.get_or_create_pipeline(PIPELINE_ID).await;
                if predicate(&ring) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    /// How many packets the pipeline's current ring holds.
    async fn published(engine: &MediaEngine) -> usize {
        engine
            .get_or_create_pipeline(PIPELINE_ID)
            .await
            .get_write_idx()
    }

    async fn wait_until_unregistered(engine: &MediaEngine) {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while engine.input_has_session(PIPELINE_ID, INPUT_ID).await {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("HLS session unregistered");
    }

    /// The init an encoder uploads once keeps a later session publishing:
    /// after an idle end, a new session resumes with the next segment and
    /// the playlist, and the media still reaches the ring.
    #[tokio::test]
    async fn media_keeps_flowing_after_an_idle_end_without_reuploading_the_init() {
        let (engine, ingest) = test_ingest(HlsIngestLimits {
            idle_floor: std::time::Duration::from_millis(150),
            ..HlsIngestLimits::default()
        });
        let (init, segments) = fmp4_video_objects(2, 4);

        assert_eq!(
            send(&ingest, ObjectKind::Fmp4, "init.mp4", &init).await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send(&ingest, ObjectKind::Fmp4, "index0.m4s", &segments[0]).await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send(
                &ingest,
                ObjectKind::Playlist,
                "index.m3u8",
                fmp4_playlist(0, &["index0.m4s"], false).as_bytes()
            )
            .await,
            ObjectOutcome::Stored
        );
        wait_for_ring(&engine, 10, "the first segment in the ring", |ring| {
            ring.get_write_idx() > 0
        })
        .await;
        wait_until_unregistered(&engine).await;

        // The encoder's next upload: no init this time (FFmpeg uploads it
        // once per run), and the stream continues.
        let before = published(&engine).await;
        assert_eq!(
            send(&ingest, ObjectKind::Fmp4, "index1.m4s", &segments[1]).await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send(
                &ingest,
                ObjectKind::Playlist,
                "index.m3u8",
                fmp4_playlist(1, &["index1.m4s"], true).as_bytes()
            )
            .await,
            ObjectOutcome::Stored
        );
        wait_for_ring(&engine, 10, "the resumed segment in the ring", |ring| {
            ring.get_write_idx() > before
        })
        .await;
        wait_until_unregistered(&engine).await;
        let phase = engine
            .recent_ingest_outcome(PIPELINE_ID)
            .await
            .and_then(|outcome| outcome.failure_phase);
        assert_eq!(phase.as_deref(), Some("eof"));
    }

    /// A refused init must void the map: FFmpeg uploads `init.mp4` on every
    /// run and never reads the refusal, so leaving the previous run's init
    /// in place would demux the new run's segments with it.
    #[tokio::test]
    async fn a_refused_init_voids_the_map_and_its_segments_do_not_publish() {
        let (engine, ingest) = test_ingest(HlsIngestLimits {
            idle_floor: std::time::Duration::from_millis(300),
            ..HlsIngestLimits::default()
        });
        let (init, segments) = fmp4_video_objects(2, 4);

        // The first run is H.264 and publishes.
        assert_eq!(
            send(&ingest, ObjectKind::Fmp4, "init.mp4", &init).await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send(&ingest, ObjectKind::Fmp4, "index0.m4s", &segments[0]).await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send(
                &ingest,
                ObjectKind::Playlist,
                "index.m3u8",
                fmp4_playlist(0, &["index0.m4s"], false).as_bytes()
            )
            .await,
            ObjectOutcome::Stored
        );
        wait_for_ring(&engine, 10, "the first run in the ring", |ring| {
            ring.get_write_idx() > 0
        })
        .await;

        // The operator switches the key to HEVC: the same `init.mp4` path
        // now carries `hvc1`, which is refused.
        assert_eq!(
            send(&ingest, ObjectKind::Fmp4, "init.mp4", &fmp4_hevc_init()).await,
            ObjectOutcome::Unsupported("HEVC fMP4 ingest is not implemented".to_string())
        );

        // The next run's media must not be demuxed with the old init: it
        // waits for an init that cannot come, and nowhere is it published.
        let before = published(&engine).await;
        assert_eq!(
            send(&ingest, ObjectKind::Fmp4, "index1.m4s", &segments[1]).await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send(
                &ingest,
                ObjectKind::Playlist,
                "index.m3u8",
                fmp4_playlist(1, &["index1.m4s"], true).as_bytes()
            )
            .await,
            ObjectOutcome::Stored
        );
        // Two target durations: long enough that a stale init would have
        // published the segment already.
        tokio::time::sleep(std::time::Duration::from_millis(2_200)).await;
        assert_eq!(
            published(&engine).await,
            before,
            "a refused init must not leave the previous run's init in place"
        );
        engine.cancel_pipeline_input(INPUT_ID).await;
    }

    /// A segment whose bytes cannot be demuxed is dropped: the session stays
    /// up and the segment after it publishes.
    #[tokio::test]
    async fn a_segment_that_fails_demux_is_skipped_and_the_next_one_publishes() {
        let (engine, ingest) = test_ingest(HlsIngestLimits {
            idle_floor: std::time::Duration::from_millis(300),
            ..HlsIngestLimits::default()
        });
        let (init, segments) = fmp4_video_objects(2, 4);

        assert_eq!(
            send(&ingest, ObjectKind::Fmp4, "init.mp4", &init).await,
            ObjectOutcome::Held
        );
        let broken = emptied_mdat(&segments[0]);
        assert_eq!(
            send(&ingest, ObjectKind::Fmp4, "index0.m4s", &broken).await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send(&ingest, ObjectKind::Fmp4, "index1.m4s", &segments[1]).await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send(
                &ingest,
                ObjectKind::Playlist,
                "index.m3u8",
                fmp4_playlist(0, &["index0.m4s", "index1.m4s"], true).as_bytes()
            )
            .await,
            ObjectOutcome::Stored
        );
        wait_for_ring(&engine, 10, "the segment after the broken one", |ring| {
            ring.get_write_idx() > 0
        })
        .await;
        wait_until_unregistered(&engine).await;
        let phase = engine
            .recent_ingest_outcome(PIPELINE_ID)
            .await
            .and_then(|outcome| outcome.failure_phase);
        assert_eq!(phase.as_deref(), Some("eof"), "the session kept going");
    }

    /// A key at its buffer share is throttled, not refused: its next body
    /// waits until the segment being published returns its bytes, and a
    /// second key publishes meanwhile. The wait is exercised on the body
    /// here; over a real socket it becomes the encoder's write blocking on
    /// a full TCP window, which no test covers yet.
    #[tokio::test]
    async fn a_key_at_its_share_is_throttled_while_another_key_publishes() {
        let (engine, ingest) = test_ingest(HlsIngestLimits {
            key_buffer_bytes: 8_000_000,
            ..HlsIngestLimits::default()
        });
        let segment = fixture();
        let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:8\n#EXTINF:8.0,\nindex0.ts\n";
        assert_eq!(
            send(&ingest, ObjectKind::TsSegment, "index0.ts", &segment).await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send(
                &ingest,
                ObjectKind::Playlist,
                "index.m3u8",
                playlist.as_bytes()
            )
            .await,
            ObjectOutcome::Stored
        );

        // index0 (6.7 MB) publishes over 8 s and holds most of the 8 MB share.
        let started = std::time::Instant::now();
        let mut next = ingest
            .object_body(ObjectKind::TsSegment, None, INPUT_ID)
            .await
            .unwrap();
        let throttled = tokio::spawn(async move {
            let pushed = next.push(&fixture()).await;
            (pushed.is_ok(), started.elapsed())
        });

        let other = AuthenticatedPipeline {
            id: "pipe-hls-b".to_string(),
            input_id: "input-hls-b".to_string(),
            selected: true,
        };
        let other_ring = engine.get_or_create_pipeline(&other.id).await;
        let other_playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1.0,\nindex0.ts\n";
        let other_segment = &segment[..188 * 2_000];
        assert_eq!(
            send_to(
                &ingest,
                &other,
                ObjectKind::TsSegment,
                "index0.ts",
                other_segment
            )
            .await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send_to(
                &ingest,
                &other,
                ObjectKind::Playlist,
                "index.m3u8",
                other_playlist.as_bytes()
            )
            .await,
            ObjectOutcome::Stored
        );
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while other_ring.get_write_idx() == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the other key publishes");
        assert!(
            !throttled.is_finished(),
            "the capped key's body waits for its share"
        );

        let (pushed, waited) = throttled.await.unwrap();
        assert!(pushed, "the capped key's body is read, never refused");
        assert!(
            waited >= std::time::Duration::from_secs(5),
            "the body waited only {waited:?} for the share"
        );
        engine.cancel_pipeline_input(INPUT_ID).await;
        engine.cancel_pipeline_input(&other.input_id).await;
    }

    /// Share claims of one key are serialized: a chunked body holds the
    /// key's claim lock until it is complete, so a second chunked body
    /// waits without a fragment and the first can still grow. Without the
    /// lock the second would hold 4 MiB and the first could not claim its
    /// next step.
    #[tokio::test]
    async fn two_chunked_bodies_of_one_key_never_both_hold_a_share_fragment() {
        const MIB: usize = 1024 * 1024;
        let (_engine, ingest) = test_ingest(HlsIngestLimits {
            key_buffer_bytes: 10 * MIB,
            ..HlsIngestLimits::default()
        });
        let mut first = ingest
            .object_body(ObjectKind::TsSegment, None, INPUT_ID)
            .await
            .unwrap();
        first.push(&[0x47; 1024]).await.unwrap();

        let mut second = ingest
            .object_body(ObjectKind::TsSegment, None, INPUT_ID)
            .await
            .unwrap();
        let waiting = tokio::spawn(async move {
            let pushed = second.push(&[0x47; 1024]).await.is_ok();
            (pushed, second)
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !waiting.is_finished(),
            "the second body waits for the first"
        );

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            first.push(&vec![0x47; 5 * MIB]),
        )
        .await
        .expect("the first body grows past its first claim")
        .unwrap();
        drop(first);
        let (pushed, _second) = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("the second body proceeds once the first is gone")
            .unwrap();
        assert!(pushed);
    }

    /// Released media waits to be published only up to two target
    /// durations: past that the session stops taking objects, so a
    /// faster-than-real-time encoder's next reply waits, though the bytes
    /// (6.7 MB) are far below the byte ceiling.
    #[tokio::test]
    async fn the_outbox_holds_two_target_durations_of_media() {
        let (engine, ingest) = test_ingest(HlsIngestLimits::default());
        let fixture = fixture();
        let slice = fixture.len() / 188 / 6 * 188;
        let mut playlist = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:1\n");
        for index in 0..6 {
            let end = if index == 5 {
                fixture.len()
            } else {
                (index + 1) * slice
            };
            let path = format!("index{index}.ts");
            let bytes = &fixture[index * slice..end];
            assert_eq!(
                send(&ingest, ObjectKind::TsSegment, &path, bytes).await,
                ObjectOutcome::Held
            );
            playlist.push_str(&format!("#EXTINF:1.0,\n{path}\n"));
        }
        // All six (6 s listed) are released at once.
        assert_eq!(
            send(
                &ingest,
                ObjectKind::Playlist,
                "index.m3u8",
                playlist.as_bytes()
            )
            .await,
            ObjectOutcome::Stored
        );

        let asked = std::time::Instant::now();
        let outcome = send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            playlist.as_bytes(),
        )
        .await;
        let waited = asked.elapsed();
        assert_eq!(outcome, ObjectOutcome::Stored);
        // Taken once under 2 s remain: five slices of 1.33 s real media.
        assert!(
            waited >= std::time::Duration::from_secs(4),
            "the next object was taken after {waited:?}"
        );
        engine.cancel_pipeline_input(INPUT_ID).await;
    }

    /// A whole segment released at once reaches the ring at media pace, not
    /// in one burst: the wall time from its first to its last published
    /// packet covers most of its media duration.
    #[tokio::test]
    async fn a_released_segment_reaches_the_ring_at_media_pace() {
        let (engine, ingest) = test_ingest(HlsIngestLimits::default());
        let ring = engine.get_or_create_pipeline(PIPELINE_ID).await;
        let start = ring.get_write_idx();
        let fixture = crate::test_fixtures::canonical_h264_ts_fixture().unwrap();
        let segment = std::fs::read(fixture).unwrap();
        let playlist =
            "#EXTM3U\n#EXT-X-TARGETDURATION:8\n#EXTINF:8.0,\nindex0.ts\n#EXT-X-ENDLIST\n";

        assert_eq!(
            send(&ingest, ObjectKind::TsSegment, "index0.ts", &segment).await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send(
                &ingest,
                ObjectKind::Playlist,
                "index.m3u8",
                playlist.as_bytes()
            )
            .await,
            ObjectOutcome::Stored
        );

        // The receive path never waits on pacing: the next upload is
        // answered while the first segment is still being published.
        let asked = std::time::Instant::now();
        assert_eq!(
            send(
                &ingest,
                ObjectKind::TsSegment,
                "index1.ts",
                &segment[..18_800]
            )
            .await,
            ObjectOutcome::Held
        );
        let replied = std::time::Instant::now();

        let mut first = None;
        let mut last = None;
        let mut seen = start;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while engine.input_has_session(PIPELINE_ID, INPUT_ID).await {
            assert!(
                std::time::Instant::now() < deadline,
                "the session never ended"
            );
            let written = ring.get_write_idx();
            if written > seen {
                let now = std::time::Instant::now();
                first.get_or_insert(now);
                last = Some(now);
                seen = written;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let dts: Vec<i64> = (start..ring.get_write_idx())
            .filter_map(|index| ring.read_at(index))
            .map(|packet| packet.dts)
            .collect();
        let media_ms = dts.iter().max().unwrap() - dts.iter().min().unwrap();
        let wall_ms = (last.unwrap() - first.unwrap()).as_millis();
        assert!(media_ms > 7_000, "the segment holds {media_ms} ms of media");
        assert!(
            replied - asked < std::time::Duration::from_millis(500) && last.unwrap() > replied,
            "the second upload was answered after {:?}, publication ended {:?} after it",
            replied - asked,
            last.unwrap().saturating_duration_since(replied)
        );
        assert!(
            wall_ms * 10 >= u128::try_from(media_ms).unwrap() * 8,
            "{media_ms} ms of media reached the ring in {wall_ms} ms"
        );
        assert!(
            wall_ms <= u128::try_from(media_ms).unwrap() + 2_000,
            "pacing must not lag: {media_ms} ms of media took {wall_ms} ms"
        );
    }
}
