//! Recording metadata persistence.
//!
//! Grounds `RecordingId` and `RecordingPhase` (defined in `domain::ids` and
//! `domain::state`) in actual SQLite storage. A recording row is created at
//! ingest start and updated as the lifecycle progresses through
//! `recording → finalizing → done` (or `failed`).

use sqlx::SqlitePool;

use crate::domain::ids::RecordingId;
use crate::domain::state::RecordingPhase;

// ─── Row type ────────────────────────────────────────────────────────────────

/// A single row from the `recordings` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RecordingRow {
    pub recording_id: String,
    pub pipeline_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: String,
    pub temp_path: Option<String>,
    pub final_path: Option<String>,
    pub codec_summary: Option<String>,
    pub error: Option<String>,
}

impl RecordingRow {
    /// Parse the `status` column into a typed `RecordingPhase`.
    pub fn phase(&self) -> RecordingPhase {
        RecordingPhase::from(self.status.as_str())
    }
}

// ─── Write operations ─────────────────────────────────────────────────────────

/// Insert a new recording row at the start of a recording session.
pub async fn create_recording(
    pool: &SqlitePool,
    recording_id: &RecordingId,
    pipeline_id: &str,
    started_at: &str,
    temp_path: Option<&str>,
    codec_summary: Option<&str>,
) -> Result<RecordingRow, sqlx::Error> {
    let id = recording_id.as_str();
    sqlx::query(
        "INSERT INTO recordings (recording_id, pipeline_id, started_at, status, temp_path, codec_summary)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(pipeline_id)
    .bind(started_at)
    .bind(RecordingPhase::Recording.as_str())
    .bind(temp_path)
    .bind(codec_summary)
    .execute(pool)
    .await?;

    get_recording(pool, recording_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

/// Transition the recording to a new phase (e.g. `Finalizing`, `Failed`).
pub async fn update_recording_status(
    pool: &SqlitePool,
    recording_id: &RecordingId,
    phase: RecordingPhase,
    error: Option<&str>,
) -> Result<Option<RecordingRow>, sqlx::Error> {
    let id = recording_id.as_str();
    let rows = sqlx::query("UPDATE recordings SET status = ?, error = ? WHERE recording_id = ?")
        .bind(phase.as_str())
        .bind(error)
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();

    if rows > 0 {
        get_recording(pool, recording_id).await
    } else {
        Ok(None)
    }
}

/// Mark the recording as `done`, storing the final file path and end time.
pub async fn finalize_recording(
    pool: &SqlitePool,
    recording_id: &RecordingId,
    ended_at: &str,
    final_path: &str,
) -> Result<Option<RecordingRow>, sqlx::Error> {
    let id = recording_id.as_str();
    let rows = sqlx::query(
        "UPDATE recordings SET status = ?, ended_at = ?, final_path = ? WHERE recording_id = ?",
    )
    .bind(RecordingPhase::Ready.as_str())
    .bind(ended_at)
    .bind(final_path)
    .bind(id)
    .execute(pool)
    .await?
    .rows_affected();

    if rows > 0 {
        get_recording(pool, recording_id).await
    } else {
        Ok(None)
    }
}

// ─── Read operations ──────────────────────────────────────────────────────────

/// Fetch a single recording by its typed ID.
pub async fn get_recording(
    pool: &SqlitePool,
    recording_id: &RecordingId,
) -> Result<Option<RecordingRow>, sqlx::Error> {
    sqlx::query_as::<_, RecordingRow>(
        "SELECT recording_id, pipeline_id, started_at, ended_at, status,
                temp_path, final_path, codec_summary, error
         FROM recordings WHERE recording_id = ?",
    )
    .bind(recording_id.as_str())
    .fetch_optional(pool)
    .await
}

/// List all recordings for a pipeline, newest first.
pub async fn list_recordings_for_pipeline(
    pool: &SqlitePool,
    pipeline_id: &str,
) -> Result<Vec<RecordingRow>, sqlx::Error> {
    sqlx::query_as::<_, RecordingRow>(
        "SELECT recording_id, pipeline_id, started_at, ended_at, status,
                temp_path, final_path, codec_summary, error
         FROM recordings WHERE pipeline_id = ?
         ORDER BY started_at DESC",
    )
    .bind(pipeline_id)
    .fetch_all(pool)
    .await
}

/// List recordings by phase (e.g. to resume in-progress recordings after restart).
pub async fn list_recordings_by_status(
    pool: &SqlitePool,
    phase: RecordingPhase,
) -> Result<Vec<RecordingRow>, sqlx::Error> {
    sqlx::query_as::<_, RecordingRow>(
        "SELECT recording_id, pipeline_id, started_at, ended_at, status,
                temp_path, final_path, codec_summary, error
         FROM recordings WHERE status = ?
         ORDER BY started_at DESC",
    )
    .bind(phase.as_str())
    .fetch_all(pool)
    .await
}

/// Delete a recording row by ID.
pub async fn delete_recording(
    pool: &SqlitePool,
    recording_id: &RecordingId,
) -> Result<bool, sqlx::Error> {
    let rows = sqlx::query("DELETE FROM recordings WHERE recording_id = ?")
        .bind(recording_id.as_str())
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows > 0)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_pipeline, create_pool, setup_database_schema};

    async fn test_pool() -> SqlitePool {
        let pool = create_pool("sqlite::memory:").await.unwrap();
        setup_database_schema(&pool).await.unwrap();
        pool
    }

    async fn make_pipeline(pool: &SqlitePool) {
        create_pipeline(pool, "p1", "Pipeline", "sk1", None, None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn create_and_get_recording() {
        let pool = test_pool().await;
        make_pipeline(&pool).await;

        let id = RecordingId::from("rec-1");
        let row = create_recording(
            &pool,
            &id,
            "p1",
            "2026-01-01T00:00:00Z",
            Some("/tmp/r.ts"),
            None,
        )
        .await
        .unwrap();

        assert_eq!(row.recording_id, "rec-1");
        assert_eq!(row.pipeline_id, "p1");
        assert_eq!(row.phase(), RecordingPhase::Recording);
        assert_eq!(row.temp_path.as_deref(), Some("/tmp/r.ts"));
    }

    #[tokio::test]
    async fn finalize_sets_done_and_final_path() {
        let pool = test_pool().await;
        make_pipeline(&pool).await;

        let id = RecordingId::from("rec-2");
        create_recording(
            &pool,
            &id,
            "p1",
            "2026-01-01T00:00:00Z",
            Some("/tmp/r.ts"),
            None,
        )
        .await
        .unwrap();

        let row = finalize_recording(&pool, &id, "2026-01-01T01:00:00Z", "/recordings/r.mp4")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(row.phase(), RecordingPhase::Ready);
        assert_eq!(row.final_path.as_deref(), Some("/recordings/r.mp4"));
        assert!(row.ended_at.is_some());
    }

    #[tokio::test]
    async fn update_status_to_failed() {
        let pool = test_pool().await;
        make_pipeline(&pool).await;

        let id = RecordingId::from("rec-3");
        create_recording(&pool, &id, "p1", "2026-01-01T00:00:00Z", None, None)
            .await
            .unwrap();

        let row = update_recording_status(&pool, &id, RecordingPhase::Failed, Some("disk full"))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(row.phase(), RecordingPhase::Failed);
        assert_eq!(row.error.as_deref(), Some("disk full"));
    }

    #[tokio::test]
    async fn list_recordings_for_pipeline_returns_newest_first() {
        let pool = test_pool().await;
        make_pipeline(&pool).await;

        create_recording(
            &pool,
            &RecordingId::from("rec-a"),
            "p1",
            "2026-01-01T00:00:00Z",
            None,
            None,
        )
        .await
        .unwrap();
        create_recording(
            &pool,
            &RecordingId::from("rec-b"),
            "p1",
            "2026-01-02T00:00:00Z",
            None,
            None,
        )
        .await
        .unwrap();

        let rows = list_recordings_for_pipeline(&pool, "p1").await.unwrap();
        assert_eq!(rows.len(), 2);
        // newest first
        assert_eq!(rows[0].recording_id, "rec-b");
    }

    #[tokio::test]
    async fn delete_recording_removes_row() {
        let pool = test_pool().await;
        make_pipeline(&pool).await;

        let id = RecordingId::from("rec-del");
        create_recording(&pool, &id, "p1", "2026-01-01T00:00:00Z", None, None)
            .await
            .unwrap();

        assert!(delete_recording(&pool, &id).await.unwrap());
        assert!(get_recording(&pool, &id).await.unwrap().is_none());
    }
}
