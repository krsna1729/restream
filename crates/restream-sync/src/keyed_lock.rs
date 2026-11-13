//! One async lock per key, for state partitioned by key (one pipeline's
//! ingest selection, one pipeline's MoQ origin generation): a holder stuck on
//! one key never delays another key's acquirers.
//!
//! Entries exist only while used. Each entry counts its users (holders and
//! waiters); a user joins and leaves only under the map's mutex, and the one
//! that brings the count to zero removes the entry there. An acquirer
//! therefore either joins an entry before its last user leaves (the count
//! keeps the entry, and the entry's FIFO mutex orders it after the holder) or
//! finds no entry and inserts a fresh one no other user has: no interleaving
//! hands one key to two holders through two mutexes. A waiter whose future is
//! dropped leaves like a holder. `tests/keyed_lock_loom.rs` model-checks this
//! protocol.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

#[derive(Default)]
pub struct KeyedLock {
    entries: Mutex<HashMap<String, Entry>>,
}

struct Entry {
    mutex: Arc<AsyncMutex<()>>,
    /// Holders and waiters; the entry is removed when it reaches zero.
    users: usize,
}

impl KeyedLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Waits for `key`'s lock, behind earlier acquirers of the same key only.
    /// Cancel safe: a dropped waiter leaves the entry as a holder would.
    pub async fn lock<'a>(&'a self, key: &'a str) -> KeyedGuard<'a> {
        let mutex = {
            let mut entries = crate::lock::lock(&self.entries);
            match entries.get_mut(key) {
                Some(entry) => {
                    entry.users += 1;
                    entry.mutex.clone()
                }
                None => {
                    let mutex = Arc::new(AsyncMutex::new(()));
                    entries.insert(
                        key.to_owned(),
                        Entry {
                            mutex: mutex.clone(),
                            users: 1,
                        },
                    );
                    mutex
                }
            }
        };
        // Declared before the acquisition: a waiter dropped mid-await leaves.
        let user = User { lock: self, key };
        let held = mutex.lock_owned().await;
        KeyedGuard {
            _held: held,
            _user: user,
        }
    }

    /// Keys with a holder or a waiter right now.
    pub fn len(&self) -> usize {
        crate::lock::lock(&self.entries).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Holders plus waiters of `key` right now (0 when it has no entry).
    pub fn users(&self, key: &str) -> usize {
        crate::lock::lock(&self.entries)
            .get(key)
            .map_or(0, |entry| entry.users)
    }
}

/// `key`'s lock, held until dropped. Fields drop in order: the mutex is
/// released first, then the user leaves the entry.
#[must_use = "dropping the guard releases the key's lock at once"]
pub struct KeyedGuard<'a> {
    _held: OwnedMutexGuard<()>,
    _user: User<'a>,
}

/// One user's membership of its key's entry.
struct User<'a> {
    lock: &'a KeyedLock,
    key: &'a str,
}

impl Drop for User<'_> {
    fn drop(&mut self) {
        let mut entries = crate::lock::lock(&self.lock.entries);
        // The entry outlives every user (`users` counts this one).
        let Some(entry) = entries.get_mut(self.key) else {
            return;
        };
        entry.users -= 1;
        if entry.users == 0 {
            entries.remove(self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::{Pin, pin};
    use std::task::{Context, Poll, Waker};

    fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    /// Another key is never held up by a held key.
    #[tokio::test]
    async fn a_held_key_does_not_block_another_key() {
        let locks = KeyedLock::new();
        let _a = locks.lock("a").await;
        let b = pin!(locks.lock("b"));
        assert!(poll_once(b).is_ready(), "key b waited behind key a");
    }

    /// The holder releases while a waiter is queued: the entry must survive
    /// for the waiter, so a later acquirer queues behind it on the same
    /// mutex instead of minting a second one. Removing the entry on release
    /// regardless of its users hands the key to two holders at once.
    #[tokio::test]
    async fn a_release_with_a_queued_waiter_never_hands_the_key_to_two_holders() {
        let locks = KeyedLock::new();
        let first = locks.lock("p").await;
        let mut second = pin!(locks.lock("p"));
        assert!(poll_once(second.as_mut()).is_pending());
        assert_eq!(locks.users("p"), 2);

        drop(first);
        let second = match poll_once(second.as_mut()) {
            Poll::Ready(guard) => guard,
            Poll::Pending => panic!("the queued waiter takes the released lock"),
        };
        let mut third = pin!(locks.lock("p"));
        assert!(
            poll_once(third.as_mut()).is_pending(),
            "a later acquirer must queue behind the current holder"
        );
        assert_eq!(
            locks.users("p"),
            2,
            "the holder and the waiter share one entry"
        );
        drop(second);
        assert!(poll_once(third.as_mut()).is_ready());
    }

    /// A waiter dropped while queued leaves the entry; the last user
    /// removes it, so finished keys never accumulate.
    #[tokio::test]
    async fn dropped_waiters_and_released_holders_leave_no_entry() {
        let locks = KeyedLock::new();
        let held = locks.lock("p").await;
        {
            let mut waiter = pin!(locks.lock("p"));
            assert!(poll_once(waiter.as_mut()).is_pending());
            assert_eq!(locks.users("p"), 2);
        }
        assert_eq!(locks.users("p"), 1, "a dropped waiter leaves");
        drop(held);
        assert!(locks.is_empty());

        // The holder leaves first and the dropped waiter last.
        let held = locks.lock("p").await;
        let mut waiter = Box::pin(locks.lock("p"));
        assert!(poll_once(waiter.as_mut()).is_pending());
        drop(held);
        assert_eq!(locks.users("p"), 1);
        drop(waiter);
        assert!(locks.is_empty());
    }

    /// Many tasks contend on few keys across worker threads: at most one
    /// holder per key at any instant, and nothing is left behind.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "the test yields inside the critical section on purpose"
    )]
    async fn contended_keys_have_one_holder_each_and_leave_no_entry() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let locks = Arc::new(KeyedLock::new());
        let holders: Arc<[AtomicUsize; 3]> = Arc::new(Default::default());
        let mut tasks = Vec::new();
        for task in 0..24_usize {
            let locks = locks.clone();
            let holders = holders.clone();
            tasks.push(tokio::spawn(async move {
                for round in 0..200_usize {
                    let key = (task + round) % 3;
                    let name = ["a", "b", "c"][key];
                    let guard = locks.lock(name).await;
                    assert_eq!(holders[key].fetch_add(1, Ordering::SeqCst), 0);
                    tokio::task::yield_now().await;
                    holders[key].fetch_sub(1, Ordering::SeqCst);
                    drop(guard);
                }
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert!(locks.is_empty());
    }
}
