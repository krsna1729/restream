use super::*;
use crate::db::{create_lifecycle_pool, create_pool, setup_database_schema};
use sqlx::AssertSqlSafe;

const DEADLINE: Duration = Duration::from_millis(300);
/// Far below the 30 s acquire and busy timeouts a call without the deadline
/// would wait.
const ANSWERED_WITHIN: Duration = Duration::from_secs(5);

/// A file database with production's two pools: a write lock and the
/// lifecycle pool's connections are then real and separate from the API's.
async fn file_pools(tag: &str) -> (SqlitePool, DeadlineDb, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "restream-deadline-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let url = format!("sqlite://{}", dir.join("db.sqlite").display());
    let api = create_pool(&url).await.unwrap();
    setup_database_schema(&api).await.unwrap();
    let lifecycle = create_lifecycle_pool(&url).await.unwrap();
    (api, DeadlineDb::new(lifecycle, DEADLINE), dir)
}

async fn hold_every_connection(pool: &SqlitePool) -> Vec<sqlx::pool::PoolConnection<sqlx::Sqlite>> {
    let mut held = Vec::new();
    while let Ok(connection) =
        tokio::time::timeout(Duration::from_millis(200), pool.acquire()).await
    {
        held.push(connection.unwrap());
    }
    assert!(!held.is_empty());
    held
}

/// The write lock another writer (an API transaction) holds.
async fn hold_write_lock(api: &SqlitePool) -> sqlx::pool::PoolConnection<sqlx::Sqlite> {
    let mut connection = api.acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .unwrap();
    connection
}

async fn release_write_lock(mut connection: sqlx::pool::PoolConnection<sqlx::Sqlite>) {
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .unwrap();
}

async fn count(pool: &SqlitePool, sql: &str) -> i64 {
    sqlx::query_scalar(AssertSqlSafe(sql.to_string()))
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Every lifecycle connection held: a shared-loop read answers at its
/// deadline instead of after the pool's 30 s acquire timeout, and reads
/// again once a connection is free.
#[tokio::test]
async fn a_read_returns_at_its_deadline_while_every_connection_is_held() {
    let (_api, db, dir) = file_pools("read").await;
    let held = hold_every_connection(&db.pool).await;

    let answer = tokio::time::timeout(ANSWERED_WITHIN, db.read(crate::db::list_outputs))
        .await
        .expect("the read answers at its deadline, not the acquire timeout");
    assert!(
        matches!(&answer, Err(sqlx::Error::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut),
        "{answer:?}"
    );

    drop(held);
    assert!(db.read(crate::db::list_outputs).await.is_ok());
    let _ = std::fs::remove_dir_all(dir);
}

/// A write behind another writer's lock is not given up at the deadline:
/// it answers with its outcome once the lock is released.
#[tokio::test]
async fn a_write_behind_a_held_write_lock_answers_with_its_outcome_after_release() {
    let (api, db, dir) = file_pools("write").await;
    let lock = hold_write_lock(&api).await;

    let write = tokio::spawn({
        let db = db.clone();
        async move {
            db.write(|pool| async move { crate::db::set_meta(&pool, "k", "v").await })
                .await
        }
    });
    tokio::time::sleep(DEADLINE * 3).await;
    assert!(
        !write.is_finished(),
        "a write gave up at its deadline while it could still land"
    );

    release_write_lock(lock).await;
    let answer = tokio::time::timeout(ANSWERED_WITHIN, write)
        .await
        .expect("the write answers once the lock is released")
        .unwrap();
    assert!(answer.is_ok(), "{answer:?}");
    assert_eq!(
        crate::db::get_meta(&api, "k").await.unwrap().as_deref(),
        Some("v")
    );
    let _ = std::fs::remove_dir_all(dir);
}

async fn insert_logs(api: &SqlitePool, rows: usize, ts: &str) {
    let mut tx = api.begin().await.unwrap();
    for _ in 0..rows {
        sqlx::query(
            "INSERT INTO app_logs (ts, level, target, message) VALUES (?, 'info', 't', 'm')",
        )
        .bind(ts)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

/// Retention deletes in statements of at most one batch, each its own
/// short write, and stops after the first short batch.
#[tokio::test]
async fn log_retention_deletes_in_batches_of_at_most_the_batch_size() {
    let (api, db, dir) = file_pools("retention").await;
    insert_logs(&api, 25, "2000-01-01T00:00:00Z").await;
    insert_logs(&api, 3, "2999-01-01T00:00:00Z").await;

    let batches = crate::db::prune_app_logs(&db, 7, 10).await.unwrap();

    assert_eq!(
        batches,
        [10, 10, 5],
        "one statement per batch of at most 10 rows"
    );
    assert_eq!(count(&api, "SELECT COUNT(*) FROM app_logs").await, 3);
    let _ = std::fs::remove_dir_all(dir);
}
