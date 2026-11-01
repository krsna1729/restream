//! Loom model-check for the lock-free reads of a ring slot by sequence:
//! `RingBuffer::frame_unit` (the per-slot frame-unit side table),
//! `RingBuffer::read_sequence` (the packet itself) and a run of
//! `RingBuffer::read_unverified` packets accepted by `RingBuffer::run_intact`.
//! Reduced to the slot atomics, the write index and `write_begin`: before
//! each slot the producer stores `write_begin` (Relaxed), then the slot's
//! unit word (Release), tagged with its sequence, then the slot's packet
//! (the `ArcSwap` store, a SeqCst swap, modelled as a Release store of the
//! sequence), and stores the write index (Release) only after the whole
//! publication; for a batch that is after several slots. A reader loads the
//! write index, keeps only the retained window, loads what it needs (all
//! Acquire) and accepts it only when the unit's tag names the sequence
//! asked. A run's packet loads are modelled Relaxed (Restream does not rely
//! on `ArcSwap`'s load ordering there); the run is accepted when
//! `write_begin`, read after an Acquire fence, shows no lap had begun over
//! it, or else by every slot's tag.

#[cfg(loom)]
mod loom_tests {
    use loom::sync::Arc;
    use loom::sync::atomic::{AtomicU64, AtomicUsize, Ordering, fence};
    use loom::thread;

    const CAPACITY: usize = 2;
    const PRESENT: u64 = 1 << 63;
    const TAG_MASK: u64 = (1 << 31) - 1;

    struct FakeRing {
        write_idx: AtomicUsize,
        write_begin: AtomicUsize,
        units: [AtomicU64; CAPACITY],
        /// The sequence whose packet the slot holds (`usize::MAX`: none).
        data: [AtomicUsize; CAPACITY],
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
                write_begin: AtomicUsize::new(0),
                units: std::array::from_fn(|_| AtomicU64::new(0)),
                data: std::array::from_fn(|_| AtomicUsize::new(usize::MAX)),
            })
        }

        /// `push_batch`: every slot first (`write_begin`, the unit word,
        /// then the packet), then one write-index store.
        fn push_batch(&self, count: usize) {
            let start = self.write_idx.load(Ordering::Relaxed);
            for sequence in start..start + count {
                self.write_begin.store(sequence + 1, Ordering::Relaxed);
                self.units[sequence % CAPACITY].store(pack(sequence), Ordering::Release);
                self.data[sequence % CAPACITY].store(sequence, Ordering::Release);
            }
            self.write_idx.store(start + count, Ordering::Release);
        }

        /// `read_ring`'s common run: up to `count` packets from `start`
        /// loaded without tags, then `run_intact`: one `write_begin` check,
        /// or each slot's tag when a lap over `start` had begun. `None` is
        /// an overrun.
        fn read_run(&self, start: usize, count: usize) -> Option<Vec<usize>> {
            let head = self.write_idx.load(Ordering::Acquire);
            if start + CAPACITY < head {
                return None;
            }
            let end = head.min(start + count);
            let mut packets = Vec::new();
            for sequence in start..end {
                let packet = self.data[sequence % CAPACITY].load(Ordering::Relaxed);
                if packet == usize::MAX {
                    return None;
                }
                packets.push(packet);
            }
            fence(Ordering::Acquire);
            if self.write_begin.load(Ordering::Relaxed) <= start + CAPACITY {
                return Some(packets);
            }
            (start..end)
                .all(|sequence| {
                    unpack(
                        self.units[sequence % CAPACITY].load(Ordering::Acquire),
                        sequence,
                    )
                    .is_some()
                })
                .then_some(packets)
        }

        fn frame_unit(&self, sequence: usize) -> Option<u32> {
            let write_idx = self.write_idx.load(Ordering::Acquire);
            if sequence >= write_idx || write_idx - sequence >= CAPACITY {
                return None;
            }
            let raw = self.units[sequence % CAPACITY].load(Ordering::Acquire);
            unpack(raw, sequence)
        }

        /// `read_sequence`: the packet first, then its unit word; a packet
        /// is accepted only when the word names `sequence`.
        fn read_sequence(&self, sequence: usize) -> Option<usize> {
            let packet = self.data[sequence % CAPACITY].load(Ordering::Acquire);
            if packet == usize::MAX {
                return None;
            }
            unpack(
                self.units[sequence % CAPACITY].load(Ordering::Acquire),
                sequence,
            )?;
            Some(packet)
        }
    }

    fn unpack(raw: u64, sequence: usize) -> Option<u32> {
        (raw & PRESENT != 0 && (raw >> 32) & TAG_MASK == sequence as u64 & TAG_MASK)
            .then_some(raw as u32)
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

    /// A reader racing a batch that laps its sequence gets either nothing or
    /// the packet of exactly the sequence asked: a newer packet in the slot
    /// always comes with its newer unit word.
    #[test]
    fn loom_read_sequence_never_returns_another_sequences_packet() {
        loom::model(|| {
            let ring = FakeRing::new();
            ring.push_batch(CAPACITY);
            let reader_ring = ring.clone();

            let reader = thread::spawn(move || {
                for sequence in 0..CAPACITY {
                    if let Some(packet) = reader_ring.read_sequence(sequence) {
                        assert_eq!(packet, sequence, "sequence {sequence}");
                    }
                }
            });

            ring.push_batch(CAPACITY);

            reader.join().unwrap();
            let last = 2 * CAPACITY - 1;
            assert_eq!(ring.read_sequence(last), Some(last));
            assert_eq!(ring.read_sequence(0), None, "lapped");
        });
    }

    /// A run read untagged while a batch laps it is either rejected or holds
    /// exactly its own sequences: any packet loaded from the lap comes with a
    /// `write_begin` the run check sees.
    #[test]
    fn loom_run_check_never_accepts_a_lapped_packet() {
        loom::model(|| {
            let ring = FakeRing::new();
            ring.push_batch(CAPACITY);
            let reader_ring = ring.clone();

            let reader = thread::spawn(move || {
                if let Some(packets) = reader_ring.read_run(0, CAPACITY) {
                    let expected: Vec<usize> = (0..packets.len()).collect();
                    assert_eq!(packets, expected);
                }
            });

            ring.push_batch(CAPACITY);

            reader.join().unwrap();
        });
    }

    /// With no publication racing it, a retained run passes the single check.
    #[test]
    fn loom_run_check_accepts_a_quiet_run_without_tags() {
        loom::model(|| {
            let ring = FakeRing::new();
            ring.push_batch(CAPACITY);
            assert_eq!(ring.read_run(0, CAPACITY), Some(vec![0, 1]));
        });
    }
}
