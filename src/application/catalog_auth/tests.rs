//! Admission semantics over the index, and the reload discipline: a failed
//! reload keeps the previous index and a retry recovers it; concurrent
//! refreshes share loads without losing a write.

use super::*;
use restream_domain::domain::ingest_security::IngestSecurityConfig;
use restream_domain::domain::pipeline_input::PipelineInputSource;
use sqlx::SqlitePool;
use std::sync::atomic::AtomicUsize;

const PIPELINE_ID: &str = "pipeline-1";
const PRIMARY_ID: &str = "input_primary_pipeline-1";
const TEST_WAIT: Duration = Duration::from_secs(5);

fn test_security_config() -> IngestSecurityConfig {
    IngestSecurityConfig {
        failure_limit: 2,
        failure_window_ms: 60_000,
        ban_ms: 60_000,
        tracked_ip_limit: 100,
    }
}

/// Observes real catalog queries. Holding the result here lets coalescing
/// tests interleave writes after SQLite has read the old rows.
#[derive(Default)]
pub(super) struct ReloadProbe {
    completed: AtomicUsize,
    gate: Option<tokio::sync::watch::Receiver<bool>>,
}

impl ReloadProbe {
    pub(super) async fn query_completed(&self) {
        self.completed.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            let mut gate = gate.clone();
            gate.wait_for(|open| *open).await.unwrap();
        }
    }

    fn completed(&self) -> usize {
        self.completed.load(Ordering::SeqCst)
    }
}

async fn wait_for_queries(catalog: &CatalogAuth, count: usize) {
    tokio::time::timeout(TEST_WAIT, async {
        while catalog.reload_probe.completed() < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("catalog attempts the expected loads");
}

async fn seeded_catalog(key: &str) -> (SqlitePool, Arc<CatalogAuth>) {
    // One connection gives the retry tests deterministic read exhaustion.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    db::setup_database_schema(&pool).await.unwrap();
    db::create_pipeline(&pool, PIPELINE_ID, "Pipeline", key, None, None)
        .await
        .unwrap();
    let catalog = CatalogAuth::new(DeadlineDb::new(pool.clone(), Duration::from_millis(100)));
    (pool, catalog)
}

async fn rotate_key(pool: &SqlitePool, key: &str) {
    db::update_pipeline(pool, PIPELINE_ID, "Pipeline", key, None, None)
        .await
        .unwrap()
        .unwrap();
}

fn authenticator(
    catalog: Arc<CatalogAuth>,
) -> (CatalogIngestAuthenticator, Arc<IngestSecurityService>) {
    let security = Arc::new(IngestSecurityService::new(test_security_config()));
    let auth = CatalogIngestAuthenticator::new(catalog, security.clone());
    (auth, security)
}

async fn load_authenticator(
    catalog: Arc<CatalogAuth>,
) -> (CatalogIngestAuthenticator, Arc<IngestSecurityService>) {
    catalog.load().await.unwrap();
    authenticator(catalog)
}

async fn live_authenticator() -> (CatalogIngestAuthenticator, Arc<IngestSecurityService>) {
    let (_, catalog) = seeded_catalog("live").await;
    load_authenticator(catalog).await
}

fn admit(
    auth: &CatalogIngestAuthenticator,
    mode: PipelineAccessMode,
    key: &str,
    ip: &str,
) -> Result<AuthenticatedPipeline, PipelineAccessError> {
    auth.admit(mode, key, ip)
}

#[tokio::test]
async fn rtmp_publish_records_a_failure_for_a_missing_stream_key() {
    let (auth, security) = live_authenticator().await;

    let result = admit(
        &auth,
        PipelineAccessMode::RtmpPublish,
        "missing",
        "10.0.0.1",
    );

    assert_eq!(result, Err(PipelineAccessError::InvalidStreamKey));
    assert!(security.is_ip_banned("10.0.0.1").is_none());
    assert!(security.record_failure("10.0.0.1"));
}

#[tokio::test]
async fn rtmp_publish_resolves_the_input_and_keeps_prior_failures() {
    let (auth, security) = live_authenticator().await;
    let ip = "10.0.0.9";
    assert!(!security.record_failure_for(RateLimitScope::RtmpPublish, ip));

    let pipeline = admit(&auth, PipelineAccessMode::RtmpPublish, "live", ip).unwrap();

    assert_eq!(
        pipeline,
        AuthenticatedPipeline {
            id: PIPELINE_ID.to_string(),
            input_id: PRIMARY_ID.to_string(),
            selected: true,
        }
    );
    // Unlike the other scopes, an accepted RTMP publish does not clear the
    // prior failure count: one more failure still trips the ban.
    assert!(security.record_failure_for(RateLimitScope::RtmpPublish, ip));
    assert!(
        security
            .is_ip_banned_for(RateLimitScope::RtmpPublish, ip)
            .is_some()
    );
}

#[tokio::test]
async fn srt_publish_clears_failure_state_after_success() {
    let (auth, security) = live_authenticator().await;
    let ip = "10.0.0.2";
    assert!(!security.record_failure_for(RateLimitScope::SrtPublish, ip));
    assert!(security.record_failure_for(RateLimitScope::SrtPublish, ip));

    admit(&auth, PipelineAccessMode::SrtPublish, "live", ip).unwrap();

    assert!(
        security
            .is_ip_banned_for(RateLimitScope::SrtPublish, ip)
            .is_none()
    );
}

/// A pull input accepts no publisher. Every push mode must refuse its key, so
/// an encoder cannot take the input's session from the pull task; a read
/// still resolves, because the pipeline's media is readable either way.
#[tokio::test]
async fn pushes_to_a_pull_input_are_refused_and_reads_resolve() {
    let (pool, catalog) = seeded_catalog("live").await;
    let pull = PipelineInputSource {
        kind: PipelineInputSourceKind::HlsPull,
        url: Some("http://origin.example/master.m3u8".to_string()),
        ..PipelineInputSource::keyed()
    };
    db::update_pipeline_input(&pool, PIPELINE_ID, PRIMARY_ID, "Origin", true, &pull)
        .await
        .unwrap()
        .unwrap();
    let (auth, _) = load_authenticator(catalog).await;

    // A distinct client per mode: a third failure from one address is banned,
    // and the ban would answer instead of the refusal under test.
    for (mode, ip) in [
        (PipelineAccessMode::RtmpPublish, "10.0.0.11"),
        (PipelineAccessMode::SrtPublish, "10.0.0.12"),
        (PipelineAccessMode::HlsPublish, "10.0.0.13"),
        (PipelineAccessMode::MoqPublish, "10.0.0.15"),
    ] {
        let result = admit(&auth, mode, "live", ip);
        assert_eq!(
            result,
            Err(PipelineAccessError::InvalidStreamKey),
            "{mode:?} must refuse a pull input's key"
        );
    }
    for mode in [PipelineAccessMode::SrtRead, PipelineAccessMode::RtmpPlay] {
        let read = admit(&auth, mode, "live", "10.0.0.14").expect("a read resolves");
        assert_eq!(read.input_id, PRIMARY_ID);
    }
}

#[tokio::test]
async fn a_disabled_input_key_is_refused_like_an_unknown_one() {
    let (pool, catalog) = seeded_catalog("active-key").await;
    let source = PipelineInputSource::keyed();
    db::create_pipeline_input(&pool, "disabled", PIPELINE_ID, "Backup", "live", &source)
        .await
        .unwrap();
    db::update_pipeline_input(&pool, PIPELINE_ID, "disabled", "Backup", false, &source)
        .await
        .unwrap()
        .unwrap();
    let (auth, security) = load_authenticator(catalog).await;

    for mode in [PipelineAccessMode::SrtPublish, PipelineAccessMode::RtmpPlay] {
        assert_eq!(
            admit(&auth, mode, "live", "10.0.0.16"),
            Err(PipelineAccessError::InvalidStreamKey)
        );
    }
    assert_eq!(security.snapshots()[0].scope, "srt-publish");
}

#[tokio::test]
async fn rtmp_play_resolves_without_rate_limit_side_effects() {
    let (auth, security) = live_authenticator().await;

    admit(&auth, PipelineAccessMode::RtmpPlay, "live", "10.0.0.4").unwrap();
    assert_eq!(
        admit(&auth, PipelineAccessMode::RtmpPlay, "missing", "10.0.0.4"),
        Err(PipelineAccessError::InvalidStreamKey)
    );

    assert!(security.snapshots().is_empty());
}

#[tokio::test]
async fn each_publish_mode_counts_failures_under_its_own_scope_and_clears_them() {
    for (mode, scope, ip) in [
        (PipelineAccessMode::SrtPublish, "srt-publish", "10.0.0.5"),
        (PipelineAccessMode::SrtRead, "srt-read", "10.0.0.8"),
        (PipelineAccessMode::HlsPublish, "hls-publish", "10.0.0.6"),
        (PipelineAccessMode::MoqPublish, "moq-publish", "10.0.0.7"),
    ] {
        let (auth, security) = live_authenticator().await;

        assert_eq!(
            admit(&auth, mode, "missing", ip),
            Err(PipelineAccessError::InvalidStreamKey)
        );
        let snapshots = security.snapshots();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].scope, scope);
        assert_eq!(snapshots[0].ip, ip);
        assert_eq!(snapshots[0].failure_count, 1);

        admit(&auth, mode, "live", ip).unwrap();
        assert!(
            security.snapshots().is_empty(),
            "{mode:?}: an accepted key clears the client's failures"
        );
    }
}

#[tokio::test]
async fn the_index_refuses_every_key_until_it_is_loaded() {
    let (_, catalog) = seeded_catalog("live").await;
    let (auth, _) = authenticator(catalog.clone());

    assert_eq!(
        admit(&auth, PipelineAccessMode::RtmpPlay, "live", "10.0.0.30"),
        Err(PipelineAccessError::InvalidStreamKey)
    );
    catalog.load().await.unwrap();
    admit(&auth, PipelineAccessMode::RtmpPlay, "live", "10.0.0.30").unwrap();
}

/// The startup load retries a failing (or timed-out) catalog read with
/// backoff instead of failing startup, and the index is filled once a read
/// succeeds.
#[tokio::test]
async fn a_failing_startup_load_retries_until_a_load_succeeds() {
    let (pool, catalog) = seeded_catalog("live").await;
    let held = pool.acquire().await.unwrap();
    let load = tokio::spawn({
        let catalog = catalog.clone();
        async move { catalog.load().await }
    });

    wait_for_queries(&catalog, 3).await;
    assert!(
        !load.is_finished(),
        "a failing startup load must keep retrying"
    );
    drop(held);

    tokio::time::timeout(TEST_WAIT, load)
        .await
        .expect("startup load succeeds after the connection returns")
        .unwrap()
        .unwrap();
    assert!(catalog.snapshot().get("live").is_some());
}

#[tokio::test]
async fn a_failing_startup_load_ends_with_its_error_at_shutdown() {
    let (pool, catalog) = seeded_catalog("live").await;
    pool.close().await;
    catalog.shutdown();

    assert!(matches!(catalog.load().await, Err(sqlx::Error::PoolClosed)));
}

/// A refresh whose reload fails keeps the previous index in force (every
/// existing key still connects), and the retry task lands the write once the
/// database answers again.
#[tokio::test]
async fn a_failed_reload_keeps_the_previous_index_until_a_retry_succeeds() {
    let (pool, catalog) = seeded_catalog("old-key").await;
    let (auth, _) = load_authenticator(catalog.clone()).await;
    rotate_key(&pool, "new-key").await;
    let held = pool.acquire().await.unwrap();

    catalog.refresh().await;

    let ip = "10.0.0.40";
    admit(&auth, PipelineAccessMode::RtmpPlay, "old-key", ip)
        .expect("the previous index stays in force while reloads fail");
    assert_eq!(
        admit(&auth, PipelineAccessMode::RtmpPlay, "new-key", ip),
        Err(PipelineAccessError::InvalidStreamKey)
    );

    // Retries back off but keep going while the database still fails.
    let attempts = catalog.reload_probe.completed();
    wait_for_queries(&catalog, attempts + 3).await;
    admit(&auth, PipelineAccessMode::RtmpPlay, "old-key", ip).unwrap();

    drop(held);
    tokio::time::timeout(TEST_WAIT, async {
        while catalog.retrying.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the retry task ends once the connection returns");

    admit(&auth, PipelineAccessMode::RtmpPlay, "new-key", ip).expect("the retry landed the write");
    assert_eq!(
        admit(&auth, PipelineAccessMode::RtmpPlay, "old-key", ip),
        Err(PipelineAccessError::InvalidStreamKey)
    );
}

/// Refreshes that arrive while a reload runs share one later reload, and
/// each returns only once its write is in the index.
#[tokio::test]
async fn refreshes_during_a_reload_coalesce_into_one_later_reload() {
    let (open, gate) = tokio::sync::watch::channel(true);
    let (pool, mut catalog) = seeded_catalog("first").await;
    Arc::get_mut(&mut catalog).unwrap().reload_probe.gate = Some(gate);
    catalog.load().await.unwrap();
    let loads_before = catalog.reload_probe.completed();

    open.send(false).unwrap();
    let running = tokio::spawn({
        let catalog = catalog.clone();
        async move { catalog.refresh().await }
    });
    wait_for_queries(&catalog, loads_before + 1).await;
    // The running reload has read its rows; this write comes after them.
    rotate_key(&pool, "second").await;
    let waiting: Vec<_> = (0..8)
        .map(|_| {
            let catalog = catalog.clone();
            tokio::spawn(async move { catalog.refresh().await })
        })
        .collect();
    // Every waiting refresh has counted its write before the gate opens.
    tokio::time::timeout(TEST_WAIT, async {
        while catalog.requested.load(Ordering::SeqCst) < 10 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the waiting refreshes all request a reload");
    open.send(true).unwrap();
    running.await.unwrap();
    for refresh in waiting {
        refresh.await.unwrap();
    }

    assert!(catalog.snapshot().get("second").is_some());
    assert!(catalog.snapshot().get("first").is_none());
    assert_eq!(
        catalog.reload_probe.completed() - loads_before,
        2,
        "one running reload plus one shared by every waiting refresh"
    );
}
