use proptest::prelude::*;
use restream_dataplane::{
    CapacityModel, CapacityRates, MediaArena, MediaRing, OpKind, OpTag, ReadyQueue, TxPool,
    Workload, jain_fairness_milli,
};

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

    #[test]
    fn tx_pool_rejects_stale_leases_after_reuse(cycles in 1usize..128) {
        let mut pool = TxPool::new(1, 64).unwrap();
        let stale = pool.acquire().unwrap();
        assert!(pool.submit(stale));
        assert!(pool.complete(stale));
        assert!(pool.release(stale));

        for _ in 0..cycles {
            let current = pool.acquire().unwrap();
            prop_assert_ne!(current.generation, stale.generation);
            prop_assert!(!pool.submit(stale));
            prop_assert!(!pool.complete(stale));
            prop_assert!(pool.submit(current));
            prop_assert!(pool.complete(current));
            prop_assert!(pool.release(current));
        }
    }

    #[test]
    fn capacity_projection_is_monotonic_in_output_count(
        small in 1u32..32,
        extra in 1u32..32,
    ) {
        let model = CapacityModel::new(CapacityRates {
            ingress_pps: 10_000.0,
            media_bps: 1_000_000_000.0,
            egress_pps: 20_000.0,
            nic_bps: 10_000_000_000.0,
            memory_bytes: 64.0 * 1024.0 * 1024.0,
            ffmpeg_stages: 1_000.0,
            disk_bps: 1_000_000_000.0,
            tx_bytes_per_output: 64.0 * 1024.0,
            shards: 2,
        }).unwrap();
        let workload = |outputs| Workload {
            outputs,
            media_bps: 1_000_000,
            media_pps: 100,
            rtmp_outputs: outputs,
            rtmps_outputs: 0,
            srt_outputs: 0,
            loss_rate: 0.0,
            stage_count: 0,
        };
        prop_assert!(
            model.hottest_utilization(workload(small + extra))
                >= model.hottest_utilization(workload(small))
        );
    }

    #[test]
    fn jain_fairness_stays_in_range(visits in prop::collection::vec(0u64..1_000_000, 0..128)) {
        prop_assert!(jain_fairness_milli(&visits) <= 1_000);
    }
}
