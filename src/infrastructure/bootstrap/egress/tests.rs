use super::super::test_support::{
    file_pools, hold_write_lock, live_output, memory_pool, output_starts_within, outputs,
    release_write_lock,
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

/// Armed by [`an_output_whose_planning_panics_leaves_the_others_planned_in_the_same_pass`].
static PANIC_FOR_A: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Times the armed decision ran for `out-a`.
static PANICKED_FOR_A: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Production's start decision, except that once armed it panics for
/// `out-a`.
#[allow(clippy::too_many_arguments)]
fn panicking_for_a(
    desired_state: DesiredOutputState,
    retries_exhausted: bool,
    is_active: bool,
    effective_has_ingest: bool,
    ingest_tracks_ready: bool,
    failure: Option<OutputFailureWindow>,
    policy: OutputRetryPolicy,
    output_id: &str,
) -> OutputStartAction {
    use std::sync::atomic::Ordering;
    if output_id == "out-a" && PANIC_FOR_A.load(Ordering::SeqCst) {
        PANICKED_FOR_A.fetch_add(1, Ordering::SeqCst);
        // `resume_unwind` skips the panic hook: quiet logs.
        std::panic::resume_unwind(Box::new("injected output planning panic"));
    }
    decide_output_start_action(
        desired_state,
        retries_exhausted,
        is_active,
        effective_has_ingest,
        ingest_tracks_ready,
        failure,
        policy,
        output_id,
    )
}

/// Output A's planning panics. In that same pass output B, after it,
/// starts and output C stops as wanted; A backs off, so the next pass does
/// not plan it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_output_whose_planning_panics_leaves_the_others_planned_in_the_same_pass() {
    use std::sync::atomic::Ordering;
    let pool = memory_pool().await;
    let engine = Arc::new(MediaEngine::new());
    // Rows in creation order: the pass reaches A first.
    let _a = live_output(&engine, &pool, "a", "sink://local/discard").await;
    let _b = live_output(&engine, &pool, "b", "sink://local/discard").await;
    let _c = live_output(&engine, &pool, "c", "sink://local/discard").await;
    crate::application::outputs::request_stop(&pool, "b", "out-b")
        .await
        .unwrap();
    let egress = egress(&engine, DeadlineDb::new(pool.clone(), SHARED_LOOP_DEADLINE))
        .with_start_decision(panicking_for_a);
    egress.reconcile(&outputs(&pool).await).await;
    assert!(output_starts_within(&engine, "out-a", Duration::from_secs(2)).await);
    assert!(output_starts_within(&engine, "out-c", Duration::from_secs(2)).await);

    crate::application::outputs::request_start(&pool, "b", "out-b")
        .await
        .unwrap();
    crate::application::outputs::request_stop(&pool, "c", "out-c")
        .await
        .unwrap();
    PANIC_FOR_A.store(true, Ordering::SeqCst);
    let wait = egress.reconcile(&outputs(&pool).await).await;
    let b_started = output_starts_within(&engine, "out-b", Duration::from_secs(2)).await;
    let c_running = engine.has_active_egress("out-c").await;
    egress.reconcile(&outputs(&pool).await).await;

    assert!(
        b_started,
        "B did not start in the pass where A's planning panicked"
    );
    assert!(
        !c_running,
        "C was not stopped in the pass where A's planning panicked"
    );
    assert!(
        wait.is_some(),
        "the pass asked to come back after A's backoff"
    );
    assert_eq!(
        PANICKED_FOR_A.load(Ordering::SeqCst),
        1,
        "A is not planned again before its backoff"
    );
    assert!(
        engine.has_active_egress("out-a").await,
        "A was left as it was"
    );
}
