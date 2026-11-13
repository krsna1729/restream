use sqlx::{Row, SqlitePool};

pub async fn get_meta(
    pool: impl sqlx::SqliteExecutor<'_>,
    key: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query("SELECT value FROM meta WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    // `value` is nullable in the schema, and a restored or hand-edited database
    // can hold any type: decode fallibly so such a row is an error or `None`,
    // never a panic in a startup or reconciler caller.
    Ok(row
        .map(|r| r.try_get::<Option<String>, _>(0))
        .transpose()?
        .flatten())
}

/// Every meta row whose key starts with one of `prefixes`, in one statement:
/// the per-pipeline switches a periodic loop needs, read once rather than
/// one lookup per pipeline. A NULL or non-text value reads as absent.
pub async fn list_meta_with_prefixes(
    pool: impl sqlx::SqliteExecutor<'_>,
    prefixes: &[&str],
) -> Result<Vec<(String, Option<String>)>, sqlx::Error> {
    if prefixes.is_empty() {
        return Ok(Vec::new());
    }
    let filter = vec!["substr(key, 1, length(?)) = ?"; prefixes.len()].join(" OR ");
    let sql = format!("SELECT key, value FROM meta WHERE {filter}");
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for prefix in prefixes {
        query = query.bind(*prefix).bind(*prefix);
    }
    query
        .fetch_all(pool)
        .await?
        .iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>(0)?,
                row.try_get::<Option<String>, _>(1).ok().flatten(),
            ))
        })
        .collect()
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

pub async fn get_ingest_host(
    pool: impl sqlx::SqliteExecutor<'_>,
) -> Result<Option<String>, sqlx::Error> {
    get_meta(pool, "ingest_host").await
}

pub async fn set_ingest_host(pool: &SqlitePool, host: &str) -> Result<String, sqlx::Error> {
    set_meta(pool, "ingest_host", host.trim()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_pool, setup_database_schema};

    async fn test_pool() -> SqlitePool {
        let pool = create_pool("sqlite::memory:").await.unwrap();
        setup_database_schema(&pool).await.unwrap();
        pool
    }

    /// A NULL or non-text `meta.value` (the column is nullable, and SQLite
    /// stores any type) reads as absent or as an error, never a panic.
    #[tokio::test]
    async fn a_null_or_mistyped_meta_value_is_not_a_panic() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO meta (key, value) VALUES ('ingest_host', NULL), ('k', x'00ff')")
            .execute(&pool)
            .await
            .unwrap();

        let null = tokio::spawn({
            let pool = pool.clone();
            async move { get_ingest_host(&pool).await }
        })
        .await
        .expect("reading a NULL meta value does not panic");
        assert_eq!(null.unwrap(), None);

        let mistyped = tokio::spawn({
            let pool = pool.clone();
            async move { get_meta(&pool, "k").await }
        })
        .await
        .expect("reading a non-text meta value does not panic");
        assert!(mistyped.is_err(), "{mistyped:?}");
    }
}
