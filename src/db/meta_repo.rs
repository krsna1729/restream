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
/// one lookup per pipeline. Each prefix is a key range, which the primary-key
/// index serves. A NULL or non-text value reads as absent.
pub async fn list_meta_with_prefixes(
    pool: impl sqlx::SqliteExecutor<'_>,
    prefixes: &[&str],
) -> Result<Vec<(String, Option<String>)>, sqlx::Error> {
    if prefixes.is_empty() {
        return Ok(Vec::new());
    }
    let ranges: Vec<(&str, Option<String>)> = prefixes
        .iter()
        .map(|prefix| (*prefix, prefix_end(prefix)))
        .collect();
    let filter = ranges
        .iter()
        .map(|(_, end)| match end {
            Some(_) => "(key >= ? AND key < ?)",
            None => "substr(key, 1, length(?)) = ?",
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let sql = format!("SELECT key, value FROM meta WHERE {filter}");
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for (prefix, end) in &ranges {
        query = query.bind(*prefix);
        query = match end {
            Some(end) => query.bind(end.as_str()),
            None => query.bind(*prefix),
        };
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

/// The least key above every key starting with `prefix`: its last character
/// incremented (keys compare as UTF-8 bytes, which order like code points).
/// `None` when there is no such character to increment.
fn prefix_end(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    let last = chars.pop()?;
    chars.push(char::from_u32(u32::from(last) + 1)?);
    Some(chars.into_iter().collect())
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

    /// A prefix read returns exactly the keys starting with a prefix,
    /// including at the range's edges.
    #[tokio::test]
    async fn a_prefix_read_returns_exactly_the_keys_starting_with_a_prefix() {
        let pool = test_pool().await;
        for key in [
            "recording_enabled:a",
            "recording_enabled:",
            "recording_enabled;",
            "recording_enabled9",
            "recording_settings",
            "recording_settingt",
            "other",
        ] {
            set_meta(&pool, key, "1").await.unwrap();
        }

        let mut keys: Vec<String> =
            list_meta_with_prefixes(&pool, &["recording_enabled:", "recording_settings"])
                .await
                .unwrap()
                .into_iter()
                .map(|(key, _)| key)
                .collect();
        keys.sort();

        assert_eq!(
            keys,
            [
                "recording_enabled:",
                "recording_enabled:a",
                "recording_settings"
            ]
        );
    }
}
