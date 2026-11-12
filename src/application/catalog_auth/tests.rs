//! Admission semantics over the index, and the reload discipline: a failed
//! reload keeps the previous index and a retry recovers it; concurrent
//! refreshes share loads without losing a write.

use super::*;
use restream_domain::domain::ingest_security::IngestSecurityConfig;
use restream_domain::domain::pipeline_input::{PipelineInputRole, PullAuth, VariantRule};
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

fn test_security_config() -> IngestSecurityConfig {
    IngestSecurityConfig {
        failure_limit: 2,
        failure_window_ms: 60_000,
        ban_ms: 60_000,
        tracked_ip_limit: 100,
    }
}

fn keyed(stream_key: &str) -> PipelineInput {
    PipelineInput {
        id: "input-primary".to_string(),
        pipeline_id: "pipeline-1".to_string(),
        label: "Primary".to_string(),
        stream_key: stream_key.to_string(),
        role: PipelineInputRole::Primary,
        enabled: true,
        selected: true,
        ..PipelineInput::keyed_default()
    }
}

/// A catalog whose loads can fail, or wait for a gate, on demand.
#[derive(Default)]
struct FakeCatalog {
    inputs: Mutex<Vec<PipelineInput>>,
    failing: AtomicBool,
    attempts: AtomicUsize,
    gate: Option<tokio::sync::watch::Receiver<bool>>,
}

impl FakeCatalog {
    fn with(inputs: Vec<PipelineInput>) -> Self {
        Self {
            inputs: Mutex::new(inputs),
            ..Self::default()
        }
    }

    fn set(&self, inputs: Vec<PipelineInput>) {
        *restream_sync::lock::lock(&self.inputs) = inputs;
    }
}

impl CatalogAuthSource for FakeCatalog {
    fn load_inputs(&self) -> CatalogAuthLoadFuture<'_> {
        Box::pin(async move {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            // Rows are read when the query starts; the gate holds the answer.
            let inputs = restream_sync::lock::lock(&self.inputs).clone();
            if let Some(gate) = &self.gate {
                let mut gate = gate.clone();
                gate.wait_for(|open| *open).await.unwrap();
            }
            if self.failing.load(Ordering::SeqCst) {
                return Err(PipelineStoreError::new("database is locked"));
            }
            Ok(inputs)
        })
    }
}

async fn authenticator(
    inputs: Vec<PipelineInput>,
) -> (
    CatalogIngestAuthenticator,
    Arc<CatalogAuth>,
    Arc<FakeCatalog>,
    Arc<IngestSecurityService>,
) {
    let source = Arc::new(FakeCatalog::with(inputs));
    let catalog = CatalogAuth::new(source.clone());
    catalog.load().await.unwrap();
    let security = Arc::new(IngestSecurityService::new(test_security_config()));
    (
        CatalogIngestAuthenticator::new(catalog.clone(), security.clone()),
        catalog,
        source,
        security,
    )
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
    let (auth, _, _, security) = authenticator(vec![keyed("live")]).await;

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
    let (auth, _, _, security) = authenticator(vec![keyed("live")]).await;
    let ip = "10.0.0.9";
    assert!(!security.record_failure_for(RateLimitScope::RtmpPublish, ip));

    let pipeline = admit(&auth, PipelineAccessMode::RtmpPublish, "live", ip).unwrap();

    assert_eq!(
        pipeline,
        AuthenticatedPipeline {
            id: "pipeline-1".to_string(),
            input_id: "input-primary".to_string(),
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
    let (auth, _, _, security) = authenticator(vec![keyed("live")]).await;
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
    let pull = PipelineInput {
        id: "input-pull".to_string(),
        source_kind: PipelineInputSourceKind::HlsPull,
        source_url: Some("http://origin.example/master.m3u8".to_string()),
        variant_rule: VariantRule::HighestBandwidth,
        source_auth: PullAuth::None,
        ..keyed("live")
    };
    let (auth, _, _, _) = authenticator(vec![pull]).await;

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
        assert_eq!(read.input_id, "input-pull");
    }
}

#[tokio::test]
async fn a_disabled_input_key_is_refused_like_an_unknown_one() {
    let disabled = PipelineInput {
        enabled: false,
        ..keyed("live")
    };
    let (auth, _, _, security) = authenticator(vec![disabled]).await;

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
    let (auth, _, _, security) = authenticator(vec![keyed("live")]).await;

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
        let (auth, _, _, security) = authenticator(vec![keyed("live")]).await;

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
    let source = Arc::new(FakeCatalog::with(vec![keyed("live")]));
    let catalog = CatalogAuth::new(source);
    let security = Arc::new(IngestSecurityService::new(test_security_config()));
    let auth = CatalogIngestAuthenticator::new(catalog.clone(), security);

    assert_eq!(
        admit(&auth, PipelineAccessMode::RtmpPlay, "live", "10.0.0.30"),
        Err(PipelineAccessError::InvalidStreamKey)
    );
    catalog.load().await.unwrap();
    admit(&auth, PipelineAccessMode::RtmpPlay, "live", "10.0.0.30").unwrap();
}

#[tokio::test]
async fn a_failed_startup_load_is_an_error() {
    let source = Arc::new(FakeCatalog::with(vec![keyed("live")]));
    source.failing.store(true, Ordering::SeqCst);

    assert!(CatalogAuth::new(source).load().await.is_err());
}

/// A refresh whose reload fails keeps the previous index in force (every
/// existing key still connects), and the retry task lands the write once the
/// database answers again.
#[tokio::test(start_paused = true)]
async fn a_failed_reload_keeps_the_previous_index_until_a_retry_succeeds() {
    let (auth, catalog, source, _) = authenticator(vec![keyed("old-key")]).await;
    source.failing.store(true, Ordering::SeqCst);
    source.set(vec![keyed("new-key")]);

    catalog.refresh().await;

    let ip = "10.0.0.40";
    admit(&auth, PipelineAccessMode::RtmpPlay, "old-key", ip)
        .expect("the previous index stays in force while reloads fail");
    assert_eq!(
        admit(&auth, PipelineAccessMode::RtmpPlay, "new-key", ip),
        Err(PipelineAccessError::InvalidStreamKey)
    );

    // Retries back off but keep going while the database still fails.
    let attempts = source.attempts.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert!(source.attempts.load(Ordering::SeqCst) >= attempts + 3);
    admit(&auth, PipelineAccessMode::RtmpPlay, "old-key", ip).unwrap();

    source.failing.store(false, Ordering::SeqCst);
    tokio::time::sleep(RETRY_MAX_DELAY * 2).await;

    admit(&auth, PipelineAccessMode::RtmpPlay, "new-key", ip).expect("the retry landed the write");
    assert_eq!(
        admit(&auth, PipelineAccessMode::RtmpPlay, "old-key", ip),
        Err(PipelineAccessError::InvalidStreamKey)
    );
    assert!(
        !catalog.retrying.load(Ordering::SeqCst),
        "the retry task ended"
    );
}

/// Refreshes that arrive while a reload runs share one later reload, and
/// each returns only once its write is in the index.
#[tokio::test]
async fn refreshes_during_a_reload_coalesce_into_one_later_reload() {
    let (open, gate) = tokio::sync::watch::channel(true);
    let source = Arc::new(FakeCatalog {
        gate: Some(gate),
        ..FakeCatalog::with(vec![keyed("first")])
    });
    let catalog = CatalogAuth::new(source.clone());
    catalog.load().await.unwrap();
    let loads_before = source.attempts.load(Ordering::SeqCst);

    open.send(false).unwrap();
    let running = tokio::spawn({
        let catalog = catalog.clone();
        async move { catalog.refresh().await }
    });
    while source.attempts.load(Ordering::SeqCst) == loads_before {
        tokio::task::yield_now().await;
    }
    // The running reload has read its rows; this write comes after them.
    source.set(vec![keyed("second")]);
    let waiting: Vec<_> = (0..8)
        .map(|_| {
            let catalog = catalog.clone();
            tokio::spawn(async move { catalog.refresh().await })
        })
        .collect();
    // Every waiting refresh has counted its write before the gate opens.
    while catalog.requested.load(Ordering::SeqCst) < 10 {
        tokio::task::yield_now().await;
    }
    open.send(true).unwrap();
    running.await.unwrap();
    for refresh in waiting {
        refresh.await.unwrap();
    }

    assert!(catalog.snapshot().get("second").is_some());
    assert!(catalog.snapshot().get("first").is_none());
    assert_eq!(
        source.attempts.load(Ordering::SeqCst) - loads_before,
        2,
        "one running reload plus one shared by every waiting refresh"
    );
}
