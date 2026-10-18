//! Tokio's half of MoQ ingest against a real engine: admission, the input
//! registration it makes, the probe and the bookkeeping at the end.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::domain::ingest_security::IngestSecurityConfig;
use crate::media::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessError, PipelineAccessFuture, PipelineAccessMode,
};
use crate::media::metadata::{AudioMeta, VideoMeta};

const STREAM_KEY: &str = "moq-key";
const PIPELINE_ID: &str = "pipe-moq";
const INPUT_ID: &str = "input-moq";

/// Accepts [`STREAM_KEY`] for MoQ publish only, and counts its lookups.
#[derive(Default)]
struct OneKey {
    lookups: AtomicUsize,
}

impl PipelineAccessAuthenticator for OneKey {
    fn authenticate<'a>(
        &'a self,
        mode: PipelineAccessMode,
        stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        self.lookups.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            if mode == PipelineAccessMode::MoqPublish && stream_key == STREAM_KEY {
                Ok(AuthenticatedPipeline {
                    id: PIPELINE_ID.to_string(),
                    input_id: INPUT_ID.to_string(),
                    selected: true,
                })
            } else {
                Err(PipelineAccessError::InvalidStreamKey)
            }
        })
    }
}

struct Fixture {
    engine: Arc<MediaEngine>,
    security: Arc<IngestSecurityService>,
    access: Arc<OneKey>,
    server: Arc<MoqIngestServer>,
}

fn fixture() -> Fixture {
    let engine = Arc::new(MediaEngine::new());
    let security = Arc::new(IngestSecurityService::new(IngestSecurityConfig::default()));
    let access = Arc::new(OneKey::default());
    let server = Arc::new(MoqIngestServer::new(
        access.clone(),
        engine.clone(),
        security.clone(),
    ));
    Fixture {
        engine,
        security,
        access,
        server,
    }
}

fn peer() -> SocketAddr {
    "203.0.113.7:50000".parse().unwrap()
}

async fn session_protocol(engine: &MediaEngine) -> Option<(String, Option<String>)> {
    let sessions = engine.ingests.sessions.read().await;
    sessions
        .get(INPUT_ID)
        .map(|ingest| (ingest.protocol.clone(), ingest.metadata().remote_addr))
}

#[test]
fn the_stream_key_is_the_last_broadcast_segment_without_catalog_suffix() {
    assert_eq!(stream_key_from_path("/live"), Some("live"));
    assert_eq!(stream_key_from_path("live/"), Some("live"));
    assert_eq!(stream_key_from_path("//key?token=abc"), Some("key"));
    assert_eq!(stream_key_from_path("tenant/live/key.hang"), Some("key"));
    assert_eq!(stream_key_from_path("tenant/live/key.msf"), Some("key"));
    assert_eq!(stream_key_from_path("key.msf?token=abc"), Some("key"));
    assert_eq!(stream_key_from_path("a/b"), Some("b"));
    assert_eq!(stream_key_from_path(".hang"), None);
    assert_eq!(stream_key_from_path(".msf"), None);
    assert_eq!(stream_key_from_path(""), None);
    assert_eq!(stream_key_from_path("/?x=1"), None);
    let longest = "k".repeat(MAX_STREAM_KEY_BYTES);
    assert_eq!(stream_key_from_path(&longest), Some(longest.as_str()));
    assert_eq!(stream_key_from_path(&format!("{longest}.msf")), Some(longest.as_str()));
    assert_eq!(stream_key_from_path(&format!("{longest}k")), None);
}

#[tokio::test]
async fn an_admitted_publisher_holds_a_moq_session_until_it_finishes() {
    let fixture = fixture();
    let admission = fixture
        .server
        .admit(peer(), STREAM_KEY)
        .await
        .unwrap_or_else(|rejection| panic!("admitted, got {rejection:?}"));
    assert_eq!(admission.session.pipeline_id, PIPELINE_ID);
    assert_eq!(admission.session.registration.input_id, INPUT_ID);
    assert_eq!(
        session_protocol(&fixture.engine).await,
        Some(("moq".to_string(), Some(peer().to_string())))
    );

    // One publisher per input: a second one is refused, the first kept.
    let second = fixture.server.admit(peer(), STREAM_KEY).await;
    assert_eq!(second.err(), Some(Rejection::Duplicate));
    assert!(session_protocol(&fixture.engine).await.is_some());

    fixture
        .server
        .finish(admission.session, "test end".to_string())
        .await;
    assert_eq!(session_protocol(&fixture.engine).await, None);
    let outcome = fixture
        .engine
        .recent_ingest_outcome(PIPELINE_ID)
        .await
        .expect("the disconnect is recorded");
    assert_eq!(outcome.protocol, "moq");
}

#[tokio::test]
async fn an_unknown_key_is_unauthorized_and_registers_nothing() {
    let fixture = fixture();
    let rejected = fixture.server.admit(peer(), "wrong-key").await;
    assert_eq!(rejected.err(), Some(Rejection::Unauthorized));
    assert_eq!(session_protocol(&fixture.engine).await, None);
}

#[tokio::test]
async fn a_banned_client_is_refused_before_any_lookup() {
    let fixture = fixture();
    let ip = peer().ip().to_string();
    let mut banned = false;
    for _ in 0..1000 {
        banned = fixture
            .security
            .record_failure_for(RateLimitScope::MoqPublish, &ip);
        if banned {
            break;
        }
    }
    assert!(banned, "the default policy bans after repeated failures");

    let rejected = fixture.server.admit(peer(), STREAM_KEY).await;
    assert_eq!(rejected.err(), Some(Rejection::Banned));
    assert_eq!(fixture.access.lookups.load(Ordering::Relaxed), 0);
    assert_eq!(session_protocol(&fixture.engine).await, None);
}

#[tokio::test]
async fn the_probe_records_tracks_and_metadata_on_the_session() {
    let fixture = fixture();
    let admission = fixture.server.admit(peer(), STREAM_KEY).await.ok().unwrap();
    let probe = MoqProbe {
        video: Some(VideoMeta {
            codec: "h264".to_string(),
            width: 1280,
            height: 720,
            fps: 30.0,
            ..VideoMeta::default()
        }),
        audio_tracks: vec![AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48_000,
            channels: 2,
            ..AudioMeta::default()
        }],
    };
    fixture.server.apply_probe(&admission.session, probe).await;

    let (metadata, audio_tracks) = fixture
        .engine
        .with_ingest_session(&admission.session.registration, |ingest| {
            (
                ingest.metadata(),
                crate::sync::lock(&ingest.audio_tracks).clone(),
            )
        })
        .await
        .expect("session is live");
    let video = metadata.video.expect("video metadata");
    assert_eq!(
        (video.codec.as_str(), video.width, video.height),
        ("h264", 1280, 720)
    );
    assert_eq!(metadata.audio.map(|audio| audio.sample_rate), Some(48_000));
    assert_eq!(metadata.video_track_count, 1);
    assert_eq!(audio_tracks.len(), 1);
}

/// A connection that goes away while Tokio admits it cannot take the
/// admission, so the bridge releases the registration instead of leaving the
/// input held by nobody.
#[tokio::test]
async fn an_admission_nobody_receives_is_released() {
    let fixture = fixture();
    let admission = fixture.server.admit(peer(), STREAM_KEY).await;
    assert!(session_protocol(&fixture.engine).await.is_some());
    let (answer, answered) = tokio::sync::oneshot::channel();
    drop(answered);
    deliver_admission(&fixture.server, answer, admission).await;
    assert_eq!(session_protocol(&fixture.engine).await, None);
}

/// The Owner thread awaits Tokio from its own Compio runtime: admission and
/// the probe answer across runtimes without blocking either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owner_awaits_admission_from_its_own_runtime() {
    let fixture = fixture();
    let lifecycle = TokioLifecycle::new(tokio::runtime::Handle::current(), fixture.server.clone());
    let owner = tokio::task::spawn_blocking(move || {
        let runtime = compio::runtime::Runtime::new().expect("compio runtime");
        runtime.block_on(async move {
            let refused = lifecycle.admit(peer(), "wrong-key").await;
            assert_eq!(refused.err(), Some(Rejection::Unauthorized));
            let admission = lifecycle
                .admit(peer(), STREAM_KEY)
                .await
                .unwrap_or_else(|rejection| panic!("admitted, got {rejection:?}"));
            let probe = MoqProbe {
                video: None,
                audio_tracks: Vec::new(),
            };
            let _ring = lifecycle.apply_probe(&admission.session, probe).await;
            admission.session
        })
    });
    let session = owner.await.expect("owner thread");
    assert_eq!(
        session_protocol(&fixture.engine)
            .await
            .map(|(protocol, _)| protocol),
        Some("moq".to_string())
    );
    fixture.server.finish(session, "done".to_string()).await;
    assert_eq!(session_protocol(&fixture.engine).await, None);
}
