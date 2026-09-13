use proptest::prelude::*;
use restream_dataplane::{MediaArena, MediaRing, OpKind, OpTag, ReadyQueue};

proptest! {
    #[test]
    fn operation_tag_round_trip(slot in 0u32..(1 << 20), generation in any::<u32>()) {
        let tag = OpTag::new(OpKind::TcpTx, slot, generation).unwrap();
        prop_assert_eq!(OpTag::decode(tag.encode()), Some(tag));
    }

    #[test]
    fn ready_queue_never_duplicates(slots in prop::collection::vec(0u8..8, 0..128)) {
        let mut queue = ReadyQueue::new(8, 8).unwrap();
        for slot in slots {
            let _ = queue.enqueue(u32::from(slot));
        }
        let mut seen = [false; 8];
        while let Some(slot) = queue.pop() {
            prop_assert!(!seen[slot as usize]);
            seen[slot as usize] = true;
        }
    }

    #[test]
    fn media_ring_never_exceeds_bounds(lengths in prop::collection::vec(1usize..8, 1..32)) {
        let mut ring = MediaRing::new(
            MediaArena::new(32, 8).unwrap(),
            8,
            24,
            std::time::Duration::from_secs(60),
        ).unwrap();
        for length in lengths {
            if let Some(reference) = ring.arena_mut().acquire(length) {
                let _ = ring.push(reference, std::time::Instant::now(), length == 1);
            }
            prop_assert!(ring.len() <= ring.capacity());
            prop_assert!(ring.bytes() <= 24);
        }
    }
}
