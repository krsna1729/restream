//! Catalog writes through the production services on a file-backed SQLite
//! database are enforced on the very next connect, and admission never waits
//! on SQLite: with every pooled connection held, every access mode and the
//! HLS push route still answer at once.

use super::*;
use crate::application::pipeline_inputs::PipelineInputService;
use crate::application::services::PipelineService;
use crate::infrastructure::service_wiring::SqliteServiceFactory;
use restream_domain::domain::ingest_security::IngestSecurityConfig;
use restream_domain::domain::pipeline_input::PipelineInputSource;

const DEADLINE: Duration = Duration::from_secs(1);
const CLIENT_IP: &str = "10.0.1.1";
const ALL_MODES: [PipelineAccessMode; 6] = [
    PipelineAccessMode::RtmpPublish,
    PipelineAccessMode::RtmpPlay,
    PipelineAccessMode::SrtPublish,
    PipelineAccessMode::SrtRead,
    PipelineAccessMode::HlsPublish,
    PipelineAccessMode::MoqPublish,
];

/// Removes the test's database directory when the test ends.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) struct Catalog {
    pub(super) pool: sqlx::SqlitePool,
    pub(super) pipelines: PipelineService,
    inputs: PipelineInputService,
    pub(super) catalog: Arc<CatalogAuth>,
    security: Arc<IngestSecurityService>,
    auth: CatalogIngestAuthenticator,
    _dir: TempDir,
}

impl Catalog {
    /// Production wiring: one factory's services share the index that the
    /// authenticator reads, loaded as startup loads it.
    pub(super) async fn open(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "restream-catalog-auth-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = TempDir(dir);
        let url = format!("sqlite://{}", dir.0.join("db.sqlite").display());
        let pool = crate::db::create_pool(&url).await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        let factory = SqliteServiceFactory::new(&pool);
        let catalog = factory.catalog_auth();
        catalog.load().await.unwrap();
        let pipelines = factory.pipeline_service();
        let inputs = factory.pipeline_input_service(pipelines.clone());
        // No bans: the matrix refuses keys on purpose.
        let security = Arc::new(IngestSecurityService::new(IngestSecurityConfig {
            failure_limit: 10_000,
            failure_window_ms: 60_000,
            ban_ms: 60_000,
            tracked_ip_limit: 100,
        }));
        let auth = CatalogIngestAuthenticator::new(catalog.clone(), security.clone());
        Self {
            pool,
            pipelines,
            inputs,
            catalog,
            security,
            auth,
            _dir: dir,
        }
    }

    pub(super) async fn admitted(
        &self,
        mode: PipelineAccessMode,
        key: &str,
    ) -> Option<AuthenticatedPipeline> {
        self.auth.authenticate(mode, key, CLIENT_IP).await.ok()
    }
}

/// An enabled input whose stored row does not decode keeps its key known:
/// every attempt on it is refused without counting a failure, so the
/// client's address is not banned from its healthy keys. Before, the row was
/// dropped on reload and its key looked wrong.
#[tokio::test]
async fn an_undecodable_input_row_is_refused_without_banning_the_client() {
    let c = Catalog::open("undecodable").await;
    c.pipelines
        .create_pipeline("pipe", "Pipe", "healthy-key", None, None)
        .await
        .unwrap();
    let broken = c
        .inputs
        .create("pipe", "Broken", PipelineInputSource::keyed())
        .await
        .unwrap();
    sqlx::query("UPDATE pipeline_inputs SET push_auth = '{\"kind\":\"basic\"}' WHERE id = ?")
        .bind(&broken.id)
        .execute(&c.pool)
        .await
        .unwrap();
    c.catalog.refresh().await;
    // A ban after two failures in a scope.
    let security = Arc::new(IngestSecurityService::new(IngestSecurityConfig {
        failure_limit: 2,
        failure_window_ms: 60_000,
        ban_ms: 60_000,
        tracked_ip_limit: 100,
    }));
    let auth = CatalogIngestAuthenticator::new(c.catalog.clone(), security.clone());

    for _ in 0..5 {
        for mode in ALL_MODES {
            assert_eq!(
                auth.authenticate(mode, &broken.stream_key, CLIENT_IP).await,
                Err(PipelineAccessError::InputUnreadable),
                "{mode:?}"
            );
        }
    }
    assert!(
        security.snapshots().is_empty(),
        "a broken row counted failures against the client: {:?}",
        security.snapshots()
    );
    for scope in [
        RateLimitScope::RtmpPublish,
        RateLimitScope::SrtPublish,
        RateLimitScope::SrtRead,
        RateLimitScope::HlsPublish,
        RateLimitScope::MoqPublish,
    ] {
        assert!(security.is_ip_banned_for(scope, CLIENT_IP).is_none());
    }
    for mode in ALL_MODES {
        auth.authenticate(mode, "healthy-key", CLIENT_IP)
            .await
            .expect("a healthy key from the same client still connects");
    }
}

fn pull_source() -> PipelineInputSource {
    PipelineInputSource {
        kind: PipelineInputSourceKind::HlsPull,
        url: Some("http://origin.example/master.m3u8".to_string()),
        ..PipelineInputSource::keyed()
    }
}

/// Each assertion runs right after the service call returns: no wait, no
/// second refresh.
#[tokio::test]
async fn every_catalog_write_is_enforced_on_the_next_connect() {
    use PipelineAccessMode::{HlsPublish, MoqPublish, RtmpPlay, RtmpPublish, SrtPublish, SrtRead};
    let c = Catalog::open("matrix").await;

    // Create a pipeline: its primary key is admitted.
    assert!(c.admitted(RtmpPublish, "key-a").await.is_none());
    c.pipelines
        .create_pipeline("pipe", "Pipe", "key-a", None, None)
        .await
        .unwrap();
    let primary = c
        .admitted(RtmpPublish, "key-a")
        .await
        .expect("a created pipeline's key is admitted");
    assert_eq!(primary.id, "pipe");
    assert!(primary.selected);

    // Rotate the key: the old one is refused, the new one admitted.
    c.pipelines
        .update_pipeline("pipe", "Pipe", "key-b", None, None)
        .await
        .unwrap();
    assert!(c.admitted(RtmpPublish, "key-a").await.is_none());
    assert_eq!(
        c.admitted(RtmpPublish, "key-b").await.unwrap().input_id,
        primary.input_id
    );

    // Create an input: its generated key is admitted, as a standby.
    let backup = c
        .inputs
        .create("pipe", "Backup", PipelineInputSource::keyed())
        .await
        .unwrap();
    let standby = c.admitted(SrtPublish, &backup.stream_key).await.unwrap();
    assert_eq!(standby.input_id, backup.id);
    assert!(!standby.selected);

    // Disable it: refused. Enable it again: admitted.
    let keyed = PipelineInputSource::keyed();
    c.inputs
        .update("pipe", &backup.id, "Backup", false, keyed.clone())
        .await
        .unwrap();
    for mode in ALL_MODES {
        assert!(c.admitted(mode, &backup.stream_key).await.is_none());
    }
    c.inputs
        .update("pipe", &backup.id, "Backup", true, keyed.clone())
        .await
        .unwrap();
    assert!(c.admitted(SrtPublish, &backup.stream_key).await.is_some());

    // Promote it: the selected flag moves.
    c.inputs.promote("pipe", &backup.id).await.unwrap();
    assert!(
        c.admitted(RtmpPublish, &backup.stream_key)
            .await
            .unwrap()
            .selected
    );
    assert!(!c.admitted(RtmpPublish, "key-b").await.unwrap().selected);

    // Change an input's HLS push credential: the route reads the new one.
    let credential = restream_domain::domain::pipeline_input::PushAuth::Basic {
        username: "encoder".to_string(),
        password_hash: "salt:hash".to_string(),
    };
    c.inputs
        .update(
            "pipe",
            &backup.id,
            "Backup",
            true,
            PipelineInputSource {
                push_auth: credential.clone(),
                ..keyed.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        c.catalog
            .snapshot()
            .get(&backup.stream_key)
            .unwrap()
            .push_auth,
        credential
    );

    // Switch an input from push to pull: every push is refused, reads resolve.
    let other = c.inputs.create("pipe", "Other", keyed).await.unwrap();
    assert!(c.admitted(RtmpPublish, &other.stream_key).await.is_some());
    c.inputs
        .update("pipe", &other.id, "Other", true, pull_source())
        .await
        .unwrap();
    for mode in [RtmpPublish, SrtPublish, HlsPublish, MoqPublish] {
        assert!(
            c.admitted(mode, &other.stream_key).await.is_none(),
            "{mode:?} must refuse a pull input's key"
        );
    }
    for mode in [SrtRead, RtmpPlay] {
        assert_eq!(
            c.admitted(mode, &other.stream_key).await.unwrap().input_id,
            other.id
        );
    }

    // Delete the input: refused.
    assert!(c.inputs.delete("pipe", &other.id).await.unwrap());
    assert!(c.admitted(SrtRead, &other.stream_key).await.is_none());

    // Delete the pipeline: every key it had is refused.
    assert!(c.pipelines.delete_pipeline("pipe").await.unwrap());
    for key in ["key-b", backup.stream_key.as_str()] {
        for mode in ALL_MODES {
            assert!(c.admitted(mode, key).await.is_none(), "{mode:?} {key}");
        }
    }
}

/// Q-122's gate. Before the index, every mode awaited
/// `get_pipeline_by_stream_key` and this test timed out on the first one.
#[tokio::test]
async fn every_access_mode_authenticates_while_the_database_pool_is_exhausted() {
    let c = Catalog::open("exhausted").await;
    c.pipelines
        .create_pipeline("pipe", "Pipe", "live-key", None, None)
        .await
        .unwrap();
    let engine = Arc::new(restream_engine::engine::MediaEngine::new());
    let ingest = restream_engine::hls_ingest::HlsIngest::new(
        engine,
        Arc::new(CatalogIngestAuthenticator::new(
            c.catalog.clone(),
            c.security.clone(),
        )),
        c.security.clone(),
        restream_engine::hls_ingest::HlsIngestLimits::default(),
    );
    let app = crate::api::create_hls_ingest_router(ingest, c.catalog.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(crate::api::http_server::serve(
        listener,
        app,
        crate::api::http_server::ServeOptions::new(
            1_024,
            tokio_util::sync::CancellationToken::new(),
        ),
    ));

    let mut held = Vec::new();
    for _ in 0..c.pool.options().get_max_connections() {
        held.push(c.pool.acquire().await.unwrap());
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), c.pool.acquire())
            .await
            .is_err(),
        "the pool must be exhausted for this test to mean anything"
    );

    for mode in ALL_MODES {
        let started = std::time::Instant::now();
        let result =
            tokio::time::timeout(DEADLINE, c.auth.authenticate(mode, "live-key", CLIENT_IP)).await;
        assert!(
            matches!(result, Ok(Ok(_))),
            "{mode:?} waited on the exhausted pool: {result:?} after {:?}",
            started.elapsed()
        );
    }
    // A refusal answers at once too: an unknown key never falls through to
    // the database.
    let refused = tokio::time::timeout(
        DEADLINE,
        c.auth
            .authenticate(PipelineAccessMode::RtmpPublish, "unknown-key", CLIENT_IP),
    )
    .await;
    assert!(
        matches!(refused, Ok(Err(PipelineAccessError::InvalidStreamKey))),
        "an unknown key waited on the exhausted pool: {refused:?}"
    );
    // The HLS push route: admission plus its per-object credential check.
    let response = tokio::time::timeout(
        DEADLINE,
        reqwest::Client::new()
            .delete(format!("http://{address}/ingest/hls/live-key/index0.ts"))
            .send(),
    )
    .await
    .expect("the HLS push route waited on the exhausted pool")
    .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    drop(held);
}
