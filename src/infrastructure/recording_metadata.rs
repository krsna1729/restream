//! SQLite-backed recording metadata reporter wiring.

use crate::domain::ids::RecordingId;
use crate::domain::state::RecordingPhase;
use crate::media::recording::{RecordingMetadataEvent, RecordingMetadataReporter};
use sqlx::SqlitePool;
use tokio::sync::mpsc;

pub fn spawn_recording_metadata_reporter(pool: SqlitePool) -> RecordingMetadataReporter {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            if let Err(error) = persist_recording_metadata_event(&pool, event).await {
                tracing::warn!(
                    err = %error,
                    "failed to persist recording metadata event"
                );
            }
        }
    });
    RecordingMetadataReporter::new(sender)
}

async fn persist_recording_metadata_event(
    pool: &SqlitePool,
    event: RecordingMetadataEvent,
) -> Result<(), sqlx::Error> {
    match event {
        RecordingMetadataEvent::Started {
            recording_id,
            pipeline_id,
            started_at,
            temp_path,
        } => {
            crate::db::create_recording(
                pool,
                &RecordingId::new(recording_id),
                &pipeline_id,
                &started_at,
                Some(&temp_path),
                None,
            )
            .await?;
        }
        RecordingMetadataEvent::Finalized {
            recording_id,
            ended_at,
            final_path,
        } => {
            crate::db::finalize_recording(
                pool,
                &RecordingId::new(recording_id),
                &ended_at,
                &final_path,
            )
            .await?;
        }
        RecordingMetadataEvent::Failed {
            recording_id,
            error,
        } => {
            crate::db::update_recording_status(
                pool,
                &RecordingId::new(recording_id),
                RecordingPhase::Failed,
                Some(&error),
            )
            .await?;
        }
    }
    Ok(())
}
