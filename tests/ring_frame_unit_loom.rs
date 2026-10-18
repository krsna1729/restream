//! Loom model-check for `RingBuffer::frame_unit`, the lock-free read of the
//! per-slot frame-unit side table. Reduced to the slot unit atomics and the
//! write index: the producer stores each slot's unit word (Release), tagged
//! with its sequence, and stores the write index (Release) only after the
//! whole publication; for a batch that is after several slots. A reader
//! loads the write index, keeps only the retained window, loads the unit
//! (both Acquire) and accepts it only when its tag names the sequence asked.

#[cfg(loom)]
mod loom_tests {
    use loom::sync::Arc;
    use loom::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use loom::thread;

    const CAPACITY: usize = 2;
    const PRESENT: u64 = 1 << 63;
    const TAG_MASK: u64 = (1 << 31) - 1;

    struct FakeRing {
        write_idx: AtomicUsize,
        units: [AtomicU64; CAPACITY],
    }

    /// The generation the producer records for `sequence`.
    fn generation_of(sequence: usize) -> u32 {
        sequence as u32 + 100
    }

    fn pack(sequence: usize) -> u64 {
        PRESENT | (sequence as u64 & TAG_MASK) << 32 | u64::from(generation_of(sequence))
    }

    impl FakeRing {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                write_idx: AtomicUsize::new(0),
                units: std::array::from_fn(|_| AtomicU64::new(0)),
            })
        }

        /// `push_batch`: every slot first, then one write-index store.
        fn push_batch(&self, count: usize) {
            let start = self.write_idx.load(Ordering::Relaxed);
            for sequence in start..start + count {
                self.units[sequence % CAPACITY].store(pack(sequence), Ordering::Release);
            }
            self.write_idx.store(start + count, Ordering::Release);
        }

        fn frame_unit(&self, sequence: usize) -> Option<u32> {
            let write_idx = self.write_idx.load(Ordering::Acquire);
            if sequence >= write_idx || write_idx - sequence >= CAPACITY {
                return None;
            }
            let raw = self.units[sequence % CAPACITY].load(Ordering::Acquire);
            (raw & PRESENT != 0 && (raw >> 32) & TAG_MASK == sequence as u64 & TAG_MASK)
                .then_some(raw as u32)
        }
    }

    /// Whatever the interleaving with a producer that laps the ring one
    /// packet at a time, a reader gets either nothing or the unit of exactly
    /// the sequence asked.
    #[test]
    fn loom_frame_unit_never_returns_another_sequences_unit() {
        loom::model(|| {
            let ring = FakeRing::new();
            let reader_ring = ring.clone();

            let reader = thread::spawn(move || {
                for sequence in 0..3 {
                    if let Some(generation) = reader_ring.frame_unit(sequence) {
                        assert_eq!(generation, generation_of(sequence), "sequence {sequence}");
                    }
                }
            });

            for _ in 0..3 {
                ring.push_batch(1);
            }

            reader.join().unwrap();
            assert_eq!(ring.frame_unit(2), Some(generation_of(2)));
            assert_eq!(ring.frame_unit(0), None, "lapped");
        });
    }

    /// A two-packet batch overwrites both slots before it moves the write
    /// index, so a reader of a still-"retained" sequence can load the unit of
    /// the packet overwriting it; the tag rejects it.
    #[test]
    fn loom_frame_unit_rejects_a_slot_an_unfinished_batch_overwrote() {
        loom::model(|| {
            let ring = FakeRing::new();
            ring.push_batch(CAPACITY);
            let reader_ring = ring.clone();

            let reader = thread::spawn(move || {
                for sequence in 0..CAPACITY {
                    if let Some(generation) = reader_ring.frame_unit(sequence) {
                        assert_eq!(generation, generation_of(sequence), "sequence {sequence}");
                    }
                }
            });

            ring.push_batch(CAPACITY);

            reader.join().unwrap();
            let last = 2 * CAPACITY - 1;
            assert_eq!(ring.frame_unit(last), Some(generation_of(last)));
        });
    }
}
