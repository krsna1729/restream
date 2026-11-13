//! Deadline-bounded SQLite access for the loops every entity shares.
//!
//! The reconciler, housekeeping, egress start-up, the scheduler and the
//! catalog reload each serve every pipeline from one task. A call that waits
//! on the pool (API reads holding every connection: a 30 s acquire timeout)
//! or on the write lock (a 30 s busy timeout, retried up to 8 times by
//! [`super::with_busy_retry`]) would stall all of them for minutes.
//! [`DeadlineDb`] bounds a whole read — acquire, statements and busy
//! retries — and in production runs on the lifecycle pool
//! ([`super::create_lifecycle_pool`]), which API handlers never use, so API
//! load cannot take the capacity these loops need.
//!
//! The rule: a deadline bounds a caller's wait, never a write's outcome. A
//! read is dropped at its deadline: nothing changed, and the loop retries on
//! its next pass. A write is never given up on: a write that missed a
//! deadline can still land, so treating it as failed makes the caller act on
//! a state the database is about to contradict. [`DeadlineDb::write`] runs
//! the write as its own task (as `CatalogAuth::commit` does), so it is never
//! cancelled mid-transaction, and returns its real outcome however long it
//! takes. Only a task that may stall on that is allowed to write: one
//! output's task, housekeeping, the scheduler; the output pass never awaits
//! a write.
//!
//! At most [`WRITE_PERMITS`] writes hold a connection at once, so writes
//! queued behind another writer's lock never take the connections the loops
//! read on (WAL readers do not wait for the write lock).
//!
//! Why no `disallowed-methods` ban enforces this: clippy's allow is per lint,
//! not per method, so exempting `src/db/` from a ban on sqlx execution would
//! also exempt it from the `Row::get` and `Mutex::lock` bans, or take an
//! attribute on every repository function; and the shared loops call
//! repository functions, not sqlx, so such a ban would not reach them. The
//! loops instead hold a `DeadlineDb` and no pool.

use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::Semaphore;

/// The deadline production gives every shared-loop call. A lifecycle-pool
/// read answers in milliseconds; past this the loop moves on and retries on
/// its next pass.
pub const SHARED_LOOP_DEADLINE: Duration = Duration::from_secs(3);

/// Writes on one executor that hold a connection at once; the rest wait for
/// a permit without one.
pub const WRITE_PERMITS: usize = 2;

/// Why a deadline-bounded call has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineError {
    /// The read did not finish within the deadline.
    Elapsed(Duration),
    /// The write task ended without an answer (it panicked).
    Aborted,
}

impl std::fmt::Display for DeadlineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Elapsed(deadline) => write!(
                formatter,
                "database call exceeded its {} ms deadline",
                deadline.as_millis()
            ),
            Self::Aborted => formatter.write_str("database write task ended without an answer"),
        }
    }
}

impl std::error::Error for DeadlineError {}

impl From<DeadlineError> for sqlx::Error {
    fn from(error: DeadlineError) -> Self {
        let kind = match error {
            DeadlineError::Elapsed(_) => std::io::ErrorKind::TimedOut,
            DeadlineError::Aborted => std::io::ErrorKind::Other,
        };
        sqlx::Error::Io(std::io::Error::new(kind, error))
    }
}

/// A pool whose reads answer within one deadline and whose writes run to
/// their outcome, at most [`WRITE_PERMITS`] at a time.
#[derive(Clone)]
pub struct DeadlineDb {
    pool: SqlitePool,
    deadline: Duration,
    writes: Arc<Semaphore>,
}

impl DeadlineDb {
    pub fn new(pool: SqlitePool, deadline: Duration) -> Self {
        Self {
            pool,
            deadline,
            writes: Arc::new(Semaphore::new(WRITE_PERMITS)),
        }
    }

    /// Bounds a read that is already built: one through an application
    /// port whose adapter runs on this pool. Dropped at the deadline.
    pub async fn bound<T, E>(&self, call: impl Future<Output = Result<T, E>>) -> Result<T, E>
    where
        E: From<DeadlineError>,
    {
        tokio::time::timeout(self.deadline, call)
            .await
            .unwrap_or_else(|_| Err(DeadlineError::Elapsed(self.deadline).into()))
    }

    /// A read: dropped at the deadline, which leaves nothing behind.
    pub async fn read<'a, T, E, Fut>(
        &'a self,
        op: impl FnOnce(&'a SqlitePool) -> Fut,
    ) -> Result<T, E>
    where
        Fut: Future<Output = Result<T, E>> + 'a,
        E: From<DeadlineError>,
    {
        self.bound(op(&self.pool)).await
    }

    /// A write, run as its own task under a write permit and awaited to its
    /// outcome: no deadline, since one that passed would not stop it
    /// landing. Dropping the caller leaves it running to completion.
    pub async fn write<T, E, Fut>(&self, op: impl FnOnce(SqlitePool) -> Fut) -> Result<T, E>
    where
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: From<DeadlineError> + Send + 'static,
    {
        let (pool, writes) = (self.pool.clone(), self.writes.clone());
        let write = op(pool);
        tokio::spawn(async move {
            // Never closed, so the permit always comes.
            let _permit = writes.acquire_owned().await;
            write.await
        })
        .await
        .unwrap_or_else(|_| Err(DeadlineError::Aborted.into()))
    }
}

#[cfg(test)]
mod tests;
