//! Poison-tolerant access to std locks.
//!
//! A std lock is poisoned when a thread panics while holding it. Per-entity
//! panics are contained (`crate::panic_boundary`), so a poisoned lock is a
//! normal state: if every later `lock().unwrap()` panicked, one entity's
//! fault would spread to every reader of shared state, and an
//! `if let Ok(..) = lock()` would silently skip its work forever. These
//! helpers recover the guard instead. Raw `Mutex::lock`,
//! `RwLock::{read, write}` are disallowed by `clippy.toml`.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

#[allow(clippy::disallowed_methods)]
pub fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[allow(clippy::disallowed_methods)]
pub fn read<T: ?Sized>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

#[allow(clippy::disallowed_methods)]
pub fn write<T: ?Sized>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lock poisoned by a panicking holder still serves later callers,
    /// with the state the holder left.
    #[test]
    fn a_poisoned_lock_still_serves_later_callers() {
        let shared = std::sync::Arc::new(Mutex::new(1));
        let holder = std::sync::Arc::clone(&shared);
        let unwound = std::thread::spawn(move || {
            let mut guard = lock(&holder);
            *guard = 2;
            std::panic::resume_unwind(Box::new("holder unwound"));
        })
        .join();
        assert!(unwound.is_err());
        #[allow(clippy::disallowed_methods)]
        let poisoned = shared.lock().is_err();
        assert!(poisoned);
        assert_eq!(*lock(&shared), 2);

        let rw = RwLock::new(5);
        assert_eq!(*read(&rw), 5);
        *write(&rw) = 6;
        assert_eq!(*read(&rw), 6);
    }
}
