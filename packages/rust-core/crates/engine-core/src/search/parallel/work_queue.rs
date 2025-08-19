//! Work queue implementation for parallel search

use crate::shogi::{Move, Position};
use crossbeam_deque::{Injector, Stealer, Worker as DequeWorker};
use smallvec::SmallVec;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

/// Work item for threads to process
#[derive(Clone, Debug)]
pub enum WorkItem {
    /// Batch of root moves to search (8-16 moves)
    RootBatch {
        /// Iteration number
        iteration: usize,
        /// Depth to search at
        depth: u8,
        /// Position to search from (shared via Arc to avoid cloning)
        position: Arc<Position>,
        /// Batch of moves to search
        moves: SmallVec<[Move; 16]>,
        /// Starting index of the batch (for debugging)
        start_index: usize,
    },
    /// Search full position (traditional mode)
    FullPosition {
        /// Iteration number
        iteration: usize,
        /// Depth to search at
        depth: u8,
        /// Position to search (shared via Arc to avoid cloning)
        position: Arc<Position>,
    },
}

/// Truly lock-free work queue structure
#[derive(Clone)]
pub struct Queues {
    /// Global injector (Arc for sharing)
    pub injector: Arc<Injector<WorkItem>>,
    /// Stealers for work stealing (immutable after creation)
    pub stealers: Arc<[Stealer<WorkItem>]>,
}

/// Get work with 3-layer priority (truly lock-free with exponential backoff)
pub fn get_job(
    my_worker: &DequeWorker<WorkItem>,
    queues: &Queues,
    my_stealer_index: usize,
    steal_success: &AtomicU64,
    steal_failure: &AtomicU64,
) -> Option<WorkItem> {
    // Thread-local steal failure counter for exponential backoff
    thread_local! {
        static CONSECUTIVE_STEAL_FAILS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    // 1. Try local worker queue first (LIFO for DFS)
    if let Some(item) = my_worker.pop() {
        // Reset failure counter on success
        CONSECUTIVE_STEAL_FAILS.with(|f| f.set(0));
        return Some(item);
    }

    // 2. Then try stealing from other workers (random selection for better scalability)
    // Only steal from other workers, not from self
    let num_stealers = queues.stealers.len();
    if num_stealers > 1 {
        // Need at least 2 workers for stealing
        use rand::Rng;
        let mut rng = rand::rng();

        // Try a few random steals (min of 3 or half the workers)
        let steal_attempts = 3.min(num_stealers - 1); // Exclude self
        for _ in 0..steal_attempts {
            // Generate random index excluding self
            let mut idx = rng.random_range(0..num_stealers - 1);
            if idx >= my_stealer_index {
                idx += 1; // Skip self index
            }

            // Try to steal from selected worker
            match queues.stealers[idx].steal() {
                crossbeam_deque::Steal::Success(item) => {
                    // Reset failure counter on success
                    CONSECUTIVE_STEAL_FAILS.with(|f| f.set(0));
                    steal_success.fetch_add(1, Ordering::Relaxed);
                    return Some(item);
                }
                crossbeam_deque::Steal::Empty => continue,
                crossbeam_deque::Steal::Retry => {
                    // Try once more on retry
                    if let crossbeam_deque::Steal::Success(item) = queues.stealers[idx].steal() {
                        CONSECUTIVE_STEAL_FAILS.with(|f| f.set(0));
                        steal_success.fetch_add(1, Ordering::Relaxed);
                        return Some(item);
                    }
                }
            }
        }

        steal_failure.fetch_add(1, Ordering::Relaxed);
    }

    // 3. Finally try the global injector with batch stealing
    loop {
        match queues.injector.steal_batch_and_pop(my_worker) {
            crossbeam_deque::Steal::Success(item) => {
                // Reset failure counter on success
                CONSECUTIVE_STEAL_FAILS.with(|f| f.set(0));
                steal_success.fetch_add(1, Ordering::Relaxed);
                return Some(item);
            }
            crossbeam_deque::Steal::Empty => break,
            crossbeam_deque::Steal::Retry => continue,
        }
    }

    // Record failure only if no successful steal from workers
    // (already recorded above if worker steal failed)

    // Apply exponential backoff based on consecutive failures
    CONSECUTIVE_STEAL_FAILS.with(|f| {
        let fails = f.get() + 1;
        f.set(fails);

        // Exponential backoff strategy
        match fails {
            1..=5 => {
                // Spin for first few attempts (hot loop)
            }
            6..=15 => {
                // Yield to OS scheduler for medium failures
                std::thread::yield_now();
            }
            16..=25 => {
                // Brief sleep for many failures
                std::thread::sleep(std::time::Duration::from_micros(10));
            }
            _ => {
                // Longer sleep for persistent failures
                std::thread::sleep(std::time::Duration::from_micros(100));
            }
        }
    });

    None
}
