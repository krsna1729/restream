//! Tokio's half of MoQ ingest against a real engine: admission, the input
//! registration it makes, the probe and the bookkeeping at the end.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessError, PipelineAccessFuture, PipelineAccessMode,
};
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_domain::domain::ingest_security::IngestSecurityConfig;

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
    assert_eq!(
        stream_key_from_path(&format!("{longest}.msf")),
        Some(longest.as_str())
    );
    assert_eq!(stream_key_from_path(&format!("{longest}k")), None);
}

#[tokio::test]
async fn an_admitted_publisher_holds_a_moq_session_until_it_finishes() {
    let fixture = fixture();
    let admitted = fixture
        .server
        .admit(peer(), STREAM_KEY)
        .await
        .unwrap_or_else(|rejection| panic!("admitted, got {rejection:?}"));
    assert_eq!(admitted.session.pipeline_id, PIPELINE_ID);
    assert_eq!(admitted.session.registration.input_id, INPUT_ID);
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
        .finish(admitted.lease, "test end".to_string())
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
    let admitted = fixture.server.admit(peer(), STREAM_KEY).await.ok().unwrap();
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
    fixture.server.apply_probe(&admitted.session, probe).await;

    let (metadata, audio_tracks) = fixture
        .engine
        .with_ingest_session(&admitted.session.registration, |ingest| {
            (
                ingest.metadata(),
                restream_sync::lock::lock(&ingest.audio_tracks).clone(),
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
    let admitted = fixture.server.admit(peer(), STREAM_KEY).await.ok().unwrap();
    assert!(session_protocol(&fixture.engine).await.is_some());
    let (answer, answered) = tokio::sync::oneshot::channel();
    drop(answered);
    hold_admission(&fixture.server, answer, admitted).await;
    assert_eq!(session_protocol(&fixture.engine).await, None);
}

/// Admit through the production bridge from a Compio runtime, as the Owner
/// thread does; `owner` runs there with the admission.
async fn admit_on_owner<T: Send + 'static>(
    lifecycle: TokioLifecycle,
    owner: impl FnOnce(TokioLifecycle, Admission) -> T + Send + 'static,
) -> T {
    tokio::task::spawn_blocking(move || {
        let runtime = compio::runtime::Runtime::new().expect("compio runtime");
        runtime.block_on(async move {
            let admission = lifecycle
                .admit(peer(), STREAM_KEY)
                .await
                .unwrap_or_else(|rejection| panic!("admitted, got {rejection:?}"));
            owner(lifecycle, admission)
        })
    })
    .await
    .expect("owner thread")
}

async fn wait_until_released(engine: &MediaEngine) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while session_protocol(engine).await.is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the input is released");
}

/// The Owner thread awaits Tokio from its own Compio runtime: admission and
/// the probe answer across runtimes without blocking either, and the reason
/// it ends with is recorded.
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
            admission.end
        })
    });
    let end = owner.await.expect("owner thread");
    assert_eq!(
        session_protocol(&fixture.engine)
            .await
            .map(|(protocol, _)| protocol),
        Some("moq".to_string())
    );
    end.finish("done".to_string());
    wait_until_released(&fixture.engine).await;
    let outcome = fixture.engine.recent_ingest_outcome(PIPELINE_ID).await;
    assert_eq!(
        outcome.and_then(|outcome| outcome.reason).as_deref(),
        Some("done")
    );
}

/// An Owner session dropped without finishing (its Compio task unwound, the
/// listener was torn down mid-session) still frees the stream key: the Tokio
/// half drops the attempt's lease, and the next publisher is admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unfinished_owner_session_frees_its_stream_key() {
    let fixture = fixture();
    let lifecycle = TokioLifecycle::new(tokio::runtime::Handle::current(), fixture.server.clone());
    admit_on_owner(lifecycle.clone(), |lifecycle, admission| {
        drop(session::Ingest::new(
            lifecycle,
            admission,
            session::SessionLimits::default(),
        ));
    })
    .await;
    wait_until_released(&fixture.engine).await;
    let outcome = fixture
        .engine
        .recent_ingest_outcome(PIPELINE_ID)
        .await
        .expect("the abandoned end is recorded");
    assert_eq!(
        outcome.failure_phase.as_deref(),
        Some(crate::ingest_lease::ABANDONED_PHASE)
    );
    let end = admit_on_owner(lifecycle, |_, admission| admission.end).await;
    end.finish("done".to_string());
    wait_until_released(&fixture.engine).await;
}
