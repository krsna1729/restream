use sqlx::{Row, SqlitePool};

pub async fn get_meta(pool: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query("SELECT value FROM meta WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.get::<String, _>(0)))
}

pub async fn set_meta(pool: &SqlitePool, key: &str, value: &str) -> Result<String, sqlx::Error> {
    sqlx::query(
        "INSERT INTO meta (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(value.to_string())
}

pub async fn get_ingest_host(pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    get_meta(pool, "ingest_host").await
}

pub async fn set_ingest_host(pool: &SqlitePool, host: &str) -> Result<String, sqlx::Error> {
    set_meta(pool, "ingest_host", host.trim()).await
}
