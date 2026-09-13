use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use restream_dataplane::ReadyQueue;

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn ready_queue_hot_path_does_not_allocate() {
    let mut queue = ReadyQueue::new(64, 64).unwrap();
    ALLOCATIONS.store(0, Ordering::Relaxed);

    for _ in 0..10_000 {
        assert!(queue.enqueue(0));
        assert_eq!(queue.pop(), Some(0));
    }

    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
