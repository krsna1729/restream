use super::super::test_support::{
    file_pools, hold_write_lock, live_output, output_starts_within, outputs, release_write_lock,
};
use super::*;
use crate::config::AppConfig;
use crate::db::SHARED_LOOP_DEADLINE;

const OUTPUT: &str = "out-pipe";

fn egress(engine: &Arc<MediaEngine>, db: DeadlineDb) -> EgressReconciler {
    let config = AppConfig::default();
    EgressReconciler::new(
        engine.clone(),
        db,
        config.tuning,
        Arc::new(StateHub::new(
            config.ports,
            config.tuning.output_max_retries,
        )),
        Arc::new(ReconcileRequests::new()),
    )
}

/// The output used up its retries: what `EgressTask` records after its last
/// failed attempt.
async fn exhaust(egress: &EgressReconciler) {
    egress.starter.last_failed.lock().await.insert(
        OUTPUT.to_string(),
        (Instant::now(), egress.starter.tuning.output_max_retries),
    );
}

/// Waits for the output's mark task to end with the row marked.
async fn mark_lands(egress: &EgressReconciler, pool: &sqlx::SqlitePool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let exhausted = outputs(pool)
                .await
                .iter()
                .any(|output| output.id == OUTPUT && output.retries_exhausted);
            if exhausted && !egress.in_flight.contains(OUTPUT) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the retries-exhausted mark lands");
}

/// The mark waits behind another writer past its deadline and lands once
/// the lock is released. The operator's next Start then starts the output:
/// nothing left of the exhausted retries marks it exhausted again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_retries_exhausted_mark_does_not_swallow_the_next_start() {
    let (api, lifecycle, dir) = file_pools("late-mark").await;
    let engine = Arc::new(MediaEngine::new());
    let _ingest = live_output(&engine, &api, "pipe", "sink://local/discard").await;
    let egress = egress(
        &engine,
        DeadlineDb::new(lifecycle, Duration::from_millis(200)),
    );
    exhaust(&egress).await;

    let lock = hold_write_lock(&api).await;
    egress.reconcile(&outputs(&api).await).await;
    // Well past the write's deadline.
    tokio::time::sleep(Duration::from_millis(600)).await;
    release_write_lock(lock).await;
    mark_lands(&egress, &api).await;
    egress.reconcile(&outputs(&api).await).await;

    crate::application::outputs::request_start(&api, "pipe", OUTPUT)
        .await
        .unwrap();
    egress.reconcile(&outputs(&api).await).await;
    let started = output_starts_within(&engine, OUTPUT, Duration::from_secs(2)).await;
    let _ = std::fs::remove_dir_all(dir);
    assert!(
        started,
        "the operator's Start after a late mark did not start the output"
    );
}

/// A pass that read the rows before the mark landed decides from that
/// stale row: it must not start the output the mark just exhausted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pass_reading_rows_from_before_the_mark_does_not_restart_the_output() {
    let (api, lifecycle, dir) = file_pools("stale-mark").await;
    let engine = Arc::new(MediaEngine::new());
    let _ingest = live_output(&engine, &api, "pipe", "sink://local/discard").await;
    let egress = egress(&engine, DeadlineDb::new(lifecycle, SHARED_LOOP_DEADLINE));
    exhaust(&egress).await;

    // Read before the mark: the row says not exhausted.
    let stale = outputs(&api).await;
    egress.reconcile(&stale).await;
    mark_lands(&egress, &api).await;
    egress.reconcile(&stale).await;

    let started = output_starts_within(&engine, OUTPUT, Duration::from_secs(1)).await;
    let _ = std::fs::remove_dir_all(dir);
    assert!(
        !started,
        "a stale row restarted an output whose retries are exhausted"
    );
}

/// Starts behind another writer's lock: every output starts at once, their
/// job-row writes queue holding at most the write permits' connections, and
/// the loops' reads on the lifecycle pool still answer (WAL readers do not
/// wait for the write lock). The rows land once the lock is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_starts_behind_a_write_lock_hold_at_most_the_write_permits_while_reads_answer() {
    const STARTS: usize = 5;
    let (api, lifecycle, dir) = file_pools("write-permits").await;
    let engine = Arc::new(MediaEngine::new());
    let mut ingests = Vec::new();
    for index in 0..STARTS {
        ingests
            .push(live_output(&engine, &api, &format!("p{index}"), "sink://local/discard").await);
    }
    let db = DeadlineDb::new(lifecycle.clone(), SHARED_LOOP_DEADLINE);
    let egress = egress(&engine, db.clone());

    let lock = hold_write_lock(&api).await;
    egress.reconcile(&outputs(&api).await).await;
    for index in 0..STARTS {
        assert!(
            output_starts_within(&engine, &format!("out-p{index}"), Duration::from_secs(2)).await,
            "a start waited on its job row"
        );
    }
    let mut most_held = 0;
    for _ in 0..10 {
        most_held = most_held.max(lifecycle.size() as usize - lifecycle.num_idle());
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let read = tokio::time::timeout(Duration::from_secs(1), db.read(crate::db::list_outputs)).await;
    release_write_lock(lock).await;
    let landed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
                .fetch_one(&api)
                .await
                .unwrap();
            if rows == STARTS as i64 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    drop(ingests);
    let _ = std::fs::remove_dir_all(dir);

    assert!(
        most_held <= crate::db::WRITE_PERMITS,
        "{most_held} lifecycle connections held by queued writes; at most {} allowed",
        crate::db::WRITE_PERMITS
    );
    assert!(
        matches!(read, Ok(Ok(_))),
        "a lifecycle read did not answer while writes queued"
    );
    assert!(
        landed.is_ok(),
        "the job rows landed after the lock was released"
    );
}
