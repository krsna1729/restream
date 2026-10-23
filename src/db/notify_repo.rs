//! Notification channels, browser push subscriptions and the delivery log.

use sqlx::{Row, SqlitePool};

/// The delivery log keeps this many most recent outcomes.
pub const DELIVERY_LOG_CAP: i64 = 2_000;

/// id, kind, name, target, secret, min_severity, on_resolved, enabled, created_at
pub type ChannelRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    bool,
    bool,
    i64,
);

pub async fn list_channels(pool: &SqlitePool) -> Result<Vec<ChannelRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, kind, name, target, secret, min_severity, on_resolved, enabled, created_at
         FROM notify_channels ORDER BY created_at, id",
    )
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
                row.get::<i64, _>(6) != 0,
                row.get::<i64, _>(7) != 0,
                row.get(8),
            )
        })
        .collect())
}

pub async fn save_channel(pool: &SqlitePool, row: &ChannelRow) -> Result<(), sqlx::Error> {
    let (id, kind, name, target, secret, min_severity, on_resolved, enabled, created_at) = row;
    sqlx::query(
        "INSERT INTO notify_channels
           (id, kind, name, target, secret, min_severity, on_resolved, enabled, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET kind = excluded.kind, name = excluded.name,
           target = excluded.target, secret = excluded.secret,
           min_severity = excluded.min_severity, on_resolved = excluded.on_resolved,
           enabled = excluded.enabled",
    )
    .bind(id)
    .bind(kind)
    .bind(name)
    .bind(target)
    .bind(secret)
    .bind(min_severity)
    .bind(i64::from(*on_resolved))
    .bind(i64::from(*enabled))
    .bind(created_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_channel(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM notify_channels WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// id, username, endpoint, p256dh, auth, min_severity, created_at
pub type SubscriptionRow = (String, String, String, String, String, String, i64);

pub async fn list_subscriptions(pool: &SqlitePool) -> Result<Vec<SubscriptionRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, username, endpoint, p256dh, auth, min_severity, created_at
         FROM push_subscriptions ORDER BY created_at, id",
    )
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

/// A browser re-subscribing (same endpoint) replaces its row.
pub async fn save_subscription(
    pool: &SqlitePool,
    row: &SubscriptionRow,
) -> Result<(), sqlx::Error> {
    let (id, username, endpoint, p256dh, auth, min_severity, created_at) = row;
    sqlx::query(
        "INSERT INTO push_subscriptions (id, username, endpoint, p256dh, auth, min_severity, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(endpoint) DO UPDATE SET id = excluded.id, username = excluded.username,
           p256dh = excluded.p256dh, auth = excluded.auth, min_severity = excluded.min_severity",
    )
    .bind(id)
    .bind(username)
    .bind(endpoint)
    .bind(p256dh)
    .bind(auth)
    .bind(min_severity)
    .bind(created_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_subscription(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM push_subscriptions WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// at, target_id, target_name, event, alert_id, title, ok, attempts, detail
pub type DeliveryRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    bool,
    i64,
    String,
);

pub async fn append_delivery(pool: &SqlitePool, row: &DeliveryRow) -> Result<(), sqlx::Error> {
    let (at, target_id, target_name, event, alert_id, title, ok, attempts, detail) = row;
    let id = sqlx::query(
        "INSERT INTO notify_deliveries
           (at, target_id, target_name, event, alert_id, title, ok, attempts, detail)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(at)
    .bind(target_id)
    .bind(target_name)
    .bind(event)
    .bind(alert_id)
    .bind(title)
    .bind(i64::from(*ok))
    .bind(attempts)
    .bind(detail)
    .execute(pool)
    .await?
    .last_insert_rowid();
    if id % 100 == 0 {
        sqlx::query("DELETE FROM notify_deliveries WHERE id <= ?")
            .bind(id - DELIVERY_LOG_CAP)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// Newest first: (id, row).
pub async fn list_deliveries(
    pool: &SqlitePool,
    limit: i64,
) -> Result<Vec<(i64, DeliveryRow)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, at, target_id, target_name, event, alert_id, title, ok, attempts, detail
         FROM notify_deliveries ORDER BY id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| {
            (
                row.get(0),
                (
                    row.get(1),
                    row.get(2),
                    row.get(3),
                    row.get(4),
                    row.get(5),
                    row.get(6),
                    row.get::<i64, _>(7) != 0,
                    row.get(8),
                    row.get(9),
                ),
            )
        })
        .collect())
}
