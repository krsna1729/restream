#![no_main]

use libfuzzer_sys::fuzz_target;
use restream_dataplane::ReadyQueue;

fuzz_target!(|operations: Vec<u8>| {
    let mut queue = ReadyQueue::new(64, 64).expect("valid fixed queue");
    for operation in operations {
        let slot = u32::from(operation & 63);
        match operation & 0xc0 {
            0x00 => {
                let _ = queue.enqueue(slot);
            }
            0x40 => {
                let _ = queue.remove(slot);
            }
            _ => {
                let _ = queue.pop();
            }
        }
    }
    let mut seen = [false; 64];
    while let Some(slot) = queue.pop() {
        assert!(!seen[slot as usize]);
        seen[slot as usize] = true;
    }
});
