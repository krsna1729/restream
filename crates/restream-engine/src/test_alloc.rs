//! A counting global allocator for allocation-budget tests: `begin` starts
//! counting this thread's allocations, `end` stops and returns the count.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

// SAFETY: every method forwards unchanged to `System`, which upholds the GlobalAlloc
// contract; the counting touches only const-initialised thread-locals and never allocates.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ACTIVE.with(|active| {
            if active.get() {
                COUNT.with(|count| count.set(count.get().saturating_add(1)));
            }
        });
        // SAFETY: the caller's `layout` contract passes through to `System` unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: `pointer` came from `alloc` above, i.e. from `System`, with this
        // `layout`.
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

pub fn begin() {
    COUNT.with(|count| count.set(0));
    ACTIVE.with(|active| active.set(true));
}

pub fn end() -> usize {
    ACTIVE.with(|active| active.set(false));
    COUNT.with(Cell::get)
}
