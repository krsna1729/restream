#![no_main]

use libfuzzer_sys::fuzz_target;
use restream_dataplane::{MediaArena, MediaRing};

fuzz_target!(|operations: Vec<u8>| {
    let mut ring = MediaRing::new(
        MediaArena::new(32, 64).expect("valid arena"),
        16,
        512,
        std::time::Duration::from_secs(1),
    )
    .expect("valid ring");

    for (index, operation) in operations.into_iter().enumerate() {
        match operation & 3 {
            0 | 1 => {
                let len = usize::from(operation & 63) + 1;
                if let Some(reference) = ring.arena_mut().acquire(len) {
                    let payload = vec![operation; len];
                    if !ring.arena_mut().write(reference, &payload)
                        || ring
                            .push(
                                reference,
                                std::time::Instant::now(),
                                operation & 0x80 != 0,
                            )
                            .is_err()
                    {
                        let _ = ring.release(reference);
                    }
                }
            }
            2 => {
                let sequence = (index as u64).saturating_sub(u64::from(operation & 15));
                if let Ok(reference) = ring.read(sequence) {
                    assert!(ring.retain(reference));
                    assert!(ring.release(reference));
                }
            }
            _ => {
                let _ = ring.evict_expired(std::time::Instant::now());
            }
        }

        assert!(ring.len() <= ring.capacity());
        assert!(ring.bytes() <= 512);
    }
});
