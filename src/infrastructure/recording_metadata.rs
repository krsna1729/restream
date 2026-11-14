//! The recording catalog writer: drains the recorders' coalescing queue
//! ([`restream_engine::recording::RecordingMetadataQueue`]) into SQLite.
//!
//! It is supervised: a panic restarts it after the restart backoff, and the
//! update it was writing is restored first, so the latest state of every
//! recording still lands. A write that may succeed later (busy, I/O, a
//! panic in the write task) puts its update back and is retried with a
//! 100 ms..30 s backoff; one that never can (a constraint or decode error)
//! is dropped, logged and counted ([`dropped_updates`]). Writes go through
//! the lifecycle [`DeadlineDb`] and are awaited to their outcome here, in
//! the writer: a stalled database holds only this task, while recorders
//! keep reporting into the queue, which holds at most one entry per
//! recording.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use restream_domain::domain::ids::RecordingId;
use restream_domain::domain::state::RecordingPhase;
use restream_engine::recording::{
    RecordingEnd, RecordingMetadataQueue, RecordingMetadataReporter, RecordingMetadataUpdate,
    recording_metadata_channel,
};

use crate::db::DeadlineDb;
use crate::infrastructure::bootstrap::supervised_task::spawn_supervised;

/// Starts the supervised writer on `db` and returns the reporter recorders
/// use. The writer ends once every reporter clone is gone and the queue is
/// drained.
pub fn spawn_recording_metadata_reporter(db: DeadlineDb) -> RecordingMetadataReporter {
    let (reporter, queue) = recording_metadata_channel();
    spawn_writer(queue, db, persist_recording_metadata_event);
    reporter
}

/// The supervised writer draining `queue` with its persist step;
/// production's is [`persist_recording_metadata_event`].
fn spawn_writer<P, Fut>(
    queue: RecordingMetadataQueue,
    db: DeadlineDb,
    persist: P,
) -> tokio::task::JoinHandle<()>
where
    P: Fn(DeadlineDb, RecordingMetadataUpdate) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<(), sqlx::Error>> + Send + 'static,
{
    spawn_supervised("recording_metadata_writer", move || {
        run_writer(queue.clone(), db.clone(), persist.clone())
    })
}

/// Backoff after a write that may succeed later, doubled per failure.
const RETRY_MIN: Duration = Duration::from_millis(100);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// Updates dropped because their write can never succeed (a constraint or
/// decode error); each is logged too.
static DROPPED_UPDATES: AtomicU64 = AtomicU64::new(0);

/// Recording catalog updates dropped since start.
pub fn dropped_updates() -> u64 {
    DROPPED_UPDATES.load(Ordering::Relaxed)
}

/// Whether a failed write may succeed if tried again: the database busy or
/// locked, an I/O or pool error, or the write task panicking
/// (`DeadlineError::Aborted` arrives as I/O).
fn is_transient(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(error) => matches!(
            error.code().as_deref().and_then(|code| code.parse::<i32>().ok()),
            // SQLITE_BUSY, SQLITE_LOCKED and their extended codes.
            Some(code) if code & 0xff == 5 || code & 0xff == 6
        ),
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
        _ => false,
    }
}

async fn run_writer<P, Fut>(queue: RecordingMetadataQueue, db: DeadlineDb, persist: P)
where
    P: Fn(DeadlineDb, RecordingMetadataUpdate) -> Fut,
    Fut: Future<Output = Result<(), sqlx::Error>>,
{
    // A previous run that panicked mid-write left its update unacknowledged.
    queue.restore();
    let mut retry = RETRY_MIN;
    while let Some(update) = queue.next().await {
        let recording_id = update.recording_id.clone();
        match persist(db.clone(), update).await {
            Ok(()) => retry = RETRY_MIN,
            Err(error) if is_transient(&error) => {
                tracing::warn!(
                    recording_id = %recording_id,
                    err = %error,
                    retry_in_ms = retry.as_millis() as u64,
                    "recording metadata write failed; retried"
                );
                // Back first, merged under anything reported since.
                queue.restore();
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(RETRY_MAX);
            }
            Err(error) => {
                DROPPED_UPDATES.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    recording_id = %recording_id,
                    err = %error,
                    "recording metadata update dropped: its write cannot succeed"
                );
            }
        }
    }
}

/// Writes one recording's update in one write task: its row first (already
/// there when a restarted writer replays it), then its end.
async fn persist_recording_metadata_event(
    db: DeadlineDb,
    update: RecordingMetadataUpdate,
) -> Result<(), sqlx::Error> {
    db.write(move |pool| async move {
        let id = RecordingId::new(update.recording_id);
        if let Some(started) = update.started {
            match crate::db::create_recording(
                &pool,
                &id,
                &started.pipeline_id,
                &started.started_at,
                Some(&started.temp_path),
                None,
            )
            .await
            {
                Ok(_) => {}
                Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {}
                Err(error) => return Err(error),
            }
        }
        match update.end {
            Some(RecordingEnd::Finalized {
                ended_at,
                final_path,
            }) => {
                crate::db::finalize_recording(&pool, &id, &ended_at, &final_path).await?;
            }
            Some(RecordingEnd::Failed { error }) => {
                crate::db::update_recording_status(
                    &pool,
                    &id,
                    RecordingPhase::Failed,
                    Some(&error),
                )
                .await?;
            }
            None => {}
        }
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests;
