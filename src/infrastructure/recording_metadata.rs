//! The recording catalog writer: drains the recorders' coalescing queue
//! ([`restream_engine::recording::RecordingMetadataQueue`]) into SQLite.
//!
//! It is supervised: a panic restarts it after the restart backoff, and the
//! update it was writing is restored first, so the latest state of every
//! recording still lands. Writes go through the lifecycle [`DeadlineDb`]
//! and are awaited to their outcome here, in the writer: a stalled database
//! holds only this task, while recorders keep reporting into the queue,
//! which holds at most one entry per recording.

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
    spawn_writer(queue, db, persist_recording_metadata);
    reporter
}

/// The supervised writer draining `queue` with its persist step;
/// production's is [`persist_recording_metadata`].
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

async fn run_writer<P, Fut>(queue: RecordingMetadataQueue, db: DeadlineDb, persist: P)
where
    P: Fn(DeadlineDb, RecordingMetadataUpdate) -> Fut,
    Fut: Future<Output = Result<(), sqlx::Error>>,
{
    // A previous run that panicked mid-write left its update unacknowledged.
    queue.restore();
    while let Some(update) = queue.next().await {
        let recording_id = update.recording_id.clone();
        if let Err(error) = persist(db.clone(), update).await {
            tracing::warn!(
                recording_id = %recording_id,
                err = %error,
                "failed to persist recording metadata"
            );
        }
    }
}

/// Writes one recording's update in one write task: its row first (already
/// there when a restarted writer replays it), then its end.
async fn persist_recording_metadata(
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
