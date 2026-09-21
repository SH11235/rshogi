//! allocation-stats 診断用。外部 allocator の安全な callback API で分類する。
use rshogi_core::allocation_stats::{Operation, record};
use std::alloc::System;
use tracking_allocator::{AllocationGroupId, AllocationRegistry, AllocationTracker, Allocator};

#[global_allocator]
static ALLOCATOR: Allocator<System> = Allocator::system();

struct Tracker;

impl AllocationTracker for Tracker {
    fn allocated(&self, _: usize, object_size: usize, _: usize, _: AllocationGroupId) {
        record(Operation::Alloc, object_size);
    }

    fn deallocated(
        &self,
        _: usize,
        _: usize,
        _: usize,
        _: AllocationGroupId,
        _: AllocationGroupId,
    ) {
        record(Operation::Dealloc, 0);
    }
}

pub fn init() -> Result<(), tracking_allocator::SetTrackerError> {
    AllocationRegistry::set_global_tracker(Tracker)?;
    AllocationRegistry::enable_tracking();
    Ok(())
}

pub fn report(before: [[u64; 3]; 5]) {
    let after = rshogi_core::allocation_stats::snapshot();
    for (index, phase) in rshogi_core::allocation_stats::PHASES.iter().enumerate() {
        let delta: [u64; 3] =
            std::array::from_fn(|c| after[index][c].wrapping_sub(before[index][c]));
        println!(
            "info string allocation_events phase={phase:?} allocated={} deallocated={} object_bytes={}",
            delta[0], delta[1], delta[2]
        );
    }
}
