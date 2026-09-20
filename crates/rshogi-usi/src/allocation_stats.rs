//! System allocator wrapper used only by allocation-stats diagnostic builds.
use rshogi_core::allocation_stats::{Operation, record};
use std::alloc::{GlobalAlloc, Layout, System};

struct CountingAllocator;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

// SAFETY: all pointer ownership, size and alignment contracts are forwarded
// unchanged to System. Recording uses only non-dropping TLS and atomic counters.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller provides the valid layout required by GlobalAlloc.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record(Operation::Alloc, layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller provides the valid layout required by GlobalAlloc.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record(Operation::AllocZeroed, layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: ptr belongs to System with this layout; the caller guarantees
        // that new_size is nonzero and satisfies GlobalAlloc::realloc's bounds.
        let result = unsafe { System.realloc(ptr, layout, new_size) };
        if !result.is_null() {
            record(Operation::Realloc, new_size);
        }
        result
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller guarantees ptr is live and was allocated with layout.
        unsafe { System.dealloc(ptr, layout) };
        record(Operation::Dealloc, 0);
    }
}

pub fn report(before: [[u64; 5]; 5]) {
    let after = rshogi_core::allocation_stats::snapshot();
    for (index, phase) in rshogi_core::allocation_stats::PHASES.iter().enumerate() {
        let delta: [u64; 5] =
            std::array::from_fn(|c| after[index][c].wrapping_sub(before[index][c]));
        println!(
            "info string allocations phase={phase:?} alloc={} zeroed={} realloc={} dealloc={} requested_bytes={}",
            delta[0], delta[1], delta[2], delta[3], delta[4]
        );
    }
}
