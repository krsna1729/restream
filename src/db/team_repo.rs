//! Team members, the member behind each session, and the audit log.

use sqlx::{Row, SqlitePool};

/// One stored member row: id, username, role, created_at, disabled, password hash.
pub type UserRow = (String, String, String, i64, bool, String);

/// The audit log keeps this many most recent entries.
pub const AUDIT_LOG_CAP: i64 = 10_000;

fn user_row(row: &sqlx::sqlite::SqliteRow) -> UserRow {
    (
        row.get(0),
        row.get(1),
        row.get(2),
        row.get(3),
        row.get::<i64, _>(4) != 0,
        row.get(5),
    )
}

pub async fn list_users(pool: &SqlitePool) -> Result<Vec<UserRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, username, role, created_at, disabled, password_hash FROM users ORDER BY created_at, username",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(user_row).collect())
}

/// `(username, ms)` for everyone with a kept sign-in or audited change:
/// the later of their newest session and their newest audit entry. A
/// session without a member belongs to `builtin_admin`. Read on a list
/// request only, so requests themselves never write activity.
pub async fn last_active(
    pool: &SqlitePool,
    builtin_admin: &str,
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT username, MAX(at) FROM (
             SELECT CASE WHEN s.user_id IS NULL THEN ? ELSE u.username END AS username,
                    s.created_at AS at
               FROM sessions s LEFT JOIN users u ON u.id = s.user_id
             UNION ALL
             SELECT username, at FROM audit_log
         ) WHERE username IS NOT NULL GROUP BY username",
    )
    .bind(builtin_admin)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(|row| (row.get(0), row.get(1))).collect())
}

pub async fn find_user_by_username(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<UserRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, username, role, created_at, disabled, password_hash FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(user_row))
}

pub async fn find_user(pool: &SqlitePool, id: &str) -> Result<Option<UserRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, username, role, created_at, disabled, password_hash FROM users WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(user_row))
}

/// Inserts a member; `Ok(false)` when the username is taken.
pub async fn insert_user(
    pool: &SqlitePool,
    id: &str,
    username: &str,
    role: &str,
    created_at: i64,
    password_hash: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO users (id, username, role, created_at, disabled, password_hash)
         VALUES (?, ?, ?, ?, 0, ?) ON CONFLICT(username) DO NOTHING",
    )
    .bind(id)
    .bind(username)
    .bind(role)
    .bind(created_at)
    .bind(password_hash)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Changes the given fields; `Ok(false)` when no such member exists.
pub async fn update_user(
    pool: &SqlitePool,
    id: &str,
    role: Option<&str>,
    disabled: Option<bool>,
    password_hash: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE users SET
            role = COALESCE(?, role),
            disabled = COALESCE(?, disabled),
            password_hash = COALESCE(?, password_hash)
         WHERE id = ?",
    )
    .bind(role)
    .bind(disabled.map(i64::from))
    .bind(password_hash)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn delete_user(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// Ends a member's sessions, except `keep` (a session token hash).
pub async fn delete_user_sessions(
    pool: &SqlitePool,
    user_id: &str,
    keep: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token != COALESCE(?, '')")
        .bind(user_id)
        .bind(keep)
        .execute(pool)
        .await?;
    Ok(())
}

/// Binds a session to a member (`None`: the built-in admin) and the role it
/// views the dashboard as.
pub async fn set_session_principal(
    pool: &SqlitePool,
    token: &str,
    user_id: Option<&str>,
    view_as: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE sessions SET user_id = ?, view_as = ? WHERE token = ?")
        .bind(user_id)
        .bind(view_as)
        .bind(token)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// The member and view of a session, or `None` when it does not exist.
pub async fn session_principal(
    pool: &SqlitePool,
    token: &str,
) -> Result<Option<(Option<String>, Option<String>)>, sqlx::Error> {
    let row = sqlx::query("SELECT user_id, view_as FROM sessions WHERE token = ?")
        .bind(token)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|row| (row.get(0), row.get(1))))
}

/// One audit row: id, at, username, role, method, path, status.
pub type AuditRow = (i64, i64, String, String, String, String, i64);

pub async fn append_audit(
    pool: &SqlitePool,
    at: i64,
    username: &str,
    role: &str,
    method: &str,
    path: &str,
    status: u16,
) -> Result<(), sqlx::Error> {
    let id = sqlx::query(
        "INSERT INTO audit_log (at, username, role, method, path, status)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(at)
    .bind(username)
    .bind(role)
    .bind(method)
    .bind(path)
    .bind(i64::from(status))
    .execute(pool)
    .await?
    .last_insert_rowid();
    // Trim in batches, not on every insert.
    if id % 100 == 0 {
        sqlx::query("DELETE FROM audit_log WHERE id <= ?")
            .bind(id - AUDIT_LOG_CAP)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// Newest first; `before` pages back from an entry id.
pub async fn list_audit(
    pool: &SqlitePool,
    limit: i64,
    before: Option<i64>,
) -> Result<Vec<AuditRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, at, username, role, method, path, status FROM audit_log
         WHERE id < COALESCE(?, 9223372036854775807) ORDER BY id DESC LIMIT ?",
    )
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| {
            (
                row.get(0),
                row.get(1),
                row.get(2),
                row.get(3),
                row.get(4),
                row.get(5),
                row.get(6),
            )
        })
        .collect())
}
