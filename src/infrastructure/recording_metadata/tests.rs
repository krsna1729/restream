use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::*;
use crate::db::SHARED_LOOP_DEADLINE;
use crate::infrastructure::bootstrap::test_support::{
    file_pools, hold_write_lock, memory_pool, release_write_lock,
};
use restream_engine::recording::RecordingMetadataEvent;

const PIPELINE: &str = "pipeline-meta";

async fn with_pipeline(pool: &sqlx::SqlitePool) {
    crate::db::create_pipeline(pool, PIPELINE, "Pipeline", "stream-key", None, None)
        .await
        .unwrap();
}

fn started(id: &str) -> RecordingMetadataEvent {
    RecordingMetadataEvent::Started {
        recording_id: id.to_string(),
        pipeline_id: PIPELINE.to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        temp_path: format!("/tmp/{id}.ts"),
    }
}

fn finalized(id: &str, n: usize) -> RecordingMetadataEvent {
    RecordingMetadataEvent::Finalized {
        recording_id: id.to_string(),
        ended_at: format!("2026-01-01T00:00:{n:02}Z"),
        final_path: format!("/tmp/{id}-{n}.mp4"),
    }
}

async fn wait_for_row(
    pool: &sqlx::SqlitePool,
    id: &str,
    done: impl Fn(&crate::db::RecordingRow) -> bool,
) -> Option<crate::db::RecordingRow> {
    let id = RecordingId::new(id);
    for _ in 0..250 {
        if let Some(row) = crate::db::get_recording(pool, &id).await.unwrap()
            && done(&row)
        {
            return Some(row);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

/// Another writer holds SQLite's write lock: the writer waits on its
/// recording's row while the recorder keeps reporting. A hundred updates for
/// that recording stay one queue entry, and once the lock is released the
/// row holds the last of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn updates_behind_a_held_write_lock_stay_one_entry_and_the_latest_lands() {
    let (api, lifecycle, dir) = file_pools("recording-metadata").await;
    with_pipeline(&api).await;
    let (reporter, queue) = recording_metadata_channel();
    let writer = spawn_writer(
        queue.clone(),
        DeadlineDb::new(lifecycle, SHARED_LOOP_DEADLINE),
        persist_recording_metadata,
    );
    let lock = hold_write_lock(&api).await;

    reporter.report(started("rec-held"));
    // The writer took the start and waits on the lock.
    for _ in 0..100 {
        if queue.pending_len() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(queue.pending_len(), 0, "the writer took the start");
    for n in 0..100 {
        if n % 2 == 0 {
            reporter.report(RecordingMetadataEvent::Failed {
                recording_id: "rec-held".to_string(),
                error: format!("attempt {n}"),
            });
        } else {
            reporter.report(finalized("rec-held", n));
        }
    }
    let queued = queue.pending_len();

    release_write_lock(lock).await;
    let row = wait_for_row(&api, "rec-held", |row| {
        row.final_path.as_deref() == Some("/tmp/rec-held-99.mp4")
    })
    .await;
    drop(reporter);
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
    let _ = std::fs::remove_dir_all(dir);

    assert_eq!(
        queued, 1,
        "a hundred updates for one recording are one entry"
    );
    let row = row.expect("the latest update landed once the lock was released");
    assert_eq!(row.status, RecordingPhase::Ready.as_str());
    assert_eq!(row.temp_path.as_deref(), Some("/tmp/rec-held.ts"));
    assert_eq!(row.error, None);
}

/// The writer panics while writing a recording's start and end: it is
/// restarted after the restart backoff and writes them, so the recording
/// does not stay `recording` in the catalog.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_writer_is_restarted_and_its_update_still_lands() {
    let pool = memory_pool().await;
    with_pipeline(&pool).await;
    let panic_next = Arc::new(AtomicBool::new(true));
    let (reporter, queue) = recording_metadata_channel();
    let _writer = spawn_writer(
        queue,
        DeadlineDb::new(pool.clone(), SHARED_LOOP_DEADLINE),
        move |db, update| {
            let panic_now = panic_next.swap(false, Ordering::SeqCst);
            async move {
                if panic_now {
                    // `resume_unwind` skips the panic hook: quiet logs.
                    std::panic::resume_unwind(Box::new("injected recording metadata writer panic"));
                }
                persist_recording_metadata(db, update).await
            }
        },
    );

    reporter.report(started("rec-panic"));
    reporter.report(finalized("rec-panic", 7));

    let row = wait_for_row(&pool, "rec-panic", |row| row.final_path.is_some())
        .await
        .expect("the restarted writer wrote the update the panicked one held");
    assert_eq!(row.status, RecordingPhase::Ready.as_str());
    assert_eq!(row.pipeline_id, PIPELINE);
    assert_eq!(row.temp_path.as_deref(), Some("/tmp/rec-panic.ts"));
    assert_eq!(row.final_path.as_deref(), Some("/tmp/rec-panic-7.mp4"));
}
