//! Loom model-check for `restream_sync::keyed_lock::KeyedLock`, the
//! per-key lock behind the engine's per-pipeline ingest selection
//! (`IngestRegistry::selection`) and the MoQ origin's per-pipeline attach
//! lock.
//!
//! An acquirer joins the key's entry (or inserts it) under the map mutex,
//! then waits on the entry's own mutex; a user (holder, or a waiter that
//! gives up) leaves under the map mutex and the last one removes the entry.
//! Every interleaving of two holders and one abandoning waiter keeps at most
//! one holder inside the key's critical section, and leaves the map empty:
//! an entry removed while a waiter still had it would let a later acquirer
//! mint a second mutex for the key.
//!
//! The model mirrors the primitive rather than importing it (its async
//! mutex is not loom-instrumented; a blocking loom mutex stands in), matching
//! the repository's loom convention: keep it in sync with `keyed_lock.rs`.
#![allow(clippy::disallowed_methods)] // test code: raw std locks are fine

#[cfg(loom)]
mod loom_tests {
    use std::collections::HashMap;

    use loom::sync::atomic::{AtomicUsize, Ordering};
    use loom::sync::{Arc, Mutex};
    use loom::thread;

    struct Entry {
        mutex: Arc<Mutex<()>>,
        users: usize,
    }

    #[derive(Default)]
    struct KeyedLock {
        entries: Mutex<HashMap<&'static str, Entry>>,
    }

    impl KeyedLock {
        /// Mirror of the synchronous half of `KeyedLock::lock`: join or
        /// insert the entry, returning its mutex.
        fn join(&self, key: &'static str) -> Arc<Mutex<()>> {
            let mut entries = self.entries.lock().unwrap();
            let entry = entries.entry(key).or_insert_with(|| Entry {
                mutex: Arc::new(Mutex::new(())),
                users: 0,
            });
            entry.users += 1;
            entry.mutex.clone()
        }

        /// Mirror of `User::drop`.
        fn leave(&self, key: &'static str) {
            let mut entries = self.entries.lock().unwrap();
            let entry = entries.get_mut(key).expect("a user's entry exists");
            entry.users -= 1;
            if entry.users == 0 {
                entries.remove(key);
            }
        }
    }

    fn hold(locks: &KeyedLock, inside: &AtomicUsize) {
        let mutex = locks.join("p");
        {
            let _held = mutex.lock().unwrap();
            assert_eq!(
                inside.fetch_add(1, Ordering::SeqCst),
                0,
                "two holders of one key"
            );
            inside.fetch_sub(1, Ordering::SeqCst);
        }
        // `KeyedGuard` releases the mutex, then the user leaves.
        locks.leave("p");
    }

    #[test]
    fn one_holder_per_key_and_no_entry_left_behind() {
        loom::model(|| {
            let locks = Arc::new(KeyedLock::default());
            let inside = Arc::new(AtomicUsize::new(0));
            let holders: Vec<_> = (0..2)
                .map(|_| {
                    let locks = locks.clone();
                    let inside = inside.clone();
                    thread::spawn(move || hold(&locks, &inside))
                })
                .collect();
            // A waiter whose future is dropped before it gets the mutex.
            let _mutex = locks.join("p");
            locks.leave("p");
            for holder in holders {
                holder.join().unwrap();
            }
            assert!(locks.entries.lock().unwrap().is_empty(), "an entry leaked");
        });
    }
}
