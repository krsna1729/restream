use std::sync::atomic::{AtomicU64, Ordering};

use super::super::test_support::{
    file_pools, hold_every_connection, hold_write_lock, live_output, memory_pool,
    output_starts_within, release_write_lock,
};
use super::*;
use crate::db::SHARED_LOOP_DEADLINE;
use restream_domain::domain::ingest_security::IngestSecurityConfig;
use restream_engine::security::IngestSecurityService;

fn reconciler(engine: Arc<MediaEngine>, db: DeadlineDb, pool: &sqlx::SqlitePool) -> Reconciler {
    let config = Arc::new(AppConfig::default());
    let security = Arc::new(IngestSecurityService::new(IngestSecurityConfig::default()));
    let hls_ingest = restream_engine::hls_ingest::HlsIngest::new(
        engine.clone(),
        Arc::new(
            crate::application::catalog_auth::CatalogIngestAuthenticator::new(
                crate::infrastructure::service_wiring::SqliteServiceFactory::new(pool)
                    .catalog_auth(),
                security.clone(),
            ),
        ),
        security,
        restream_engine::hls_ingest::HlsIngestLimits::default(),
    );
    let runtime_state = Arc::new(crate::application::runtime_state::hub::StateHub::new(
        config.ports,
        config.tuning.output_max_retries,
    ));
    Reconciler::new(
        config,
        db,
        engine,
        Arc::new(ReconcileRequests::new()),
        runtime_state,
        hls_ingest,
    )
}

/// A listener that faults while the output pass is stalled (every
/// lifecycle connection held: each of the pass's reads waits out its
/// deadline) restarts within its 1 s backoff, not after the pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_faulted_listener_restarts_within_its_backoff_while_the_output_pass_is_stalled() {
    let (api, lifecycle, dir) = file_pools("listener").await;
    let reconciler = Arc::new(reconciler(
        Arc::new(MediaEngine::new()),
        DeadlineDb::new(lifecycle.clone(), SHARED_LOOP_DEADLINE),
        &api,
    ));
    let held = hold_every_connection(&lifecycle).await;
    let listener = |restarts: Arc<AtomicU64>| {
        super::super::listener_supervisor::SupervisedListener::new(
            "test",
            // Faulted: the listener task has already ended.
            tokio::spawn(async {}),
            Box::new(|| tokio::spawn(std::future::pending())),
            restarts,
        )
    };
    let restarts = Arc::new(AtomicU64::new(0));
    let listeners = super::super::runtime::Listeners::new(
        tokio::spawn(std::future::pending()),
        listener(restarts.clone()),
        listener(Arc::new(AtomicU64::new(0))),
    );
    let shutdown = CancellationToken::new();
    let control = tokio::spawn({
        let (reconciler, shutdown) = (reconciler.clone(), shutdown.clone());
        async move { super::super::run_control_loops(&reconciler, listeners, &shutdown).await }
    });

    let started = Instant::now();
    let restarted = tokio::time::timeout(Duration::from_secs(3), async {
        while restarts.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    shutdown.cancel();
    control.abort();
    drop(held);
    let _ = std::fs::remove_dir_all(dir);
    assert!(
        restarted.is_ok(),
        "the faulted listener did not restart within its 1 s backoff (+2 s) while the pass was stalled; waited {:?}",
        started.elapsed()
    );
}

/// Pipeline A's output start stalls on its `ca` file (a FIFO no one
/// writes: a hung filesystem). Pipeline B's output still starts at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_pipelines_output_starts_while_one_output_start_is_stalled() {
    let dir = std::env::temp_dir().join(format!(
        "restream-ca-fifo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let fifo = dir.join("stalled.pem");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let engine = Arc::new(MediaEngine::new_with_config(Arc::new(
        restream_engine::config::EngineConfig {
            moq_ca_dir: Some(dir.to_string_lossy().into_owned()),
            ..Default::default()
        },
    )));
    let pool = memory_pool().await;
    // A first: `list_outputs` returns creation order, so the pass reaches
    // A's start before B's.
    let _a = live_output(
        &engine,
        &pool,
        "pipe-a",
        "moqt://localhost/live?ca=stalled.pem",
    )
    .await;
    let _b = live_output(&engine, &pool, "pipe-b", "sink://local/discard").await;
    let reconciler = Arc::new(reconciler(
        engine.clone(),
        DeadlineDb::new(pool.clone(), SHARED_LOOP_DEADLINE),
        &pool,
    ));

    let pass = tokio::spawn({
        let reconciler = reconciler.clone();
        async move {
            reconciler
                .contained_output_pass(Duration::from_secs(1))
                .await
        }
    });
    let b_started = output_starts_within(&engine, "out-pipe-b", Duration::from_secs(2)).await;

    // End the stalled read so its blocking thread returns.
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo);
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), pass).await;
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        b_started,
        "pipeline B's output did not start within 2 s while pipeline A's output start was stalled on its CA file"
    );
}

/// Every lifecycle connection held: the pass's reads answer at their
/// deadline, the pass ends and asks for a retry; once a connection is free,
/// the next pass starts the output.
#[tokio::test]
async fn a_stalled_pool_ends_the_pass_at_its_deadline_and_the_output_converges_after() {
    let engine = Arc::new(MediaEngine::new());
    let pool = memory_pool().await;
    let _ingest = live_output(&engine, &pool, "pipe", "sink://local/discard").await;
    let reconciler = reconciler(
        engine.clone(),
        DeadlineDb::new(pool.clone(), Duration::from_millis(200)),
        &pool,
    );
    let held = hold_every_connection(&pool).await;

    let completed = tokio::time::timeout(
        Duration::from_secs(5),
        reconciler.contained_output_pass(Duration::from_secs(1)),
    )
    .await
    .expect("the pass answers at its deadlines, not the acquire timeout");
    assert!(completed);
    assert!(
        reconciler.requests.next_deadline().is_some(),
        "the pass asked to be retried"
    );
    assert!(!engine.has_active_egress("out-pipe").await);

    drop(held);
    reconciler
        .contained_output_pass(Duration::from_secs(1))
        .await;
    assert!(output_starts_within(&engine, "out-pipe", Duration::from_secs(2)).await);
}

/// Another writer holds the write lock: the output starts and the pass ends
/// on time, since the job row is written off the start path; it lands once
/// the lock is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_output_starts_while_another_writer_holds_the_write_lock() {
    let (api, lifecycle, dir) = file_pools("write-lock").await;
    let engine = Arc::new(MediaEngine::new());
    let _ingest = live_output(&engine, &api, "pipe", "sink://local/discard").await;
    let reconciler = reconciler(
        engine.clone(),
        DeadlineDb::new(lifecycle, SHARED_LOOP_DEADLINE),
        &api,
    );
    let lock = hold_write_lock(&api).await;

    tokio::time::timeout(
        Duration::from_secs(5),
        reconciler.contained_output_pass(Duration::from_secs(1)),
    )
    .await
    .expect("the pass does not wait on the write lock");
    assert!(output_starts_within(&engine, "out-pipe", Duration::from_secs(2)).await);

    release_write_lock(lock).await;
    let landed = tokio::time::timeout(Duration::from_secs(5), async {
        while crate::db::list_jobs(&api).await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let _ = std::fs::remove_dir_all(dir);
    assert!(
        landed.is_ok(),
        "the job row lands after the lock is released"
    );
}

/// API reads hold every connection of the API pool: the pass reads through
/// the lifecycle pool, so another pipeline's output starts on time.
#[tokio::test]
async fn an_output_starts_while_api_reads_hold_every_api_connection() {
    let (api, lifecycle, dir) = file_pools("api-held").await;
    let engine = Arc::new(MediaEngine::new());
    let _ingest = live_output(&engine, &api, "pipe", "sink://local/discard").await;
    let reconciler = reconciler(
        engine.clone(),
        DeadlineDb::new(lifecycle, SHARED_LOOP_DEADLINE),
        &api,
    );
    let held = hold_every_connection(&api).await;

    reconciler
        .contained_output_pass(Duration::from_secs(1))
        .await;
    let started = output_starts_within(&engine, "out-pipe", Duration::from_secs(2)).await;
    drop(held);
    let _ = std::fs::remove_dir_all(dir);
    assert!(
        started,
        "the output did not start while API reads held the API pool"
    );
}
