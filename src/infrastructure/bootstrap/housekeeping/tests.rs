use super::super::test_support::{hold_every_connection, memory_pool};
use super::*;
use crate::db::SHARED_LOOP_DEADLINE;

fn housekeeping(db: DeadlineDb) -> Housekeeping {
    Housekeeping::new(
        Arc::new(AppConfig::default()),
        db,
        Arc::new(MediaEngine::new()),
        Arc::default(),
        restream_engine::recording::recording_metadata_channel().0,
    )
}

/// A panic in a tick is contained, reported as a failed step, and the next
/// tick runs again.
#[tokio::test]
async fn a_panicking_housekeeping_step_is_contained_and_the_next_tick_runs() {
    let pool = memory_pool().await;
    let housekeeping = housekeeping(DeadlineDb::new(pool, SHARED_LOOP_DEADLINE));
    let shutdown = CancellationToken::new();
    let mut last_tick = 0;
    tokio::time::timeout(
        Duration::from_secs(5),
        housekeeping.run_ticks(shutdown.clone(), |tick, prune_every| {
            last_tick = tick;
            let housekeeping = &housekeeping;
            let shutdown = &shutdown;
            async move {
                if tick == 1 {
                    // The panic hook is not relevant to containment.
                    std::panic::resume_unwind(Box::new("injected housekeeping panic"));
                }
                housekeeping.tick(tick, prune_every).await;
                shutdown.cancel();
            }
        }),
    )
    .await
    .expect("the tick after a panic runs and cancels the loop");
    assert_eq!(last_tick, 2);
}

/// Every connection of its pool held: the tick's reads (pipelines,
/// switches) answer at their deadline and the tick ends, so the next one
/// runs, instead of each waiting out the pool's 30 s acquire timeout. The
/// prune's writes are not in this tick: a write waits for its outcome.
#[tokio::test]
async fn a_housekeeping_ticks_reads_return_at_their_deadlines_while_every_connection_is_held() {
    let pool = memory_pool().await;
    let db = DeadlineDb::new(pool.clone(), Duration::from_millis(200));
    let housekeeping = housekeeping(db);
    let held = hold_every_connection(&pool).await;
    tokio::time::timeout(Duration::from_secs(5), housekeeping.tick(1, u64::MAX))
        .await
        .expect("the tick answers at its deadlines, not the acquire timeout");
    drop(held);
}

/// The pipeline catalog answers, but the switch query itself fails. Neither
/// a running recording nor a stream check should treat failure as off.
#[tokio::test]
async fn an_unreadable_switch_read_stops_no_recording_or_stream_check() {
    let pool = memory_pool().await;
    crate::db::create_pipeline(&pool, "pipe", "Show", "key-pipe", None, None)
        .await
        .unwrap();
    sqlx::query("DROP TABLE meta").execute(&pool).await.unwrap();
    assert_eq!(crate::db::list_pipelines(&pool).await.unwrap().len(), 1);
    let housekeeping = housekeeping(DeadlineDb::new(pool, SHARED_LOOP_DEADLINE));
    let _recording = housekeeping
        .engine
        .register_recording("pipe")
        .await
        .expect("the recording registers");
    let _stream_check = housekeeping
        .engine
        .register_stream_qa("pipe")
        .expect("the stream check registers");
    assert!(housekeeping.read_switches(1).await.is_none());
    housekeeping.tick(1, u64::MAX).await;
    assert!(
        housekeeping.engine.is_recording_active("pipe").await,
        "an unreadable switch stopped the recording"
    );
    assert!(
        housekeeping.engine.is_stream_qa_active("pipe"),
        "an unreadable switch stopped the stream check"
    );
}
