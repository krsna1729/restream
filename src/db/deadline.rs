//! Deadline-bounded SQLite access for the loops every entity shares.
//!
//! The reconciler, housekeeping, egress start-up, the scheduler and the
//! catalog reload each serve every pipeline from one task. A call that waits
//! on the pool (API reads holding every connection: a 30 s acquire timeout)
//! or on the write lock (a 30 s busy timeout, retried up to 8 times by
//! [`super::with_busy_retry`]) would stall all of them for minutes.
//! [`DeadlineDb`] bounds the whole call — acquire, statements and busy
//! retries — and in production runs on the lifecycle pool
//! ([`super::create_lifecycle_pool`]), which API handlers never use, so API
//! load cannot take the capacity these loops need.
//!
//! A read is dropped at its deadline: nothing changed, and the loop retries
//! on its next pass. A write runs as its own task, as `CatalogAuth::commit`
//! does: the caller stops waiting at the deadline, but the write is never
//! cancelled mid-transaction, so it lands or fails as a unit and the next
//! pass observes whichever happened.
//!
//! Why no `disallowed-methods` ban enforces this: clippy's allow is per lint,
//! not per method, so exempting `src/db/` from a ban on sqlx execution would
//! also exempt it from the `Row::get` and `Mutex::lock` bans, or take an
//! attribute on every repository function; and the shared loops call
//! repository functions, not sqlx, so such a ban would not reach them. The
//! loops instead hold a `DeadlineDb` and no pool.

use std::time::Duration;

use sqlx::SqlitePool;

/// The deadline production gives every shared-loop call. A lifecycle-pool
/// read answers in milliseconds; past this the loop moves on and retries on
/// its next pass.
pub const SHARED_LOOP_DEADLINE: Duration = Duration::from_secs(3);

/// Why a deadline-bounded call has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineError {
    /// The call did not finish within the deadline. A write may still land.
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

/// A pool whose every call answers within one deadline.
#[derive(Clone)]
pub struct DeadlineDb {
    pool: SqlitePool,
    deadline: Duration,
}

impl DeadlineDb {
    pub fn new(pool: SqlitePool, deadline: Duration) -> Self {
        Self { pool, deadline }
    }

    pub fn deadline(&self) -> Duration {
        self.deadline
    }

    /// Bounds a call that is already built: a read through an application
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

    /// A write, run as its own task: the caller stops waiting at the
    /// deadline, the write runs to completion. Its effect is observed by the
    /// next read, never assumed from a timeout.
    pub async fn write<T, E, Fut>(&self, op: impl FnOnce(SqlitePool) -> Fut) -> Result<T, E>
    where
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: From<DeadlineError> + Send + 'static,
    {
        match tokio::time::timeout(self.deadline, self.detached(op)).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(DeadlineError::Aborted.into()),
            Err(_) => Err(DeadlineError::Elapsed(self.deadline).into()),
        }
    }

    /// Writes no loop waits for (one entity's bookkeeping, written in
    /// order), run as their own task.
    pub fn detached<Fut>(
        &self,
        op: impl FnOnce(SqlitePool) -> Fut,
    ) -> tokio::task::JoinHandle<Fut::Output>
    where
        Fut: Future + Send + 'static,
        Fut::Output: Send + 'static,
    {
        tokio::spawn(op(self.pool.clone()))
    }
}

#[cfg(test)]
mod tests;
