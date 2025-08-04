//! Prefetch budget management to prevent cache pollution
//!
//! Controls the amount of prefetching to avoid exceeding L2 cache capacity

use std::sync::atomic::{AtomicU32, Ordering};

/// Manages prefetch budget to prevent cache pollution
///
/// Ensures that prefetch operations don't exceed L2 cache capacity,
/// preventing performance degradation from excessive prefetching
#[repr(align(64))] // Cache line alignment
pub struct PrefetchBudget {
    /// Remaining budget in this frame (in bytes)
    remaining: AtomicU32,
    /// Maximum budget per frame (in bytes)
    max_per_frame: u32,
    /// L2 cache capacity in bytes
    l2_capacity_bytes: usize,
    /// Number of threads (for budget distribution)
    thread_count: usize,
}

impl PrefetchBudget {
    /// Create a new prefetch budget manager
    pub fn new() -> Self {
        // Conservative L2 cache size estimate (256KB)
        // In production, this should be detected at runtime
        const L2_SIZE_KB: usize = 256;
        const THREAD_COUNT: usize = 8;

        let l2_capacity = L2_SIZE_KB * 1024;

        // Allocate 50% of L2 cache for prefetching (conservative)
        // Divide by thread count for per-thread budget
        let max_per_frame = (l2_capacity / 2 / THREAD_COUNT) as u32;

        Self {
            remaining: AtomicU32::new(max_per_frame),
            max_per_frame,
            l2_capacity_bytes: l2_capacity,
            thread_count: THREAD_COUNT,
        }
    }

    /// Create with custom parameters (for testing)
    pub fn with_params(l2_size_kb: usize, thread_count: usize) -> Self {
        let l2_capacity = l2_size_kb * 1024;
        let max_per_frame = (l2_capacity / 2 / thread_count) as u32;

        Self {
            remaining: AtomicU32::new(max_per_frame),
            max_per_frame,
            l2_capacity_bytes: l2_capacity,
            thread_count,
        }
    }

    /// Try to consume budget for a prefetch operation
    /// Returns true if budget was available and consumed
    #[inline]
    pub fn try_consume(&self, bytes: u32) -> bool {
        // Fast path: check if we have enough budget
        let current = self.remaining.load(Ordering::Relaxed);
        if current < bytes {
            return false;
        }

        // Try to atomically subtract the bytes
        // Use compare-exchange to handle concurrent access
        loop {
            let current = self.remaining.load(Ordering::Relaxed);
            if current < bytes {
                return false;
            }

            match self.remaining.compare_exchange_weak(
                current,
                current - bytes,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(_) => continue, // Retry on concurrent modification
            }
        }
    }

    /// Reset budget for new frame/iteration
    #[inline]
    pub fn reset_frame(&self) {
        self.remaining.store(self.max_per_frame, Ordering::Relaxed);
    }

    /// Get remaining budget
    pub fn remaining(&self) -> u32 {
        self.remaining.load(Ordering::Relaxed)
    }

    /// Get maximum budget per frame
    pub fn max_budget(&self) -> u32 {
        self.max_per_frame
    }

    /// Calculate dynamic budget based on search phase
    /// Early search: more aggressive (use full budget)
    /// Late search: conservative (reduce budget)
    pub fn adjust_for_phase(&mut self, depth: u8, max_depth: u8) {
        let phase_ratio = depth as f32 / max_depth.max(1) as f32;

        // Reduce budget as we go deeper
        // At depth 1: 100% budget
        // At max depth: 25% budget
        let adjusted_budget = if phase_ratio < 0.5 {
            self.max_per_frame // Early phase: full budget
        } else {
            // Late phase: reduce budget progressively
            let reduction = (phase_ratio - 0.5) * 1.5; // 0.5 -> 0.75 reduction
            (self.max_per_frame as f32 * (1.0 - reduction)).max(self.max_per_frame as f32 * 0.25)
                as u32
        };

        self.remaining.store(adjusted_budget, Ordering::Relaxed);
    }
}

/// Budget-aware prefetch statistics
#[derive(Debug, Default)]
pub struct BudgetStats {
    /// Number of prefetches allowed
    pub allowed: u64,
    /// Number of prefetches denied due to budget
    pub denied: u64,
    /// Total bytes consumed
    pub bytes_consumed: u64,
    /// Number of frame resets
    pub frame_resets: u64,
}

impl BudgetStats {
    /// Calculate budget efficiency
    pub fn efficiency(&self) -> f32 {
        if self.allowed + self.denied == 0 {
            0.0
        } else {
            self.allowed as f32 / (self.allowed + self.denied) as f32 * 100.0
        }
    }

    /// Average bytes per prefetch
    pub fn avg_bytes_per_prefetch(&self) -> f32 {
        if self.allowed == 0 {
            0.0
        } else {
            self.bytes_consumed as f32 / self.allowed as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_budget_consumption() {
        let budget = PrefetchBudget::with_params(256, 1); // 256KB L2, 1 thread

        // Should have 128KB budget (50% of L2)
        assert_eq!(budget.max_budget(), 131072);

        // Consume some budget
        assert!(budget.try_consume(64)); // 64 bytes for one TT bucket
        assert_eq!(budget.remaining(), 131072 - 64);

        // Try to consume more than available
        assert!(!budget.try_consume(200000));

        // Reset and verify
        budget.reset_frame();
        assert_eq!(budget.remaining(), 131072);
    }

    #[test]
    fn test_phase_adjustment() {
        let mut budget = PrefetchBudget::with_params(256, 1);
        let max_budget = budget.max_budget();

        // Early phase - full budget
        budget.adjust_for_phase(1, 10);
        assert_eq!(budget.remaining(), max_budget);

        // Mid phase - still full budget
        budget.adjust_for_phase(5, 10);
        assert_eq!(budget.remaining(), max_budget);

        // Late phase - reduced budget
        budget.adjust_for_phase(8, 10);
        assert!(budget.remaining() < max_budget);
        assert!(budget.remaining() >= max_budget / 4); // At least 25%
    }

    #[test]
    fn test_concurrent_consumption() {
        use std::sync::Arc;
        use std::thread;

        let budget = Arc::new(PrefetchBudget::with_params(256, 4));
        let mut handles = vec![];

        // Spawn 4 threads trying to consume budget
        for _ in 0..4 {
            let budget_clone = budget.clone();
            handles.push(thread::spawn(move || {
                let mut consumed = 0;
                while budget_clone.try_consume(64) {
                    consumed += 64;
                }
                consumed
            }));
        }

        // Collect results
        let total_consumed: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();

        // Should consume exactly the budget amount
        assert_eq!(total_consumed, budget.max_budget());
    }
}
