use sqlx::{AssertSqlSafe, SqlitePool};

use super::log_types::{AppLogEntry, AppLogFilters, AppLogRow};

/// Batch-insert log entries. Called by the DbLayer drain task every 100 ms.
pub async fn append_app_log_batch(
    pool: &SqlitePool,
    entries: &[AppLogEntry],
) -> Result<(), sqlx::Error> {
    append_app_log_batch_returning(pool, entries).await?;
    Ok(())
}

/// Batch-insert log entries and return their persisted rows with stable IDs.
/// The logging drain uses this to publish SSE only after the transaction commits.
pub async fn append_app_log_batch_returning(
    pool: &SqlitePool,
    entries: &[AppLogEntry],
) -> Result<Vec<AppLogRow>, sqlx::Error> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    super::with_busy_retry(|| async {
        let mut tx = pool.begin().await?;
        let mut rows = Vec::with_capacity(entries.len());
        for e in entries {
            let result = sqlx::query(
                "INSERT INTO app_logs (ts, level, target, message, fields, pipeline_id, output_id, event_type, event_class)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&e.ts)
            .bind(&e.level)
            .bind(&e.target)
            .bind(&e.message)
            .bind(&e.fields)
            .bind(&e.pipeline_id)
            .bind(&e.output_id)
            .bind(&e.event_type)
            .bind(&e.event_class)
            .execute(&mut *tx)
            .await?;
            rows.push(AppLogRow {
                id: result.last_insert_rowid(),
                ts: e.ts.clone(),
                level: e.level.clone(),
                target: e.target.clone(),
                message: e.message.clone(),
                fields: e.fields.clone(),
                pipeline_id: e.pipeline_id.clone(),
                output_id: e.output_id.clone(),
                event_type: e.event_type.clone(),
                event_class: e.event_class.clone(),
            });
        }
        tx.commit().await?;
        Ok(rows)
    })
    .await
}

/// One bound value of an app-log filter.
enum LogParam<'a> {
    Text(&'a str),
    Owned(String),
    Id(i64),
}

/// The `WHERE` conditions for `filters`, each pushed with the values its
/// placeholders bind, so the two cannot fall out of order.
fn app_log_conditions(filters: &AppLogFilters) -> (Vec<String>, Vec<LogParam<'_>>) {
    let mut clauses: Vec<String> = vec![];
    let mut params = vec![];

    let levels: &[&'static str] = match filters.level.as_deref().unwrap_or("info") {
        "error" => &["ERROR"],
        "warn" => &["ERROR", "WARN"],
        "debug" => &["ERROR", "WARN", "INFO", "DEBUG"],
        _ => &["ERROR", "WARN", "INFO"],
    };
    let placeholders = levels.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    clauses.push(format!("level IN ({})", placeholders));
    params.extend(levels.iter().map(|level| LogParam::Text(level)));

    if let Some(target) = &filters.target {
        clauses.push("target LIKE ?".to_string());
        params.push(LogParam::Owned(format!("{target}%")));
    }
    if let Some(after_id) = filters.after_id {
        clauses.push("id > ?".to_string());
        params.push(LogParam::Id(after_id));
    }
    clauses.extend(scope_conditions(filters.scope.as_deref()).map(str::to_string));
    for (column, value) in [
        ("pipeline_id = ?", &filters.pipeline_id),
        ("output_id = ?", &filters.output_id),
        ("event_class = ?", &filters.event_class),
        ("ts >= ?", &filters.since),
        ("ts < ?", &filters.until),
    ] {
        if let Some(value) = value {
            clauses.push(column.to_string());
            params.push(LogParam::Text(value));
        }
    }

    if let Some(prefix) = &filters.prefix {
        let parts: Vec<_> = prefix
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if !parts.is_empty() {
            let px: Vec<_> = parts.iter().map(|_| "message LIKE ?").collect();
            clauses.push(format!("({})", px.join(" OR ")));
            params.extend(parts.iter().map(|part| LogParam::Owned(format!("{part}%"))));
        }
    }
    (clauses, params)
}

/// The row-origin conditions a `scope` filter adds; none bind a value.
fn scope_conditions(scope: Option<&str>) -> impl Iterator<Item = &'static str> {
    let conditions: &[&str] = match scope {
        Some("restream") => &["pipeline_id IS NULL", "output_id IS NULL"],
        Some("pipeline") => &["pipeline_id IS NOT NULL", "output_id IS NULL"],
        Some("output") => &["output_id IS NOT NULL"],
        _ => &[],
    };
    conditions.iter().copied()
}

/// General query for `/api/v1/logs` — supports level, target, scope, pipeline_id,
/// event_class, prefix (message LIKE), time range, limit, order.
pub async fn list_app_logs(
    pool: &SqlitePool,
    filters: &AppLogFilters,
) -> Result<Vec<AppLogRow>, sqlx::Error> {
    let (clauses, params) = app_log_conditions(filters);

    let order = if filters.order.as_deref() == Some("asc") {
        "ASC"
    } else {
        "DESC"
    };
    let limit = filters.limit.unwrap_or(200).clamp(1, 1000);
    let where_clause = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    // `after_id` is an ID cursor, so cursor-backed reads must use the same
    // monotonic key regardless of event timestamps supplied by callers.
    let order_clause = if filters.after_id.is_some() {
        "id ASC".to_string()
    } else {
        format!("ts {order}, id {order}")
    };
    let sql = format!(
        "SELECT id, ts, level, target, message, fields, pipeline_id, output_id, event_type, event_class \
         FROM app_logs {} ORDER BY {} LIMIT {}",
        where_clause, order_clause, limit
    );

    let mut q = sqlx::query_as::<_, AppLogRow>(AssertSqlSafe(sql));
    for param in params {
        q = match param {
            LogParam::Text(value) => q.bind(value),
            LogParam::Owned(value) => q.bind(value),
            LogParam::Id(value) => q.bind(value),
        };
    }
    q.fetch_all(pool).await
}

/// Rows one retention statement deletes at most: each batch is its own short
/// write transaction, so the app-log drain and API writes take the write
/// lock between batches instead of waiting behind one long `DELETE`.
pub const APP_LOG_RETENTION_BATCH: u32 = 1000;

/// Deletes app-log rows older than `days`, at most `batch` rows per
/// statement, each a [`super::DeadlineDb::write`] awaited to its outcome.
/// Returns the rows each statement deleted; stops after a short batch. A
/// failed statement ends this prune with its error; the next prune carries
/// on.
pub async fn prune_app_logs(
    db: &super::DeadlineDb,
    days: i64,
    batch: u32,
) -> Result<Vec<u64>, sqlx::Error> {
    let cutoff = format!("-{days} days");
    let mut batches = Vec::new();
    loop {
        let cutoff = cutoff.clone();
        let deleted = db
            .write(move |pool| async move {
                sqlx::query(
                    "DELETE FROM app_logs WHERE id IN (
                         SELECT id FROM app_logs WHERE ts < datetime('now', ?) ORDER BY ts LIMIT ?
                     )",
                )
                .bind(cutoff)
                .bind(batch)
                .execute(&pool)
                .await
                .map(|result| result.rows_affected())
            })
            .await?;
        batches.push(deleted);
        if deleted < u64::from(batch) {
            return Ok(batches);
        }
    }
}
