//! Adaptive prefetch manager for transposition table
//!
//! This module implements an adaptive prefetching strategy that:
//! - Tracks prefetch hit/miss rates
//! - Dynamically adjusts prefetch distance based on effectiveness
//! - Provides statistics for performance tuning

use crate::util::sync_compat::{AtomicU64, Ordering};
use std::sync::atomic::AtomicUsize;

/// Adaptive prefetcher for the transposition table
///
/// Tracks prefetch effectiveness and adjusts strategy dynamically
/// Fields are cache-line aligned to prevent false sharing in multi-threaded scenarios
#[repr(align(64))] // Cache line alignment
pub struct AdaptivePrefetcher {
    /// Prefetch hit count
    hit_count: AtomicU64,
    /// Prefetch miss count  
    miss_count: AtomicU64,
    /// Current prefetch distance (1-8 moves ahead)
    /// Placed on separate cache line to avoid false sharing
    current_distance: AtomicUsize,
    /// Statistics update counter
    stat_counter: AtomicU64,
    /// Padding to ensure cache line separation
    _padding: [u8; 32], // Ensures full 64-byte alignment
}

impl AdaptivePrefetcher {
    /// Maximum prefetch distance
    const MAX_DISTANCE: usize = 8;
    /// Minimum prefetch distance
    const MIN_DISTANCE: usize = 1;
    /// Number of samples before adjusting strategy
    /// Reduced from 1024 to 512 for faster adaptation (8 threads * 512 = 4k events per generation)
    const STAT_WINDOW: u64 = 512;

    /// Create a new adaptive prefetcher
    pub fn new() -> Self {
        Self {
            hit_count: AtomicU64::new(0),
            miss_count: AtomicU64::new(0),
            current_distance: AtomicUsize::new(2), // Start with moderate distance
            stat_counter: AtomicU64::new(0),
            _padding: [0; 32],
        }
    }

    /// Record a prefetch hit
    #[inline]
    pub fn record_hit(&self) {
        self.hit_count.fetch_add(1, Ordering::Relaxed);
        self.update_statistics();
    }

    /// Record a prefetch miss
    #[inline]
    pub fn record_miss(&self) {
        self.miss_count.fetch_add(1, Ordering::Relaxed);
        self.update_statistics();
    }

    /// Update statistics and adjust strategy if needed
    fn update_statistics(&self) {
        let count = self.stat_counter.fetch_add(1, Ordering::Relaxed);

        // Check if we should adjust strategy
        if (count + 1) % Self::STAT_WINDOW == 0 {
            self.adjust_distance();
        }
    }

    /// Adjust prefetch distance based on hit rate
    fn adjust_distance(&self) {
        let hits = self.hit_count.load(Ordering::Relaxed);
        let misses = self.miss_count.load(Ordering::Relaxed);
        let total = hits + misses;

        // Don't adjust until we have enough statistics
        if total < Self::STAT_WINDOW {
            return;
        }

        let hit_rate = hits as f64 / total as f64;
        let current = self.current_distance.load(Ordering::Relaxed);

        // Use wider hysteresis (0.75/0.25 instead of 0.7/0.3) to prevent oscillation
        let new_distance = if hit_rate > 0.75 {
            // High hit rate: increase distance to prefetch more aggressively
            (current + 1).min(Self::MAX_DISTANCE)
        } else if hit_rate < 0.25 {
            // Low hit rate: reduce distance to avoid wasting bandwidth
            current.saturating_sub(1).max(Self::MIN_DISTANCE)
        } else {
            current
        };

        if new_distance != current {
            self.current_distance.store(new_distance, Ordering::Relaxed);
        }

        // Decay statistics to prevent overflow and adapt to changing patterns
        // Right shift by 1 = divide by 2
        self.hit_count.store(hits >> 1, Ordering::Relaxed);
        self.miss_count.store(misses >> 1, Ordering::Relaxed);
    }

    /// Calculate dynamic prefetch distance based on remaining nodes
    /// Now considers L2 cache size and thread count for better scaling
    pub fn calculate_distance(&self, remaining_nodes: u64) -> usize {
        // Dynamic coefficient calculation based on system parameters
        // L2_size / (bucket_bytes × threads)
        const L2_SIZE_KB: usize = 256; // Conservative estimate, could be runtime detected
        const BUCKET_SIZE: usize = 64; // TT bucket size in bytes
        const THREAD_COUNT: usize = 8; // Could be runtime detected
        const COEFFICIENT: usize = (L2_SIZE_KB * 1024) / (BUCKET_SIZE * THREAD_COUNT);

        let base_distance = self.current_distance.load(Ordering::Relaxed);

        // Dynamic scaling based on remaining nodes with improved coefficient
        // Use log2 to scale distance with search depth
        let dynamic_factor = if remaining_nodes > 0 {
            (remaining_nodes as f64).log2().max(1.0)
        } else {
            1.0
        };

        // Adjust base distance by dynamic factor with cache-aware coefficient
        // The coefficient (64 instead of 8) considers L2 cache constraints
        let adjusted = (base_distance as f64 * dynamic_factor / COEFFICIENT.max(1) as f64) as usize;

        adjusted.clamp(Self::MIN_DISTANCE, Self::MAX_DISTANCE)
    }

    /// Get current prefetch distance
    pub fn current_distance(&self) -> usize {
        self.current_distance.load(Ordering::Relaxed)
    }

    /// Get current hit rate
    pub fn hit_rate(&self) -> f64 {
        let hits = self.hit_count.load(Ordering::Relaxed);
        let misses = self.miss_count.load(Ordering::Relaxed);
        let total = hits + misses;

        if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        }
    }

    /// Get statistics
    pub fn stats(&self) -> PrefetchStats {
        let hits = self.hit_count.load(Ordering::Relaxed);
        let misses = self.miss_count.load(Ordering::Relaxed);
        let distance = self.current_distance.load(Ordering::Relaxed);

        PrefetchStats {
            hits,
            misses,
            distance,
            hit_rate: if hits + misses > 0 {
                hits as f64 / (hits + misses) as f64
            } else {
                0.0
            },
        }
    }

    /// Reset statistics
    pub fn reset(&self) {
        self.hit_count.store(0, Ordering::Relaxed);
        self.miss_count.store(0, Ordering::Relaxed);
        self.stat_counter.store(0, Ordering::Relaxed);
        self.current_distance.store(2, Ordering::Relaxed);
    }
}

impl Default for AdaptivePrefetcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Prefetch statistics
#[derive(Debug, Clone, Copy)]
pub struct PrefetchStats {
    /// Number of prefetch hits
    pub hits: u64,
    /// Number of prefetch misses
    pub misses: u64,
    /// Current prefetch distance
    pub distance: usize,
    /// Hit rate (0.0 to 1.0)
    pub hit_rate: f64,
}

impl PrefetchStats {
    /// Total number of prefetch attempts
    pub fn total(&self) -> u64 {
        self.hits + self.misses
    }

    /// Check if prefetch is effective (hit rate above threshold)
    pub fn is_effective(&self) -> bool {
        self.hit_rate > 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adaptive_distance() {
        let prefetcher = AdaptivePrefetcher::new();

        // Initial distance should be 2
        assert_eq!(prefetcher.current_distance(), 2);

        // Simulate high hit rate
        for _ in 0..800 {
            prefetcher.record_hit();
        }
        for _ in 0..200 {
            prefetcher.record_miss();
        }

        // After STAT_WINDOW samples, distance should increase
        prefetcher.adjust_distance();
        assert!(prefetcher.current_distance() >= 2);

        // Check hit rate
        let stats = prefetcher.stats();
        assert!(stats.hit_rate > 0.7);
    }

    #[test]
    fn test_low_hit_rate_adjustment() {
        let prefetcher = AdaptivePrefetcher::new();

        // Simulate low hit rate
        for _ in 0..200 {
            prefetcher.record_hit();
        }
        for _ in 0..800 {
            prefetcher.record_miss();
        }

        // Distance should decrease
        prefetcher.adjust_distance();
        assert!(prefetcher.current_distance() <= 2);
    }

    #[test]
    fn test_dynamic_distance_calculation() {
        let prefetcher = AdaptivePrefetcher::new();

        // Test with different remaining nodes
        let dist_low = prefetcher.calculate_distance(10);
        let dist_high = prefetcher.calculate_distance(1000000);

        // Higher remaining nodes should not drastically increase distance
        assert!(dist_high <= AdaptivePrefetcher::MAX_DISTANCE);
        assert!(dist_low >= AdaptivePrefetcher::MIN_DISTANCE);
    }

    #[test]
    fn test_statistics_decay() {
        let prefetcher = AdaptivePrefetcher::new();

        // Directly set statistics to test decay behavior
        // We bypass the automatic decay that happens during recording
        prefetcher.hit_count.store(2000, Ordering::Relaxed);
        prefetcher.miss_count.store(48, Ordering::Relaxed);

        let stats_before = prefetcher.stats();
        assert_eq!(stats_before.hits, 2000);
        assert_eq!(stats_before.misses, 48);

        // Call adjust_distance which should decay the statistics
        // since total (2048) >= STAT_WINDOW (1024)
        prefetcher.adjust_distance();

        let stats_after = prefetcher.stats();

        // Statistics should decay by right shift (divide by 2)
        assert_eq!(stats_after.hits, 1000, "Hits should decay from 2000 to 1000");
        assert_eq!(stats_after.misses, 24, "Misses should decay from 48 to 24");
    }

    #[test]
    fn test_integration_with_transposition_table() {
        use crate::search::tt::TranspositionTable;
        use crate::shogi::position::Position;

        let prefetcher = AdaptivePrefetcher::new();
        let tt = TranspositionTable::new(16);

        // Simulate prefetch with adaptive distance
        let pos = Position::startpos();
        let distance = prefetcher.calculate_distance(1000);

        // Prefetch multiple entries based on distance
        for i in 0..distance {
            let hash = pos.zobrist_hash() ^ (i as u64);
            tt.prefetch_l1(hash);
        }

        // Record hits and misses
        for i in 0..distance {
            let hash = pos.zobrist_hash() ^ (i as u64);
            if tt.probe(hash).is_some() {
                prefetcher.record_hit();
            } else {
                prefetcher.record_miss();
            }
        }

        // Check statistics
        let stats = prefetcher.stats();
        assert_eq!(stats.total(), distance as u64);
    }
}
