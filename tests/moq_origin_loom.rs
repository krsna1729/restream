//! Loom model-checks for the MoQ origin's cross-thread hand-offs
//! (`crates/restream-engine/src/moq/origin/mod.rs`):
//!
//! - **Viewer cap.** `ViewerSlots::try_acquire` is a load + CAS loop that
//!   refuses at `max`; a `ViewerSlot` drop is an `AcqRel` decrement. Listener
//!   threads may admit concurrently: no interleaving admits past `max`, and
//!   every release frees its slot.
//! - **Ring → pump wake.** The ring's publisher (a Tokio or ingest thread)
//!   advances the write index, then wakes the origin's pump through a
//!   one-slot channel whose `try_send` drops the wake when a token is already
//!   queued. The pump reads the ring to empty, then waits for a token. A
//!   publication after the read leaves a token behind, so no interleaving
//!   strands published data (loom's deadlock detector catches a pump asleep
//!   with nothing left to wake it).
//!
//! The model mirrors the primitives rather than importing them (std atomics
//! vs loom atomics), matching the repository's loom convention: keep it in
//! sync with `ViewerSlots`, `ChannelWake` and `pump` in the origin module.
#![allow(clippy::disallowed_methods)] // test code: raw std locks are fine

#[cfg(loom)]
mod loom_tests {
    use loom::sync::atomic::{AtomicUsize, Ordering};
    use loom::sync::{Arc, Condvar, Mutex};
    use loom::thread;

    /// Mirror of `ViewerSlots::try_acquire`: returns the count it took.
    fn try_acquire(in_use: &AtomicUsize, max: usize) -> Option<usize> {
        let mut current = in_use.load(Ordering::Acquire);
        loop {
            if current >= max {
                return None;
            }
            match in_use.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(current + 1),
                Err(actual) => current = actual,
            }
        }
    }

    /// Mirror of `ViewerSlot::drop`.
    fn release(in_use: &AtomicUsize) {
        in_use.fetch_sub(1, Ordering::AcqRel);
    }

    #[test]
    fn concurrent_admissions_never_pass_the_cap_and_releases_free_slots() {
        loom::model(|| {
            const MAX: usize = 1;
            let in_use = Arc::new(AtomicUsize::new(0));
            let admitted = Arc::new(AtomicUsize::new(0));
            let viewers: Vec<_> = (0..2)
                .map(|_| {
                    let in_use = in_use.clone();
                    let admitted = admitted.clone();
                    thread::spawn(move || {
                        if let Some(taken) = try_acquire(&in_use, MAX) {
                            assert!(taken <= MAX, "admitted past the cap");
                            admitted.fetch_add(1, Ordering::Relaxed);
                            release(&in_use);
                        }
                    })
                })
                .collect();
            if let Some(taken) = try_acquire(&in_use, MAX) {
                assert!(taken <= MAX, "admitted past the cap");
                admitted.fetch_add(1, Ordering::Relaxed);
                release(&in_use);
            }
            for viewer in viewers {
                viewer.join().unwrap();
            }
            assert_eq!(in_use.load(Ordering::Acquire), 0, "a slot leaked");
            assert!(admitted.load(Ordering::Relaxed) >= 1, "nobody was admitted");
        });
    }

    #[test]
    fn a_full_cap_refuses_until_a_release() {
        loom::model(|| {
            let in_use = Arc::new(AtomicUsize::new(1));
            let leaver = {
                let in_use = in_use.clone();
                thread::spawn(move || release(&in_use))
            };
            let first = try_acquire(&in_use, 1);
            leaver.join().unwrap();
            match first {
                // Admitted after the release: the slot is taken again.
                Some(taken) => assert_eq!((taken, in_use.load(Ordering::Acquire)), (1, 1)),
                // Refused before it: the release is visible now.
                None => assert_eq!(try_acquire(&in_use, 1), Some(1)),
            }
        });
    }

    /// Mirror of the ring write index plus the `flume::bounded(1)` wake
    /// channel (`token`): `try_send` sets it unless set; `recv` waits for it
    /// and takes it.
    struct WakeModel {
        head: AtomicUsize,
        token: Mutex<bool>,
        ready: Condvar,
    }

    impl WakeModel {
        /// Ring publication, then `ChannelWake::wake`.
        fn publish(&self, head: usize) {
            self.head.store(head, Ordering::Release);
            let mut token = self.token.lock().unwrap();
            if !*token {
                *token = true;
                self.ready.notify_one();
            }
        }

        /// `recv_async`: sleep until a token is queued, then take it.
        fn recv(&self) {
            let mut token = self.token.lock().unwrap();
            while !*token {
                token = self.ready.wait(token).unwrap();
            }
            *token = false;
        }
    }

    #[test]
    fn the_pump_never_sleeps_on_published_data() {
        loom::model(|| {
            const LAST: usize = 2;
            let model = Arc::new(WakeModel {
                head: AtomicUsize::new(0),
                token: Mutex::new(false),
                ready: Condvar::new(),
            });
            let publisher = {
                let model = model.clone();
                thread::spawn(move || {
                    for head in 1..=LAST {
                        model.publish(head);
                    }
                })
            };
            // The pump: read to the head (`Pump::Sent`), wait when empty.
            let mut cursor = 0;
            while cursor < LAST {
                let head = model.head.load(Ordering::Acquire);
                if head > cursor {
                    cursor = head;
                } else {
                    model.recv();
                }
            }
            publisher.join().unwrap();
            assert_eq!(cursor, LAST);
        });
    }
}
