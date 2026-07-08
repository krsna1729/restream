use sqlx::{AssertSqlSafe, SqlitePool};

/// Batch-insert log entries. Called by the DbLayer drain task every 100 ms.
pub async fn append_app_log_batch(
    pool: &SqlitePool,
    entries: &[crate::logging::types::AppLogEntry],
) -> Result<(), sqlx::Error> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for e in entries {
        sqlx::query(
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
    }
    tx.commit().await?;
    Ok(())
}

/// General query for `/api/v1/logs` — supports level, target, scope, pipeline_id,
/// event_class, prefix (message LIKE), time range, limit, order.
pub async fn list_app_logs(
    pool: &SqlitePool,
    filters: &crate::logging::types::AppLogFilters,
) -> Result<Vec<crate::logging::types::AppLogRow>, sqlx::Error> {
    let mut clauses: Vec<String> = vec![];

    let levels: &[&str] = match filters.level.as_deref().unwrap_or("info") {
        "error" => &["ERROR"],
        "warn" => &["ERROR", "WARN"],
        "debug" => &["ERROR", "WARN", "INFO", "DEBUG"],
        _ => &["ERROR", "WARN", "INFO"],
    };
    let placeholders = levels.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    clauses.push(format!("level IN ({})", placeholders));

    if filters.target.is_some() {
        clauses.push("target LIKE ?".to_string());
    }
    if filters.after_id.is_some() {
        clauses.push("id > ?".to_string());
    }
    match filters.scope.as_deref() {
        Some("restream") => {
            clauses.push("pipeline_id IS NULL".to_string());
            clauses.push("output_id IS NULL".to_string());
        }
        Some("pipeline") => {
            clauses.push("pipeline_id IS NOT NULL".to_string());
            clauses.push("output_id IS NULL".to_string());
        }
        Some("output") => {
            clauses.push("output_id IS NOT NULL".to_string());
        }
        _ => {}
    }
    if filters.pipeline_id.is_some() {
        clauses.push("pipeline_id = ?".to_string());
    }
    if filters.output_id.is_some() {
        clauses.push("output_id = ?".to_string());
    }
    if filters.event_class.is_some() {
        clauses.push("event_class = ?".to_string());
    }
    if filters.since.is_some() {
        clauses.push("ts >= ?".to_string());
    }
    if filters.until.is_some() {
        clauses.push("ts < ?".to_string());
    }

    if let Some(ref prefix) = filters.prefix {
        let parts: Vec<_> = prefix
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if !parts.is_empty() {
            let px: Vec<_> = parts.iter().map(|_| "message LIKE ?".to_string()).collect();
            clauses.push(format!("({})", px.join(" OR ")));
        }
    }

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
    let sql = format!(
        "SELECT id, ts, level, target, message, fields, pipeline_id, output_id, event_type \
         FROM app_logs {} ORDER BY ts {}, id {} LIMIT {}",
        where_clause, order, order, limit
    );

    let mut q = sqlx::query_as::<_, crate::logging::types::AppLogRow>(AssertSqlSafe(sql));
    for l in levels {
        q = q.bind(l);
    }
    if let Some(ref t) = filters.target {
        q = q.bind(format!("{}%", t));
    }
    if let Some(after_id) = filters.after_id {
        q = q.bind(after_id);
    }
    if let Some(ref p) = filters.pipeline_id {
        q = q.bind(p);
    }
    if let Some(ref o) = filters.output_id {
        q = q.bind(o);
    }
    if let Some(ref ec) = filters.event_class {
        q = q.bind(ec);
    }
    if let Some(ref s) = filters.since {
        q = q.bind(s);
    }
    if let Some(ref u) = filters.until {
        q = q.bind(u);
    }
    if let Some(ref prefix) = filters.prefix {
        for p in prefix.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            q = q.bind(format!("{}%", p));
        }
    }

    q.fetch_all(pool).await
}

pub async fn delete_app_logs_older_than(pool: &SqlitePool, days: i64) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM app_logs WHERE ts < datetime('now', ?)")
        .bind(format!("-{} days", days))
        .execute(pool)
        .await?;
    Ok(())
}
