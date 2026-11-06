//! System-allocator forwarding counters, compiled only into separate diagnostic builds.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
struct Meter;
static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
// SAFETY: every allocation and deallocation is forwarded with its original layout
// to System; counters neither allocate nor alter pointers. Realloc records requested
// bytes (allocation traffic), not live heap or SQLite's C allocator.
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size() as u64, Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size() as u64, Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(size as u64, Relaxed);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Meter = Meter;
pub(crate) fn snapshot() -> (u64, u64) {
    (CALLS.load(Relaxed), BYTES.load(Relaxed))
}
