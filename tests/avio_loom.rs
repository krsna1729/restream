//! Loom model-checks for the AV I/O queue synchronization boundary.
//! This file owns the close/wake contract behind `MemoryQueue`, proving the
//! shutdown and backpressure invariants that media thread hops rely on.

#[cfg(loom)]
mod loom_tests {
    use loom::sync::{Arc, Condvar, Mutex};
    use loom::thread;

    struct FakeQueue {
        mu: Mutex<State>,
        data_available: Condvar,
        space_available: Condvar,
        capacity: usize,
    }

    #[derive(Clone, Copy)]
    struct State {
        len: usize,
        closed: bool,
    }

    impl FakeQueue {
        fn new(capacity: usize) -> Arc<Self> {
            Arc::new(Self {
                mu: Mutex::new(State {
                    len: 0,
                    closed: false,
                }),
                data_available: Condvar::new(),
                space_available: Condvar::new(),
                capacity,
            })
        }

        fn write_one(&self) -> bool {
            let mut guard = self.mu.lock().unwrap();
            loop {
                if guard.closed {
                    return false;
                }
                if guard.len < self.capacity {
                    guard.len += 1;
                    self.data_available.notify_all();
                    return true;
                }
                guard = self.space_available.wait(guard).unwrap();
            }
        }

        fn write_batch(&self, bytes: usize) -> bool {
            if bytes == 0 {
                return true;
            }

            let mut guard = self.mu.lock().unwrap();
            loop {
                if guard.closed {
                    return false;
                }
                if guard.len < self.capacity {
                    guard.len += bytes;
                    self.data_available.notify_all();
                    return true;
                }
                guard = self.space_available.wait(guard).unwrap();
            }
        }

        fn read_one(&self) -> Option<()> {
            let mut guard = self.mu.lock().unwrap();
            loop {
                if guard.len > 0 {
                    guard.len -= 1;
                    self.space_available.notify_all();
                    return Some(());
                }
                if guard.closed {
                    return None;
                }
                guard = self.data_available.wait(guard).unwrap();
            }
        }

        fn close(&self) {
            let mut guard = self.mu.lock().unwrap();
            guard.closed = true;
            self.data_available.notify_all();
            self.space_available.notify_all();
        }
    }

    #[test]
    fn loom_close_wakes_blocked_writer() {
        loom::model(|| {
            let queue = FakeQueue::new(1);
            assert!(queue.write_one(), "initial write should fill queue");

            let writer_queue = queue.clone();
            let writer = thread::spawn(move || {
                let wrote = writer_queue.write_one();
                assert!(
                    !wrote,
                    "writer blocked on a full queue must return after close"
                );
            });

            queue.close();
            writer.join().unwrap();
        });
    }

    #[test]
    fn loom_close_wakes_blocked_reader() {
        loom::model(|| {
            let queue = FakeQueue::new(1);
            let reader_queue = queue.clone();

            let reader = thread::spawn(move || {
                let item = reader_queue.read_one();
                assert!(
                    item.is_none(),
                    "reader blocked on an empty queue must return after close"
                );
            });

            queue.close();
            reader.join().unwrap();
        });
    }

    #[test]
    fn loom_close_wakes_blocked_batch_writer() {
        loom::model(|| {
            let queue = FakeQueue::new(1);
            assert!(queue.write_one(), "initial write should fill queue");

            let writer_queue = queue.clone();
            let writer = thread::spawn(move || {
                let wrote = writer_queue.write_batch(2);
                assert!(
                    !wrote,
                    "batch writer blocked on a full queue must return after close"
                );
            });

            queue.close();
            writer.join().unwrap();
        });
    }

    #[test]
    fn loom_read_wakes_blocked_batch_writer() {
        loom::model(|| {
            let queue = FakeQueue::new(1);
            assert!(queue.write_one(), "initial write should fill queue");

            let writer_queue = queue.clone();
            let writer = thread::spawn(move || {
                let wrote = writer_queue.write_batch(2);
                assert!(wrote, "batch writer should continue after read frees space");
            });

            assert!(
                queue.read_one().is_some(),
                "reader should drain initial byte"
            );
            writer.join().unwrap();
        });
    }
}

/// Loom model of the `tokio::sync::Notify` arm-before-check ordering that
/// `f679a249` (fix lost-wakeup race in `MemoryQueue::write`) depends on.
/// Loom has no built-in `Notify` equivalent, so this rebuilds the piece of
/// its semantics that matters here directly: `notify_waiters()` only wakes
/// threads that had already registered ("armed") by the time it runs; a
/// thread that registers afterward does not observe that notification.
/// `thread::park`/`unpark` alone would model `notify_one`'s permit
/// semantics (a token survives regardless of call order) — the explicit
/// `waiters` registration list is what reintroduces the stricter ordering
/// constraint `notify_waiters()` actually has.
///
/// This intentionally proves only the positive invariant (arm-before-check
/// survives every interleaving), not a negative "buggy shape deadlocks"
/// control: driving loom into a genuine no-progress deadlock here makes it
/// panic from inside its own `Arc`/thread cleanup during unwind, which
/// escalates to a double panic and aborts the whole test binary — taking
/// down every other test in this file rather than yielding a clean,
/// catchable `#[should_panic]` result. The historical bug (`f679a249`) and
/// its regression test (`write_wakeup_survives_lock_release_race` in
/// `src/media/avio.rs`) already document what the arm-after-check shape
/// does; this model exists to prove the fixed shape holds, not to
/// re-litigate the bug shape under a scheduler whose deadlock path isn't
/// safe to assert against here.
#[cfg(loom)]
mod notify_arm_order_tests {
    use loom::sync::{Arc, Mutex};
    use loom::thread;

    struct ArmOrderQueue {
        len: Mutex<usize>,
        capacity: usize,
        waiters: Mutex<Vec<thread::Thread>>,
    }

    impl ArmOrderQueue {
        fn new(capacity: usize, initial_len: usize) -> Arc<Self> {
            Arc::new(Self {
                len: Mutex::new(initial_len),
                capacity,
                waiters: Mutex::new(Vec::new()),
            })
        }

        /// The "arm" step `notified()` performs in the real code: register
        /// the current thread so a subsequent `notify_waiters()` can find
        /// and wake it. Whether this runs before or after the capacity
        /// check/lock release is the entire question this test settles.
        fn arm(&self) {
            self.waiters.lock().unwrap().push(thread::current());
        }

        fn notify_waiters(&self) {
            let mut waiters = self.waiters.lock().unwrap();
            for waiter in waiters.drain(..) {
                waiter.unpark();
            }
        }

        /// Fixed shape: arm before the capacity check, mirroring post-fix
        /// `MemoryQueue::write` — registration snapshots "I am waiting"
        /// before the lock (and thus the notifier) can run.
        fn write_fixed(&self) {
            loop {
                self.arm();
                {
                    let mut guard = self.len.lock().unwrap();
                    if *guard < self.capacity {
                        *guard += 1;
                        return;
                    }
                }
                thread::park();
            }
        }

        fn read_one(&self) {
            let mut guard = self.len.lock().unwrap();
            *guard -= 1;
            drop(guard);
            self.notify_waiters();
        }
    }

    /// The fix's invariant: arming before the capacity check must survive
    /// every interleaving of a single reader draining the queue.
    #[test]
    fn loom_fixed_write_survives_notify_race() {
        loom::model(|| {
            let queue = ArmOrderQueue::new(1, 1);
            let writer_queue = queue.clone();
            let writer = thread::spawn(move || {
                writer_queue.write_fixed();
            });

            queue.read_one();

            writer.join().unwrap();
            assert_eq!(*queue.len.lock().unwrap(), 1);
        });
    }
}
