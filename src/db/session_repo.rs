use sqlx::{Row, SqlitePool};
use std::time::SystemTime;

pub async fn create_session(pool: &SqlitePool, token: &str, ts: i64) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT OR REPLACE INTO sessions (token, created_at) VALUES (?, ?)")
        .bind(token)
        .bind(ts)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_session(pool: &SqlitePool, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE token = ?")
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_sessions(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    let rows = sqlx::query("SELECT token FROM sessions")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|r| r.get::<String, _>(0)).collect())
}

pub async fn prune_expired_sessions(pool: &SqlitePool, max_age_ms: i64) -> Result<(), sqlx::Error> {
    let expire_limit = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
        - max_age_ms;

    sqlx::query("DELETE FROM sessions WHERE created_at < ?")
        .bind(expire_limit)
        .execute(pool)
        .await?;
    Ok(())
}
