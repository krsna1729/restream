#![no_main]

use libfuzzer_sys::fuzz_target;
use restream_dataplane::TxPool;

fuzz_target!(|operations: Vec<u8>| {
    let mut pool = TxPool::new(8, 256).expect("valid fixed TX pool");
    let mut leases = Vec::with_capacity(8);
    for operation in operations {
        let slot = usize::from(operation & 7);
        match operation % 6 {
            0 => {
                if let Some(lease) = pool.acquire() {
                    leases.push(lease);
                }
            }
            1 => {
                if let Some(lease) = leases.get(slot % leases.len().max(1)).copied() {
                    let _ = pool.slot_mut(lease);
                    let _ = pool.submit(lease);
                }
            }
            2 => {
                if let Some(lease) = leases.get(slot % leases.len().max(1)).copied() {
                    let _ = pool.await_zc_notification(lease);
                }
            }
            3 => {
                if let Some(lease) = leases.get(slot % leases.len().max(1)).copied() {
                    let _ = pool.complete(lease);
                }
            }
            4 => {
                if let Some(lease) = leases.get(slot % leases.len().max(1)).copied() {
                    let _ = pool.release(lease);
                }
            }
            _ => {
                leases.retain(|lease| pool.state(*lease).is_some());
            }
        }
        assert!(pool.available() <= 8);
    }
});
