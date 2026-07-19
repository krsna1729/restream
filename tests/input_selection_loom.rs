#[cfg(loom)]
mod loom_tests {
    use loom::sync::Arc;
    use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use loom::thread;

    const STANDBY: u8 = 0;
    const AWAITING_REPLAY: u8 = 1;
    const ACTIVE: u8 = 2;
    const STATE_MASK: usize = 0b11;
    const IN_FLIGHT_ONE: usize = 1 << 2;

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

        fn try_enter(&self, replay_ready: bool) -> Option<bool> {
            loop {
                let current = self.state_and_in_flight.load(Ordering::SeqCst);
                let state = current & STATE_MASK;
                let activated = match state {
                    state if state == ACTIVE as usize => false,
                    state if state == AWAITING_REPLAY as usize && replay_ready => true,
                    _ => return None,
                };
                let next_state = if activated { ACTIVE as usize } else { state };
                let next = (current & !STATE_MASK) + IN_FLIGHT_ONE | next_state;
                if self
                    .state_and_in_flight
                    .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    return Some(activated);
                }
            }
        }

        fn leave(&self) {
            self.state_and_in_flight
                .fetch_sub(IN_FLIGHT_ONE, Ordering::SeqCst);
        }

        fn demote(&self) {
            self.set_state(STANDBY);
        }

        fn arm(&self) {
            self.set_state(AWAITING_REPLAY);
        }

        fn in_flight(&self) -> usize {
            self.state_and_in_flight.load(Ordering::SeqCst) >> 2
        }

        fn set_state(&self, state: u8) {
            let mut current = self.state_and_in_flight.load(Ordering::SeqCst);
            loop {
                let next = (current & !STATE_MASK) | state as usize;
                match self.state_and_in_flight.compare_exchange(
                    current,
                    next,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => return,
                    Err(observed) => current = observed,
                }
            }
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
                    if gate.try_enter(false).is_some() {
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
                    new.arm();
                })
            };
            let new_writer = {
                let gate = new.clone();
                let count = writer_count.clone();
                let overlap = overlap.clone();
                thread::spawn(move || {
                    if gate.try_enter(true).is_some() {
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

    #[test]
    fn replay_ready_boundary_activates_exactly_once() {
        loom::model(|| {
            let gate = Arc::new(Gate::standby());
            gate.arm();
            let activations = Arc::new(AtomicUsize::new(0));

            let writers = (0..2)
                .map(|_| {
                    let gate = gate.clone();
                    let activations = activations.clone();
                    thread::spawn(move || {
                        if let Some(activated) = gate.try_enter(true) {
                            if activated {
                                activations.fetch_add(1, Ordering::SeqCst);
                            }
                            gate.leave();
                        }
                    })
                })
                .collect::<Vec<_>>();

            for writer in writers {
                writer.join().unwrap();
            }
            assert_eq!(activations.load(Ordering::SeqCst), 1);
        });
    }
}
