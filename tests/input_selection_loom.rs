#[cfg(loom)]
mod loom_tests {
    use loom::sync::Arc;
    use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use loom::thread;

    const STANDBY: u8 = 0;
    const ACTIVE: u8 = 1;
    const STATE_MASK: usize = 0b1;
    const IN_FLIGHT_ONE: usize = 1 << 1;

    struct Gate {
        state_and_in_flight: AtomicUsize,
    }

    impl Gate {
        fn active() -> Self {
            Self {
                state_and_in_flight: AtomicUsize::new(ACTIVE as usize),
            }
        }

        fn standby() -> Self {
            Self {
                state_and_in_flight: AtomicUsize::new(STANDBY as usize),
            }
        }

        fn try_enter(&self) -> bool {
            loop {
                let current = self.state_and_in_flight.load(Ordering::SeqCst);
                if current & STATE_MASK != ACTIVE as usize {
                    return false;
                }
                if self
                    .state_and_in_flight
                    .compare_exchange(
                        current,
                        current + IN_FLIGHT_ONE,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    return true;
                }
            }
        }

        fn leave(&self) {
            self.state_and_in_flight
                .fetch_sub(IN_FLIGHT_ONE, Ordering::SeqCst);
        }

        fn demote(&self) {
            self.state_and_in_flight
                .fetch_and(!STATE_MASK, Ordering::SeqCst);
        }

        fn activate(&self) {
            self.state_and_in_flight
                .fetch_or(ACTIVE as usize, Ordering::SeqCst);
        }

        fn in_flight(&self) -> usize {
            self.state_and_in_flight.load(Ordering::SeqCst) >> 1
        }
    }

    #[test]
    fn promotion_never_allows_old_and_new_writers_to_overlap() {
        loom::model(|| {
            let old = Arc::new(Gate::active());
            let new = Arc::new(Gate::standby());
            let overlap = Arc::new(AtomicBool::new(false));
            let writer_count = Arc::new(AtomicUsize::new(0));

            let old_writer = {
                let gate = old.clone();
                let count = writer_count.clone();
                let overlap = overlap.clone();
                thread::spawn(move || {
                    if gate.try_enter() {
                        let previous = count.fetch_add(1, Ordering::SeqCst);
                        if previous != 0 {
                            overlap.store(true, Ordering::SeqCst);
                        }
                        count.fetch_sub(1, Ordering::SeqCst);
                        gate.leave();
                    }
                })
            };
            let promoter = {
                let old = old.clone();
                let new = new.clone();
                thread::spawn(move || {
                    old.demote();
                    while old.in_flight() != 0 {
                        thread::yield_now();
                    }
                    new.activate();
                })
            };
            let new_writer = {
                let gate = new.clone();
                let count = writer_count.clone();
                let overlap = overlap.clone();
                thread::spawn(move || {
                    if gate.try_enter() {
                        let previous = count.fetch_add(1, Ordering::SeqCst);
                        if previous != 0 {
                            overlap.store(true, Ordering::SeqCst);
                        }
                        count.fetch_sub(1, Ordering::SeqCst);
                        gate.leave();
                    }
                })
            };
            old_writer.join().unwrap();
            promoter.join().unwrap();
            new_writer.join().unwrap();

            assert!(!overlap.load(Ordering::SeqCst));
        });
    }
}
