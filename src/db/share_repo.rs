//! Share links: public watch links to one pipeline's HLS preview.

use sqlx::{Row, SqlitePool};

#[derive(Debug, Clone)]
pub struct ShareLinkRecord {
    pub id: String,
    pub pipeline_id: String,
    pub label: String,
    pub token: String,
    pub created_at_ms: i64,
    pub expires_at_ms: Option<i64>,
    pub views: i64,
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<ShareLinkRecord>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, pipeline_id, label, token, created_at, expires_at, views FROM share_links ORDER BY created_at, id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| ShareLinkRecord {
            id: row.get(0),
            pipeline_id: row.get(1),
            label: row.get(2),
            token: row.get(3),
            created_at_ms: row.get(4),
            expires_at_ms: row.get(5),
            views: row.get(6),
        })
        .collect())
}

pub async fn insert(pool: &SqlitePool, link: &ShareLinkRecord) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO share_links (id, pipeline_id, label, token, created_at, expires_at, views)
         VALUES (?, ?, ?, ?, ?, ?, 0)",
    )
    .bind(&link.id)
    .bind(&link.pipeline_id)
    .bind(&link.label)
    .bind(&link.token)
    .bind(link.created_at_ms)
    .bind(link.expires_at_ms)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM share_links WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// Removes every link to a pipeline (it was deleted).
pub async fn delete_for_pipeline(pool: &SqlitePool, pipeline_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM share_links WHERE pipeline_id = ?")
        .bind(pipeline_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn count_view(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE share_links SET views = views + 1 WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
