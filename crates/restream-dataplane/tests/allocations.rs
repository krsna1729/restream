use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use restream_dataplane::{FeedCursor, MediaArena, MediaRing, ReadyQueue, TxPool};

struct CountingAllocator;

thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|allocations| {
            if let Some(count) = allocations.get() {
                allocations.set(Some(count.saturating_add(1)));
            }
        });
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn count_allocations(f: impl FnOnce()) -> usize {
    ALLOCATIONS.with(|allocations| allocations.set(Some(0)));
    f();
    ALLOCATIONS.with(|allocations| allocations.replace(None).unwrap_or_default())
}

#[test]
fn dataplane_hot_paths_do_not_allocate() {
    let mut queue = ReadyQueue::new(64, 64).unwrap();
    let queue_allocations = count_allocations(|| {
        for _ in 0..10_000 {
            assert!(queue.enqueue(0));
            assert_eq!(queue.pop(), Some(0));
        }
    });
    assert_eq!(queue_allocations, 0);

    let mut ring = MediaRing::new(
        MediaArena::new(128, 256).unwrap(),
        64,
        16 * 1024,
        std::time::Duration::from_secs(60),
    )
    .unwrap();
    let payload = [7_u8; 128];
    let ring_allocations = count_allocations(|| {
        for sequence in 0..10_000_u64 {
            let reference = ring.arena_mut().acquire_copy(&payload).unwrap();
            ring.push(reference, std::time::Instant::now(), sequence % 30 == 0)
                .unwrap();
            let cursor = FeedCursor::new(ring.epoch(), ring.next_sequence().saturating_sub(1));
            let current = ring.read_cursor(cursor).unwrap();
            assert!(ring.retain(current));
            assert!(ring.release(current));
        }
    });
    assert_eq!(ring_allocations, 0);

    let mut tx = TxPool::new(8, 256).unwrap();
    let tx_allocations = count_allocations(|| {
        for _ in 0..10_000 {
            let lease = tx.acquire().unwrap();
            tx.slot_mut(lease).unwrap()[0] = 1;
            assert!(tx.submit(lease));
            assert!(tx.complete(lease));
            assert!(tx.release(lease));
        }
    });
    assert_eq!(tx_allocations, 0);
}
