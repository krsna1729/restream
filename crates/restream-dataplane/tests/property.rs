use proptest::prelude::*;
use restream_dataplane::{OpKind, OpTag, ReadyQueue};

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
}
